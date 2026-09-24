//! The service's entire configuration surface.
//!
//! One file, one path, passed as the single argument. Deliberately no
//! environment sniffing and no implicit credential discovery: secrets arrive
//! as *file paths* to be read at startup, so systemd `LoadCredential`,
//! Kubernetes projected secrets, ECS secrets, sops, and agenix all work
//! without this binary knowing any of them exist. The moment it reads
//! `AWS_SECRET_ACCESS_KEY` itself, it is coupled to one delivery mechanism
//! and several deployment targets are lost.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use txcript_share_core::identity::{ForwardedClientCert, Identity, StaticTokens};
use txcript_share_core::policy::{AllowAll, OwnerPrefix, Policy, ReadOnlyMirror, TeamScoped};
use txcript_share_core::{Principal, PrincipalId, PrincipalKind};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    pub identity: IdentityConfig,
    pub store: StoreConfig,
    pub policy: PolicyConfig,
    #[serde(default)]
    pub limits: Limits,
}

fn default_listen() -> SocketAddr {
    // Loopback by default: a service that binds every interface the moment
    // it starts is one misconfiguration away from being the public endpoint
    // this design says it must never be.
    ([127, 0, 0, 1], 8787).into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityConfig {
    /// A token table read from a file. Every line is `<token> <id> [label]`.
    StaticTokens {
        header: String,
        tokens_file: PathBuf,
    },
    /// Trust an identity header set by an authenticating proxy.
    ///
    /// **The proxy must be the only route to this service.** If the origin
    /// is reachable directly, anyone can set the header and become anyone.
    /// Bind to loopback and put the proxy in front, or use a network policy.
    ForwardedHeader { header: String },
    /// Verify the Cloudflare Access assertion here, rather than trusting a
    /// proxy to have done it.
    ///
    /// Unlike `forwarded_header`, this does not require the tunnel to be the
    /// only route in: a request that reaches the origin some other way
    /// carries no signature this service will accept.
    #[cfg(feature = "cloudflare_access")]
    CloudflareAccess {
        /// The Zero Trust team: the `<team>` of
        /// `<team>.cloudflareaccess.com`.
        team: String,
        /// A file holding the Access application's AUD tag. A path, like
        /// every other credential here, so the deployment chooses how it is
        /// delivered.
        aud_file: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreConfig {
    Filesystem {
        root: PathBuf,
    },
    /// Any S3-compatible endpoint: AWS, `R2`, `MinIO`, Ceph.
    ///
    /// Credentials are **not** configured here. They come from the ambient
    /// AWS chain — instance role, web identity, or a credentials file the
    /// deployment mounts — so the same binary works under an IAM role, a
    /// Kubernetes service account, and a static key file without knowing
    /// which it is.
    #[cfg(feature = "s3")]
    S3 {
        bucket: String,
        /// Omit for AWS; set for `R2`, `MinIO`, or another gateway.
        #[serde(default)]
        endpoint: Option<String>,
        /// Prefix inside the bucket, so one bucket can host several
        /// deployments.
        #[serde(default)]
        root: Option<String>,
        /// Self-hosted gateways generally serve path-style only.
        #[serde(default)]
        force_path_style: bool,
    },
    /// Nothing survives a restart. For tests and demos.
    Memory,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PolicyConfig {
    OwnerPrefix,
    ReadOnlyMirror,
    TeamScoped {
        members: Vec<TeamMember>,
    },
    /// Permits everything. Never for a deployment; present so the test
    /// harness can isolate transport bugs from policy bugs.
    AllowAll,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamMember {
    pub principal: String,
    pub team: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_max_document_bytes")]
    pub max_document_bytes: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

const fn default_max_document_bytes() -> usize {
    10 * 1024 * 1024
}

const fn default_page_size() -> usize {
    1000
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_document_bytes: default_max_document_bytes(),
            page_size: default_page_size(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read { path: PathBuf, detail: String },
    Parse(String),
    Credentials(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Read { path, detail } => {
                write!(f, "reading {}: {detail}", path.display())
            }
            ConfigError::Parse(detail) => write!(f, "parsing configuration: {detail}"),
            ConfigError::Credentials(detail) => write!(f, "loading credentials: {detail}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// # Errors
    /// When the file cannot be read or does not parse.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|error| ConfigError::Read {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
        toml::from_str(&raw).map_err(|error| ConfigError::Parse(error.to_string()))
    }
}

impl IdentityConfig {
    /// # Errors
    /// When a credential file cannot be read, or holds something unusable —
    /// a principal id that is not a plain segment, an empty Access AUD.
    pub fn build(&self) -> Result<Box<dyn Identity>, ConfigError> {
        match self {
            IdentityConfig::ForwardedHeader { header } => {
                Ok(Box::new(ForwardedClientCert::new(header)))
            }
            #[cfg(feature = "cloudflare_access")]
            IdentityConfig::CloudflareAccess { team, aud_file } => {
                // The team name becomes a hostname, so anything that is not
                // one is a configuration error rather than a lookup that
                // fails later against a URL nobody meant to write.
                if team.is_empty() || !team.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(ConfigError::Credentials(format!(
                        "`{team}` is not a Zero Trust team name"
                    )));
                }
                let raw = std::fs::read_to_string(aud_file).map_err(|error| ConfigError::Read {
                    path: aud_file.clone(),
                    detail: error.to_string(),
                })?;
                let audience = raw.trim();
                if audience.is_empty() {
                    return Err(ConfigError::Credentials(
                        "the Access AUD file is empty".to_string(),
                    ));
                }
                Ok(Box::new(crate::access::CloudflareAccess::new(
                    team, audience,
                )))
            }
            IdentityConfig::StaticTokens {
                header,
                tokens_file,
            } => {
                let raw =
                    std::fs::read_to_string(tokens_file).map_err(|error| ConfigError::Read {
                        path: tokens_file.clone(),
                        detail: error.to_string(),
                    })?;
                let mut tokens = StaticTokens::new(header);
                for (number, line) in raw.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let mut fields = line.split_whitespace();
                    let (Some(token), Some(id)) = (fields.next(), fields.next()) else {
                        return Err(ConfigError::Credentials(format!(
                            "line {}: expected `<token> <id> [label]`",
                            number + 1
                        )));
                    };
                    let principal = PrincipalId::new(id).ok_or_else(|| {
                        ConfigError::Credentials(format!(
                            "line {}: `{id}` is not usable as a principal id",
                            number + 1
                        ))
                    })?;
                    let mut built = Principal::new(principal, PrincipalKind::Service);
                    if let Some(label) = fields.next() {
                        built = built.with_label(label);
                    }
                    tokens = tokens.with(token, built);
                }
                Ok(Box::new(tokens))
            }
        }
    }
}

impl PolicyConfig {
    /// # Errors
    /// When a team member names an unusable principal id.
    pub fn build(&self) -> Result<Box<dyn Policy>, ConfigError> {
        Ok(match self {
            PolicyConfig::OwnerPrefix => Box::new(OwnerPrefix),
            PolicyConfig::ReadOnlyMirror => Box::new(ReadOnlyMirror),
            PolicyConfig::AllowAll => Box::new(AllowAll),
            PolicyConfig::TeamScoped { members } => {
                let mut policy = TeamScoped::new();
                for member in members {
                    let id = PrincipalId::new(member.principal.clone()).ok_or_else(|| {
                        ConfigError::Credentials(format!(
                            "`{}` is not usable as a principal id",
                            member.principal
                        ))
                    })?;
                    policy = policy.with(id, member.team.clone());
                }
                Box::new(policy)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_configuration_parses_and_defaults_sensibly() {
        let config: Config = toml::from_str(
            r#"
            [identity]
            kind = "forwarded_header"
            header = "x-forwarded-user"
            [store]
            kind = "memory"
            [policy]
            kind = "owner_prefix"
            "#,
        )
        .expect("parses");
        assert_eq!(config.listen.to_string(), "127.0.0.1:8787");
        assert_eq!(config.limits.max_document_bytes, 10 * 1024 * 1024);
    }

    #[cfg(feature = "cloudflare_access")]
    #[test]
    fn a_cloudflare_access_identity_reads_its_aud_from_a_file() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let aud = directory.path().join("aud");
        std::fs::write(&aud, "  0123456789abcdef\n").expect("writes");

        let config: Config = toml::from_str(&format!(
            r#"
            [identity]
            kind = "cloudflare_access"
            team = "example"
            aud_file = "{}"
            [store]
            kind = "memory"
            [policy]
            kind = "owner_prefix"
            "#,
            aud.display()
        ))
        .expect("parses");
        assert!(config.identity.build().is_ok());
    }

    #[cfg(feature = "cloudflare_access")]
    #[test]
    fn a_team_that_is_not_a_hostname_label_is_refused() {
        // The team becomes part of the JWKS URL. A value that is not a
        // label could point verification at somebody else's key set.
        let identity = IdentityConfig::CloudflareAccess {
            team: "example.com/evil".to_string(),
            aud_file: PathBuf::from("/nonexistent"),
        };
        assert!(matches!(identity.build(), Err(ConfigError::Credentials(_))));
    }

    #[cfg(feature = "cloudflare_access")]
    #[test]
    fn an_empty_aud_file_is_refused_rather_than_matching_nothing() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let aud = directory.path().join("aud");
        std::fs::write(&aud, "\n").expect("writes");
        let identity = IdentityConfig::CloudflareAccess {
            team: "example".to_string(),
            aud_file: aud,
        };
        assert!(matches!(identity.build(), Err(ConfigError::Credentials(_))));
    }

    #[test]
    fn an_unknown_key_is_an_error_rather_than_silently_ignored() {
        // A typo in a security-relevant field must not read as a default.
        let bad = toml::from_str::<Config>(
            r#"
            [identity]
            kind = "forwarded_header"
            header = "x-forwarded-user"
            [store]
            kind = "memory"
            [policy]
            kind = "owner_prefx"
            "#,
        );
        assert!(bad.is_err());
    }
}
