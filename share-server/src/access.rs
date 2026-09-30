//! Cloudflare Access JWT verification, performed at the origin.
//!
//! Verifying here rather than trusting a header means a request that reaches
//! the service by some other route than the proxy still cannot forge an
//! identity.
//!
//! The principal id must match the digest `deploy/cloudflare/src/support.js`
//! computes: both hosts can front one bucket, so a disagreement would make
//! ownership of a transcript depend on which host published it.
//!
//! [`Identity`] is synchronous, so fetching a key set inside `principal()`
//! would stall whichever thread is serving the request. A refresher thread
//! owns the fetching; the request path reads what it published.

use std::sync::{Arc, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::GeneralPurpose;
use ring::digest;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use txcript_share_core::identity::{Headers, Identity, IdentityError};
use txcript_share_core::{Principal, PrincipalId, PrincipalKind};

/// The header Cloudflare Access sets on every request it admits.
///
/// Not configurable: it is part of the Access protocol, not a deployment
/// choice, and a verifier that reads some other header is verifying the
/// wrong thing.
pub const HEADER: &str = "cf-access-jwt-assertion";

/// How long the refresher waits between fetches.
const JWKS_TTL: Duration = Duration::from_hours(1);

/// The interval between attempts while fetching is failing. The unit starts
/// `After=network.target`, so the first attempt can land before there is a
/// route out, and until one succeeds every request is a 503.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// How long a key set stays usable once refreshing it starts failing, so a
/// JWKS outage does not become an outage of ours. The bound is what stops
/// "indefinitely".
const STALE_GRACE: Duration = Duration::from_hours(24);

/// A key set is a few kilobytes. Anything larger is not one.
const MAX_JWKS_BYTES: u64 = 64 * 1024;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Unpadded base64url, but tolerant of padding: JWTs omit it, and a key set
/// that includes it is still a key set.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

/// One RSA public key from a JWKS document.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonWebKey {
    #[serde(default)]
    pub kid: String,
    #[serde(default)]
    pub kty: String,
    #[serde(default)]
    pub alg: Option<String>,
    /// Modulus, base64url.
    pub n: String,
    /// Exponent, base64url.
    pub e: String,
}

impl JsonWebKey {
    fn usable_for_rs256(&self) -> bool {
        self.kty == "RSA" && self.alg.as_deref().is_none_or(|alg| alg == "RS256")
    }
}

/// Where the team's signing keys come from.
///
/// A trait so the verifier can be tested without a network, and so a future
/// OIDC backend can reuse the refreshing rather than growing a second copy
/// of it. Blocking, because it is only ever called from the thread that
/// exists to wait for it.
pub trait Jwks: Send + Sync {
    /// # Errors
    /// When the key set could not be fetched or parsed.
    fn fetch(&self) -> Result<Vec<JsonWebKey>, String>;
}

/// The team's published key set, over HTTPS.
pub struct TeamJwks {
    url: String,
    agent: ureq::Agent,
}

impl TeamJwks {
    /// The `cdn-cgi` certs endpoint for a Zero Trust team.
    #[must_use]
    pub fn new(team: &str) -> Self {
        Self::at(format!(
            "https://{team}.cloudflareaccess.com/cdn-cgi/access/certs"
        ))
    }

    #[must_use]
    pub fn at(url: impl Into<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(FETCH_TIMEOUT))
            .build();
        Self {
            url: url.into(),
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Jwks for TeamJwks {
    fn fetch(&self) -> Result<Vec<JsonWebKey>, String> {
        #[derive(Deserialize)]
        struct Document {
            keys: Vec<JsonWebKey>,
        }

        let mut response = self
            .agent
            .get(&self.url)
            .call()
            .map_err(|error| format!("fetching {}: {error}", self.url))?;
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_JWKS_BYTES)
            .read_to_string()
            .map_err(|error| format!("reading {}: {error}", self.url))?;
        let document: Document = serde_json::from_str(&body)
            .map_err(|error| format!("parsing {}: {error}", self.url))?;
        Ok(document.keys)
    }
}

/// The keys the refresher has published, and how much they can be trusted.
#[derive(Default)]
struct KeySet {
    keys: Vec<JsonWebKey>,
    /// When these keys were fetched. `None` until a fetch has succeeded,
    /// which is the difference between "no key for that `kid`" and "no keys
    /// at all" — a 401 and a 503.
    fetched: Option<Instant>,
}

impl KeySet {
    fn key(&self, kid: &str) -> Option<&JsonWebKey> {
        self.keys
            .iter()
            .find(|key| key.kid == kid && key.usable_for_rs256())
    }

    fn usable_within(&self, age: Duration) -> bool {
        self.fetched.is_some_and(|at| at.elapsed() < age)
    }
}

/// The keys the request path reads and the refresher replaces.
struct Shared {
    keys: RwLock<KeySet>,
}

impl Shared {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, KeySet> {
        // A panic while holding the write lock would leave the keys intact —
        // publishing them is a move, not a mutation in steps — so recovering
        // is right. Failing every request forever afterwards is not.
        self.keys.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Returns whether the fetch succeeded, which is how the refresher
    /// knows which interval to wait next.
    fn publish(&self, fetched: Result<Vec<JsonWebKey>, String>) -> bool {
        let mut set = self.keys.write().unwrap_or_else(PoisonError::into_inner);
        match fetched {
            Ok(keys) => {
                set.keys = keys;
                set.fetched = Some(Instant::now());
                true
            }
            Err(detail) => {
                // Logged here because the request path cannot: it reports
                // "unavailable" without ever having seen the reason.
                eprintln!("Access key set: {detail}");
                false
            }
        }
    }
}

/// How often the refresher fetches, healthy and failing. Tests need both
/// shorter; a deployment needs neither.
#[derive(Debug, Clone, Copy)]
struct Rate {
    ttl: Duration,
    retry: Duration,
}

impl Default for Rate {
    fn default() -> Self {
        Self {
            ttl: JWKS_TTL,
            retry: RETRY_INTERVAL,
        }
    }
}

/// Verifies `Cf-Access-Jwt-Assertion` against a team's key set and an
/// application's AUD tag.
pub struct CloudflareAccess {
    issuer: String,
    audience: String,
    shared: Arc<Shared>,
    /// How old the newest successful fetch may be and still be treated as
    /// what the team currently publishes.
    current_for: Duration,
    /// How old it may be before its keys stop being accepted at all.
    usable_for: Duration,
}

impl CloudflareAccess {
    /// Verify against `<team>.cloudflareaccess.com`.
    ///
    /// Fetches the key set once, here, so a healthy service answers its
    /// first request rather than 503-ing while a refresher catches up. A
    /// failure is logged and retried in the background, because refusing to
    /// start over a transient fetch is the worse failure.
    #[must_use]
    pub fn new(team: &str, audience: &str) -> Self {
        Self::with_keys(team, audience, Box::new(TeamJwks::new(team)))
    }

    /// The same, against a key set from somewhere else.
    #[must_use]
    pub fn with_keys(team: &str, audience: &str, source: Box<dyn Jwks>) -> Self {
        Self::refreshing(team, audience, source, Rate::default())
    }

    fn refreshing(team: &str, audience: &str, source: Box<dyn Jwks>, rate: Rate) -> Self {
        let shared = Arc::new(Shared {
            keys: RwLock::new(KeySet::default()),
        });
        let healthy = shared.publish(source.fetch());
        refresh_in_the_background(Arc::downgrade(&shared), source, rate, healthy);
        Self {
            // Access mints tokens for one team, and the key set is that
            // team's. Checking the issuer too costs nothing and refuses a
            // token that was somehow signed by the right key for the wrong
            // tenant.
            issuer: format!("https://{team}.cloudflareaccess.com"),
            audience: audience.to_string(),
            shared,
            current_for: rate.ttl,
            usable_for: rate.ttl + STALE_GRACE,
        }
    }

    /// The verified claims, or `None` for any token this service will not
    /// accept.
    fn claims(&self, token: &str) -> Result<Option<Claims>, String> {
        let mut parts = token.split('.');
        let (Some(header64), Some(payload64), Some(signature64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Ok(None);
        };

        let Some(header) = decode_json::<JwtHeader>(header64) else {
            return Ok(None);
        };
        // The signature is checked as RSA-PKCS1-SHA256 whatever the token
        // says, so `alg` confusion is not reachable; refusing anything else
        // outright keeps it that way if this function ever grows.
        if header.alg != "RS256" {
            return Ok(None);
        }

        let Some(key) = self.key(&header.kid)? else {
            return Ok(None);
        };
        let (Some(n), Some(e), Some(signature)) = (
            BASE64URL.decode(&key.n).ok(),
            BASE64URL.decode(&key.e).ok(),
            BASE64URL.decode(signature64).ok(),
        ) else {
            return Ok(None);
        };
        let signed = format!("{header64}.{payload64}");
        let public = RsaPublicKeyComponents { n, e };
        if public
            .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
            .is_err()
        {
            return Ok(None);
        }

        let Some(claims) = decode_json::<Claims>(payload64) else {
            return Ok(None);
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "the system clock is before the unix epoch".to_string())?
            .as_secs();
        if claims.exp <= now || claims.iss != self.issuer || !claims.aud.includes(&self.audience) {
            return Ok(None);
        }
        Ok(Some(claims))
    }

    /// The key a token names, from whatever the refresher last published.
    ///
    /// Never fetches: a `kid` we do not hold is refused, and a rotation is
    /// picked up by the refresher within the TTL.
    fn key(&self, kid: &str) -> Result<Option<JsonWebKey>, String> {
        let set = self.shared.read();
        if set.usable_within(self.usable_for)
            && let Some(key) = set.key(kid)
        {
            return Ok(Some(key.clone()));
        }
        // Whether an unrecognised `kid` is the caller's problem or ours
        // turns on whether the key set is current, not on whether the last
        // refresh happened to fail: inside the TTL we hold what the team
        // publishes, so a `kid` missing from it is a 401 even during a blip.
        // Past the TTL a refresh was due and did not land.
        if set.usable_within(self.current_for) {
            Ok(None)
        } else {
            Err("the Access key set is unavailable".to_string())
        }
    }
}

/// Fetch on a thread of its own, on the TTL.
///
/// A thread rather than a task because the fetch is blocking, and this way
/// it cannot occupy a runtime worker whatever runtime the host is using —
/// or whether it has one at all.
fn refresh_in_the_background(
    shared: Weak<Shared>,
    source: Box<dyn Jwks>,
    rate: Rate,
    healthy: bool,
) {
    let refresher = move || {
        let mut healthy = healthy;
        loop {
            std::thread::sleep(if healthy { rate.ttl } else { rate.retry });
            // The verifier is gone, and with it the reason to refresh.
            let Some(shared) = shared.upgrade() else {
                return;
            };
            healthy = shared.publish(source.fetch());
        }
    };

    if let Err(error) = std::thread::Builder::new()
        .name("access-jwks".to_string())
        .spawn(refresher)
    {
        // Not fatal: the key set fetched at startup still verifies tokens
        // until it ages out. Loud, because a rotation will end that.
        eprintln!("Access key set: no refresher thread ({error}); keys will not be refreshed");
    }
}

impl Identity for CloudflareAccess {
    fn principal(&self, headers: &Headers) -> Result<Option<Principal>, IdentityError> {
        let Some(token) = headers.get(HEADER).filter(|token| !token.is_empty()) else {
            return Ok(None);
        };
        let Some(claims) = self.claims(token).map_err(IdentityError)? else {
            return Ok(None);
        };

        // An SSO login carries `email`; a service token carries
        // `common_name`. Reading only `email` is the quiet way to break
        // every machine client while the humans keep working.
        //
        // `email` present but empty falls through to neither, matching the
        // Worker's `claims.email ?? claims.common_name`: the two hosts
        // agreeing about who a token is includes agreeing that it is nobody.
        let (identity, kind) = match (claims.email, claims.common_name) {
            (Some(email), _) => (email, PrincipalKind::Human),
            (None, Some(name)) => (name, PrincipalKind::Service),
            (None, None) => return Ok(None),
        };
        if identity.is_empty() {
            return Ok(None);
        }

        let id = PrincipalId::new(sha256_hex(&identity)).ok_or_else(|| {
            IdentityError("a hashed Access identity is not a usable principal id".to_string())
        })?;
        Ok(Some(Principal::new(id, kind).with_label(identity)))
    }
}

#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    kid: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    exp: u64,
    #[serde(default)]
    iss: String,
    aud: Audience,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    common_name: Option<String>,
}

/// `aud` is one string or several; both are legal, and Access sends the
/// second form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn includes(&self, wanted: &str) -> bool {
        match self {
            Audience::One(only) => only == wanted,
            Audience::Many(all) => all.iter().any(|one| one == wanted),
        }
    }
}

/// The principal id, which the rest of the service only ever compares.
///
/// **It must be injective in the Access identity.** Ownership is a
/// comparison of these, so two identities sharing one could delete each
/// other's transcripts; an earlier prototype lowercased and replaced
/// punctuation and collapsed `a+b@x.com` onto `a_b@x.com`. SHA-256 of the
/// identity string, exactly as the Worker computes it — the two hosts must
/// agree.
fn sha256_hex(identity: &str) -> String {
    digest::digest(&digest::SHA256, identity.as_bytes())
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            out.push(DIGITS[usize::from(byte >> 4)] as char);
            out.push(DIGITS[usize::from(byte & 0x0f)] as char);
            out
        })
}

fn decode_json<T: serde::de::DeserializeOwned>(segment: &str) -> Option<T> {
    serde_json::from_slice(&BASE64URL.decode(segment).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ring::rand::SystemRandom;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    use serde_json::json;
    use txcript_share_core::identity::conformance;

    use super::*;

    const TEAM: &str = "example";
    const AUD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // A throwaway RSA-2048 key pair, PKCS#1, and its modulus as a JWK would
    // publish it. Two of them: one is the team's, the other is the forger's.
    const KEY_A: &str = "\
        MIIEowIBAAKCAQEAmOC0/gwyY2duimguNlG91V1hXrUpWaFfRETuIjni2A4hxuYaGWabaHPuoZ1S\
        ZYMzMIw48FXy0du6onjpG5dFTKPRIzpgHRdsrhnNzwNEDURUieF84UXHbD19ZVs91YDm8c3cMuM2\
        sNaIeKf4eK0g3cllopE9bFJf/qJWnGcSFQuHhSgo9JI2UMwGlDKV9UKrAMEvMDREwuP5xYv0sUJM\
        tHRAB/TCxUHbGqgdqiOOlEqmdHH3qnVqhtzl4XLJ1NoP5o4CnMgalg40dNizNWY5NjX/xCHDPqfg\
        1vJ5BsayWPm4TYpQ5vzhWZToLinQsVqJ161NCMTi/WDa36fH8InTbQIDAQABAoIBADI5XGy74A/1\
        KoUw/cGdsCJ5F6SQGsIV+GDKznsTDlnRprob3UYsBfFWaPbSYv/zju0rnAclDW1xZQq8c0S7uSoZ\
        BXuv0WStTeiSlKEmXwSGxsk3eZnenUoLl3cldxZ9zyFwcp+LMuv8xv/wmvo2Un5ajFfQpF/CXkQe\
        3Bps6C1eRo4VD50v/+B2YfpuRW3dcYBHLEey8svDrK2GQjjWGTviZ8ajBdmsJ0RLtAnmE31t7/70\
        0zopNIrmcI8wdScMeunnp0qXraYhkzI3yzbgpU6gvnAjZ9pBdvgmpMarJaGBATBWcWKb+hs2619z\
        1lGUwN/eS4qSCxhhKcUJCJlaVjcCgYEA0oiNgzNaejWdZvvYtwoG9M/peykBXthur+anmz+0z1/S\
        kcb5jYOG9hjvagFjwEzfCBsnSl1Satln9wrSIrWy3U51q8Uq213t7c04FGj2DfvxmoUpvyqsQHsn\
        y9q0te2XTFAUoNOh59MS3kaukZkkla5iz0tTulX68v1CBlzOrBsCgYEAueSh20Njp/V2ryNBGXJC\
        Q8UG5nw+CgkU3Eb0ebNHBBxy+vlpQdD7Z7KNMqZ4jj0hXyRpnmi8BaOiwIHNpZmEwufe95TqwvtU\
        TBpZaUd3fzXLvuqobHQWPuTahVk0vkGVSpe26IkA8jj9doKPxwd7AxT5hsvBt+CLeIf4N4jE5xcC\
        gYAsVonG+ceyORxfFeb8FWaFpEu9nlMlkFsvPFpL/cysZ7fG76qavPptVa8GGijR1N6brGxH4wN6\
        cTLN+j9rA+0ZYm6xsCJodI2pKTEIS1qWc1rcefLiya/hHI6zBepM7i6Q6cSOYkOUuQUePrCBBUmJ\
        JGK22VxWv8jL575B7MWxxQKBgDhNB7yJ33/6NxT6P3g+g9VUsi9Sh9OwRnIkx1yosKSNHUHoEjoN\
        2mbgzCUACFlEKHxRYe/JVtD6a4uUhL1YDr6dTYl8v9GIH1LhVB0vuQB9QZU0KwiV7DrmQ0zJ5NCO\
        unGaG5q4C+JQ4mtnRbaJDHe1fZGW2rgfOP6rZ8EiGkjHAoGBALMQrWCLlNqmyzM7XSGyufVI60fg\
        XU/p3T/HRRRvSq1bbwb8Ae4mxM1yB4/OfImW9fGR9G67wlku4vYu0XlNeznhCQlm804RobCTZ1d/\
        4+KW6Pb77iWr6R4i/K7iko+2/aSP+e4Sdz2mFZeMu1PkuX+0455l/gdQfbW5r4EP9Gfp";
    const KEY_A_N: &str = "\
        mOC0_gwyY2duimguNlG91V1hXrUpWaFfRETuIjni2A4hxuYaGWabaHPuoZ1SZYMzMIw48FXy0du6\
        onjpG5dFTKPRIzpgHRdsrhnNzwNEDURUieF84UXHbD19ZVs91YDm8c3cMuM2sNaIeKf4eK0g3cll\
        opE9bFJf_qJWnGcSFQuHhSgo9JI2UMwGlDKV9UKrAMEvMDREwuP5xYv0sUJMtHRAB_TCxUHbGqgd\
        qiOOlEqmdHH3qnVqhtzl4XLJ1NoP5o4CnMgalg40dNizNWY5NjX_xCHDPqfg1vJ5BsayWPm4TYpQ\
        5vzhWZToLinQsVqJ161NCMTi_WDa36fH8InTbQ";
    const KEY_B: &str = "\
        MIIEowIBAAKCAQEAtCf/BdmGkI6kRpN9bMN2VeLo2vAx3Ldoux2fq7f54hIZht9RBjSK5xaMBfDm\
        LHai/02iAVHxg+4YQAOAKfkshl2VK3qJP6JLgI9trjC/B+YhGHYL1x3vFi33bv3sztHrEwgqGERS\
        Vy439v/gvGb/JygUX+B6F5pq2jlzcgwZSK+KqACEl8+YPYyfwUu9ssmbDYUgSCqH6plrXagO1GTy\
        SFiczAFSEp81xANC4ywjejWhan3HCx/HleyVu5nfWEWqYymt5oiwGMRw8ZEuSBHXf5jmRkQOGjy+\
        31VWoB8ap5AnVQFqNcW0FnPYklwWewXAe6ozmsuCVJ7rV4N8HcbxyQIDAQABAoIBAAnGccv7dZW6\
        Z6d8sT2JjY4zdbcLeWkkeoZMoTFMFj7yBHiQ/XB8wVywmIBqBdM7zLRVngi8TYJ/FMnEZtvgLU5N\
        HZ7yUygkwjwDjEv7USI4lmshXJsbgLGPZPGczZCbS0oJE1+ltWINm/PHBoayf/276v8Yywck1c9p\
        dp4lkemjpVLfQpyGp7Ce579mTV59KWQoyaAFuOAfwkMP1Vxn0J6O9E5Vkxm24supOwBWq8xPdCZ3\
        DUEl19BUcZECd6SERdnGKzNJMQvGQJJA+G/sAgCvza+yI81eg4l80QUIO41q59dIJU7knR3JBWc/\
        RbW+R1xrhoer7yf+JsXQUuyL1UECgYEA7SEgJLli1uchgxSP0Nfazi+CprVuXO2j6LwV6Yu0tbg+\
        LIgUpJPwkGySjlorgRAuMZoiO7R07Lyw/1pNp7wweaFWtQ1Oa0fMRjIOco0zXak/Z+ritI715ju+\
        QLBjnKfEcPpIqEVwYFJSF5jVZqD5MZl034PlUIWh7VV6YJAk0MUCgYEAwn4yDKmi4H1lCcqI5naK\
        BYBDttnovNpbkNOGmL2EZGtGSnMD5kZi3+ch5j1vfaDLy4lJtfFeeaX05lRWP77uuIdds1gxB7LI\
        oMbjAe3Of9WkxPG1wmX2cGVdT/jbagLJUWjVOTmgMnpCWib5zboV+0Bz5GrOqniBQa6XTwlCZTUC\
        gYEA0n/CafdLv1vECvl4xTqomLniMB0E0GeARnyYsw56p4nbX2qZcJOHTJ2k58sxrUtwxkV+OCP0\
        W6cRNEy0fL75BS/sqaIbR+6fbnzHCHdB7tXsXFJNV11E+lF0jTZH5uui3WvAjee+XzMUfrAEWCtz\
        qsz/y97o9Lb7zJRBo3CxzcUCgYAbzR4qYTU/Ea2XsLPQ/bDNCIClapCyLiRYl2PuAWkjUZJCoh40\
        lGsdxlQ6LR36vzliZsV6lH4EtYxEQFnz0r8c8XybXkfj6dJz6PoaFHwoGcnrvQFWQGzxtAuamuAC\
        T1Onp2yTOYGEtaU2bcvXdHof6B6oz/uqsn8HfIV0bsZm4QKBgFO2//McPb2x19c95N+UiD+s5GSI\
        Ha8Ut4tpuCLF5C6EfFW7/qRUy3X54rhfC5AFdHqCHkd9U8ms+TwztpEyeJDJskv1W/pemyIU+nhI\
        NzM2QCkgfAovYHTQ8ZHvSKWoaecQURbr+c3L4Ip929i//kewHyPbjI9WoftWioGJgpsp";

    fn key_pair(pkcs1: &str) -> RsaKeyPair {
        let der = base64::engine::general_purpose::STANDARD
            .decode(pkcs1)
            .expect("test key is base64");
        RsaKeyPair::from_der(&der).expect("test key is a PKCS#1 RSA key")
    }

    fn jwk(kid: &str, n: &str) -> JsonWebKey {
        JsonWebKey {
            kid: kid.to_string(),
            kty: "RSA".to_string(),
            alg: Some("RS256".to_string()),
            n: n.to_string(),
            // 65537, as every RSA JWK in practice.
            e: "AQAB".to_string(),
        }
    }

    /// Mint a token the way Access would, so the tests exercise the real
    /// signature path rather than a stub of it.
    fn sign(key: &RsaKeyPair, kid: &str, claims: &serde_json::Value) -> String {
        let header = json!({ "alg": "RS256", "kid": kid, "typ": "JWT" });
        let signed = format!(
            "{}.{}",
            BASE64URL.encode(header.to_string()),
            BASE64URL.encode(claims.to_string())
        );
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signed.as_bytes(),
            &mut signature,
        )
        .expect("signing succeeds");
        format!("{signed}.{}", BASE64URL.encode(signature))
    }

    fn in_an_hour() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs()
            + 3600
    }

    fn sso(email: &str) -> serde_json::Value {
        json!({
            "iss": format!("https://{TEAM}.cloudflareaccess.com"),
            "aud": [AUD],
            "exp": in_an_hour(),
            "email": email,
        })
    }

    fn service_token(common_name: &str) -> serde_json::Value {
        json!({
            "iss": format!("https://{TEAM}.cloudflareaccess.com"),
            "aud": [AUD],
            "exp": in_an_hour(),
            "common_name": common_name,
        })
    }

    /// A key set that can be swapped and that counts what it costs.
    struct Published {
        keys: Mutex<Result<Vec<JsonWebKey>, String>>,
        fetches: AtomicUsize,
        served: AtomicUsize,
    }

    impl Published {
        fn new(keys: Vec<JsonWebKey>) -> Arc<Self> {
            Arc::new(Self {
                keys: Mutex::new(Ok(keys)),
                fetches: AtomicUsize::new(0),
                served: AtomicUsize::new(0),
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                keys: Mutex::new(Err("connection refused".to_string())),
                fetches: AtomicUsize::new(0),
                served: AtomicUsize::new(0),
            })
        }

        fn set(&self, keys: Result<Vec<JsonWebKey>, String>) {
            *self.keys.lock().expect("not poisoned") = keys;
        }

        fn fetches(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }

        /// Fetching is another thread's job now, so a test that provokes one
        /// has to wait for it rather than assume it already happened.
        fn awaits_fetch(&self, nth: usize) {
            awaits("fetch", nth, &self.fetches);
        }

        /// The same for a fetch that *succeeded*, so a test can watch the
        /// service recover without asking it anything.
        fn awaits_served_key_set(&self, nth: usize) {
            awaits("served key set", nth, &self.served);
        }
    }

    fn awaits(what: &str, nth: usize, counter: &AtomicUsize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while counter.load(Ordering::SeqCst) < nth {
            assert!(
                Instant::now() < deadline,
                "waited for {what} {nth}, saw {}",
                counter.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    impl Jwks for Arc<Published> {
        fn fetch(&self) -> Result<Vec<JsonWebKey>, String> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let fetched = match self.keys.lock() {
                Ok(keys) => keys.clone(),
                Err(_) => Err("the test key set is poisoned".to_string()),
            };
            if fetched.is_ok() {
                self.served.fetch_add(1, Ordering::SeqCst);
            }
            fetched
        }
    }

    fn verifier(source: Arc<Published>) -> CloudflareAccess {
        CloudflareAccess::with_keys(TEAM, AUD, Box::new(source))
    }

    /// The same, refreshing fast enough that a test need not sit through a
    /// deployment's intervals.
    fn eager_verifier(source: Arc<Published>) -> CloudflareAccess {
        CloudflareAccess::refreshing(TEAM, AUD, Box::new(source), eagerly())
    }

    fn eagerly() -> Rate {
        Rate {
            ttl: Duration::from_millis(20),
            retry: Duration::from_millis(20),
        }
    }

    fn presented(token: &str) -> Headers {
        Headers::new().with(HEADER, token)
    }

    fn label_of(identity: &CloudflareAccess, token: &str) -> Result<Option<String>, IdentityError> {
        Ok(identity
            .principal(&presented(token))?
            .and_then(|who| who.label))
    }

    #[test]
    fn a_valid_token_yields_its_identity() {
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let token = sign(&key_pair(KEY_A), "k1", &sso("alice@example.com"));
        let who = identity
            .principal(&presented(&token))
            .expect("no backend failure")
            .expect("a principal");
        assert_eq!(who.label.as_deref(), Some("alice@example.com"));
        assert_eq!(who.kind, PrincipalKind::Human);
    }

    #[test]
    fn a_service_token_is_a_distinct_service_principal() {
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let key = key_pair(KEY_A);
        let human = identity
            .principal(&presented(&sign(&key, "k1", &sso("ci@example.com"))))
            .expect("no backend failure")
            .expect("a principal");
        let robot = identity
            .principal(&presented(&sign(
                &key,
                "k1",
                &service_token("ci@example.com"),
            )))
            .expect("no backend failure")
            .expect("a principal");

        assert_eq!(robot.kind, PrincipalKind::Service);
        assert_eq!(human.kind, PrincipalKind::Human);
        // Same string, so the same id — the hash is of the identity and
        // nothing else, which is what keeps it equal to the Worker's. The
        // kinds still differ, and a policy may read that.
        assert_eq!(robot.id, human.id);

        let other = identity
            .principal(&presented(&sign(&key, "k1", &service_token("builder"))))
            .expect("no backend failure")
            .expect("a principal");
        assert_ne!(other.id, robot.id);
    }

    #[test]
    fn a_token_this_service_will_not_accept_is_none_never_an_error() {
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let key = key_pair(KEY_A);
        let valid = sign(&key, "k1", &sso("alice@example.com"));

        let mut wrong_audience = sso("alice@example.com");
        wrong_audience["aud"] = json!(["some-other-application"]);
        let mut expired = sso("alice@example.com");
        expired["exp"] = json!(in_an_hour() - 7200);
        let mut wrong_issuer = sso("alice@example.com");
        wrong_issuer["iss"] = json!("https://attacker.cloudflareaccess.com");
        let mut no_identity = sso("alice@example.com");
        no_identity["email"] = json!("");

        // An honest `kid`, a signature that is not the key's.
        let forged = {
            let (body, signature) = valid.rsplit_once('.').expect("three parts");
            let mut bytes = BASE64URL.decode(signature).expect("base64");
            bytes[0] ^= 0x01;
            format!("{body}.{}", BASE64URL.encode(bytes))
        };
        // Signed by a real key, just not the one published under `k1`.
        let other_key = sign(&key_pair(KEY_B), "k1", &sso("alice@example.com"));

        for (case, token) in [
            ("wrong audience", sign(&key, "k1", &wrong_audience)),
            ("expired", sign(&key, "k1", &expired)),
            ("wrong issuer", sign(&key, "k1", &wrong_issuer)),
            ("no identity claim", sign(&key, "k1", &no_identity)),
            ("forged signature", forged),
            ("signed by another key", other_key),
            ("truncated", valid[..valid.len() / 2].to_string()),
            (
                "no signature",
                valid.rsplit_once('.').expect("parts").0.into(),
            ),
            ("four segments", format!("{valid}.extra")),
            ("empty", String::new()),
            ("not base64", "!!!.!!!.!!!".to_string()),
        ] {
            assert_eq!(
                label_of(&identity, &token),
                Ok(None),
                "{case} must be refused, and refused as a 401"
            );
        }
    }

    #[test]
    fn a_rotation_is_picked_up_by_the_refresher() {
        // A kid we do not hold is refused while we do not hold it, and
        // verifies once the refresher has fetched the set containing it.
        let published = Published::new(vec![jwk("old", KEY_A_N)]);
        let identity = eager_verifier(published.clone());
        let key = key_pair(KEY_A);
        let rotated = sign(&key, "new", &sso("alice@example.com"));

        assert_eq!(label_of(&identity, &rotated), Ok(None));

        published.set(Ok(vec![jwk("new", KEY_A_N)]));
        published.awaits_served_key_set(2);
        assert_eq!(
            label_of(&identity, &rotated),
            Ok(Some("alice@example.com".to_string()))
        );
    }

    #[test]
    fn an_unknown_kid_costs_no_outbound_request() {
        // Fetching is the refresher's alone, so no volume of forged `kid`s
        // can point this service's subrequests at Cloudflare.
        let published = Published::new(vec![jwk("k1", KEY_A_N)]);
        let identity = verifier(published.clone());
        let key = key_pair(KEY_A);

        for attempt in 0..20 {
            let token = sign(&key, &format!("forged-{attempt}"), &sso("mallory@x.com"));
            assert_eq!(
                label_of(&identity, &token),
                Ok(None),
                "an unknown kid is a 401, not an outage"
            );
        }
        assert_eq!(published.fetches(), 1, "the one fetch at construction");
    }

    #[test]
    fn a_request_never_waits_on_the_key_set() {
        // The property the refresher exists for. Fetching inside
        // `principal()` means a certs endpoint that hangs holds whichever
        // thread is serving the request — and with a forged `kid` to
        // provoke it, every such thread.
        struct Slow {
            fetches: AtomicUsize,
            keys: Vec<JsonWebKey>,
        }

        impl Jwks for Arc<Slow> {
            fn fetch(&self) -> Result<Vec<JsonWebKey>, String> {
                // The first is the constructor's; every later one hangs.
                if self.fetches.fetch_add(1, Ordering::SeqCst) > 0 {
                    std::thread::sleep(Duration::from_secs(3));
                }
                Ok(self.keys.clone())
            }
        }

        let source = Arc::new(Slow {
            fetches: AtomicUsize::new(0),
            keys: vec![jwk("k1", KEY_A_N)],
        });
        let identity = CloudflareAccess::refreshing(TEAM, AUD, Box::new(source), eagerly());
        let forged = sign(&key_pair(KEY_A), "forged", &sso("mallory@x.com"));

        let started = Instant::now();
        for _ in 0..5 {
            assert_eq!(label_of(&identity, &forged), Ok(None));
        }
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "requests waited {:?} on a hung fetch",
            started.elapsed()
        );
    }

    #[test]
    fn an_empty_email_is_refused_the_way_the_worker_refuses_it() {
        // `claims.email ?? claims.common_name` keeps an empty string, so the
        // Worker rejects this token. Falling through to `common_name` here
        // would authenticate it, and the same token would be two different
        // callers depending on which host it reached.
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let mut claims = service_token("robot");
        claims["email"] = json!("");
        let token = sign(&key_pair(KEY_A), "k1", &claims);
        assert_eq!(label_of(&identity, &token), Ok(None));
    }

    #[test]
    fn an_unreachable_key_set_is_an_error_not_an_anonymous_request() {
        let identity = verifier(Published::failing());
        let token = sign(&key_pair(KEY_A), "k1", &sso("alice@example.com"));
        assert!(
            identity.principal(&presented(&token)).is_err(),
            "a JWKS outage must be a 503, not a wall of 401s"
        );
    }

    #[test]
    fn keys_already_held_survive_a_failing_refresh() {
        let published = Published::new(vec![jwk("k1", KEY_A_N)]);
        let identity = eager_verifier(published.clone());
        let key = key_pair(KEY_A);
        let token = sign(&key, "k1", &sso("alice@example.com"));
        assert_eq!(
            label_of(&identity, &token),
            Ok(Some("alice@example.com".to_string()))
        );

        // Break the endpoint and let the refresher fail against it.
        published.set(Err("connection refused".to_string()));
        published.awaits_fetch(2);

        assert_eq!(
            label_of(&identity, &token),
            Ok(Some("alice@example.com".to_string())),
            "a refresh failure must not revoke keys that still verify honest tokens"
        );
    }

    #[test]
    fn a_failed_fetch_is_retried_without_waiting_out_the_ttl() {
        // The unit starts `After=network.target`, so the fetch at startup
        // can land before there is a route out. Until one succeeds every
        // request is a 503, and waiting out the TTL to try again is the
        // outage rather than the protection against one.
        let published = Published::failing();
        let identity =
            CloudflareAccess::refreshing(TEAM, AUD, Box::new(published.clone()), eagerly());

        // No requests at all: the refresher keeps trying on its own.
        published.awaits_fetch(3);

        published.set(Ok(vec![jwk("k1", KEY_A_N)]));
        published.awaits_served_key_set(1);
        let token = sign(&key_pair(KEY_A), "k1", &sso("alice@example.com"));
        assert_eq!(
            label_of(&identity, &token),
            Ok(Some("alice@example.com".to_string())),
            "the service must recover on its own once the endpoint comes back"
        );
    }

    #[test]
    fn an_unknown_kid_is_a_401_while_the_key_set_is_current() {
        // We hold what the team publishes and that `kid` is not in it, so
        // the refusal is the caller's — a forged flood must not read as an
        // outage of this service.
        let published = Published::new(vec![jwk("k1", KEY_A_N)]);
        let identity = verifier(published);
        let key = key_pair(KEY_A);

        let forged = sign(&key, "forged", &sso("mallory@x.com"));
        assert_eq!(label_of(&identity, &forged), Ok(None));
        assert_eq!(
            label_of(&identity, &sign(&key, "k1", &sso("alice@example.com"))),
            Ok(Some("alice@example.com".to_string())),
            "and the keys we hold keep working"
        );
    }

    #[test]
    fn an_unknown_kid_is_a_503_once_the_key_set_is_no_longer_current() {
        // Past the TTL a refresh was due and did not land. The keys still
        // verify what they can — that is the grace — but "I do not have
        // that kid" is no longer a statement about the caller.
        let published = Published::new(vec![jwk("k1", KEY_A_N)]);
        let identity = CloudflareAccess::refreshing(
            TEAM,
            AUD,
            Box::new(published.clone()),
            Rate {
                ttl: Duration::from_millis(1),
                // Long enough that the refresher goes quiet after the first
                // failure instead of racing this test.
                retry: Duration::from_secs(3600),
            },
        );
        let key = key_pair(KEY_A);

        published.set(Err("connection refused".to_string()));
        published.awaits_fetch(2);
        std::thread::sleep(Duration::from_millis(10));

        assert!(
            label_of(&identity, &sign(&key, "forged", &sso("mallory@x.com"))).is_err(),
            "a kid we cannot rule out must not be reported as the caller's fault"
        );
        assert_eq!(
            label_of(&identity, &sign(&key, "k1", &sso("alice@example.com"))),
            Ok(Some("alice@example.com".to_string())),
            "a stale key set still verifies what it can"
        );
    }

    #[test]
    fn the_principal_id_is_the_same_digest_the_worker_computes() {
        // `printf %s alice@example.com | sha256sum`. Pinned because the two
        // hosts can front the same bucket: if they disagreed, who owns a
        // transcript would depend on which one published it.
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let token = sign(&key_pair(KEY_A), "k1", &sso("alice@example.com"));
        let who = identity
            .principal(&presented(&token))
            .expect("no backend failure")
            .expect("a principal");
        assert_eq!(
            who.id.as_str(),
            "ff8d9819fc0e12bf0d24892e45987e249a28dce836a85cad60e28eaaa8c6d976"
        );
    }

    #[test]
    fn cloudflare_access_satisfies_the_conformance_suite() {
        let identity = verifier(Published::new(vec![jwk("k1", KEY_A_N)]));
        let key = key_pair(KEY_A);

        conformance::absent_credential_is_none(&identity);
        conformance::malformed_credential_is_not_an_outage(&identity, HEADER);
        conformance::unknown_credential_is_none(&identity, HEADER, "not.a.jwt");

        // The inputs that collided under the prototype's lowercase-and-
        // replace scheme, each inside a real signed token, plus a service
        // token so both claim shapes are covered.
        let tokens: Vec<String> = [
            "a+b@x.com",
            "a_b@x.com",
            "a b@x.com",
            "A.B@x.com",
            "a.b@x.com",
        ]
        .iter()
        .map(|identity| sign(&key, "k1", &sso(identity)))
        .chain(std::iter::once(sign(&key, "k1", &service_token("robot"))))
        .collect();
        let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
        conformance::ids_are_injective(&identity, HEADER, &tokens);
    }
}
