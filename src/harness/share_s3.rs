//! Publishing straight to an object store, with no share service.
//!
//! The same transcripts, the same bucket layout, and the same metadata as
//! [`ShareStore`](super::share::ShareStore) — reached directly rather than
//! through a service. Ownership is enforced by the bucket's own access
//! policy (an IAM prefix condition), because there is nothing else in the
//! path to enforce it.
//!
//! # When this is the right choice
//!
//! Everyone already has credentials for the bucket, the rule is
//! "write your own prefix, read everything", and you would rather not run a
//! service. That covers a surprising number of real deployments.
//!
//! # What you give up
//!
//! - **Identity is the object store's.** Every reader needs bucket
//!   credentials. A service lets you put Access, OIDC, or tokens in front
//!   and keep store credentials in one place.
//! - **Policies a prefix cannot express.** Team scoping reads an object's
//!   stored metadata; a bucket policy cannot.
//! - **No validation.** Nothing checks that what lands is a transcript, so
//!   the bucket accumulates whatever anyone writes.
//! - **Listing costs more.** `ListObjectsV2` does not return user metadata,
//!   so a listing is one `HEAD` per entry — over the internet here, rather
//!   than inside the service's network.
//!
//! The two paths are interchangeable by design: a bucket written directly
//! lists correctly through a service placed in front of it later, and vice
//! versa. `tests/integration/share_round_trip.rs` asserts that.

use std::collections::HashMap;

use txcript_share_core::Key;
use txcript_share_core::plan::Precondition;
use txcript_share_store::{ObjectStore, S3, StoreError, attrs_for_document};

use crate::error::{Error, Result};
use crate::harness::share::{Share, ShareRef};
use crate::transcript::{Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// How many objects one listing page fetches.
const PAGE_SIZE: usize = 1000;

/// Transcripts in an object store, reached directly.
pub struct DirectStore {
    inner: S3,
    /// The prefix this machine publishes under. There is no service to
    /// derive it from an authenticated principal, so it is configuration —
    /// and the bucket policy is what makes it binding.
    owner: String,
    runtime: tokio::runtime::Runtime,
}

impl DirectStore {
    /// Wrap a configured S3 client.
    ///
    /// `owner` is the key prefix to publish under. It must match whatever
    /// the bucket policy allows this identity to write, or the store will
    /// list happily and fail on the first publish.
    ///
    /// # Errors
    /// When `owner` is not usable as a single key segment, or a runtime
    /// cannot be started.
    pub fn new(inner: S3, owner: &str) -> Result<Self> {
        crate::harness::checked_id_component(Share::NAME, owner)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| remote(&format!("could not start a runtime: {error}")))?;
        Ok(Self {
            inner,
            owner: owner.to_string(),
            runtime,
        })
    }

    /// From the environment: `TXCRIPT_SHARE_BUCKET` and
    /// `TXCRIPT_SHARE_OWNER`, optionally `TXCRIPT_SHARE_ENDPOINT` (for `R2`,
    /// `MinIO`, or another gateway) and `TXCRIPT_SHARE_PREFIX`.
    ///
    /// Credentials come from the ambient AWS chain, never from here, so an
    /// instance role, a web identity, and a key file all work unchanged.
    ///
    /// # Errors
    /// When the bucket or owner is unset, or a client cannot be built.
    pub fn from_env() -> Result<Self> {
        let bucket =
            env("TXCRIPT_SHARE_BUCKET").ok_or_else(|| remote("TXCRIPT_SHARE_BUCKET is not set"))?;
        let owner = env("TXCRIPT_SHARE_OWNER").ok_or_else(|| {
            remote("TXCRIPT_SHARE_OWNER is not set: a direct publisher has no service to derive its prefix from")
        })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| remote(&format!("could not start a runtime: {error}")))?;
        let loaded = runtime.block_on(aws_config::load_defaults(
            aws_config::BehaviorVersion::latest(),
        ));
        let mut builder = aws_sdk_s3::config::Builder::from(&loaded);
        if let Some(endpoint) = env("TXCRIPT_SHARE_ENDPOINT") {
            // A gateway that is not AWS almost always serves path-style only.
            builder = builder.endpoint_url(endpoint).force_path_style(true);
        }
        let client = aws_sdk_s3::Client::from_conf(builder.build());
        let mut inner = S3::new(client, bucket);
        if let Some(prefix) = env("TXCRIPT_SHARE_PREFIX") {
            inner = inner.with_root(prefix);
        }

        crate::harness::checked_id_component(Share::NAME, &owner)?;
        Ok(Self {
            inner,
            owner,
            runtime,
        })
    }

    fn key_for(&self, session: &str) -> Result<Key> {
        let owner = txcript_share_core::PrincipalId::new(self.owner.clone())
            .ok_or_else(|| remote("the configured owner is not a usable key segment"))?;
        Key::new(owner, session).ok_or_else(|| remote("the session id is not a usable key segment"))
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn remote(detail: &str) -> Error {
    Error::Remote {
        harness: Share::NAME,
        detail: detail.to_string(),
    }
}

fn store_error(error: &StoreError) -> Error {
    remote(&error.to_string())
}

impl Store for DirectStore {
    type H = Share;
    type Ref = ShareRef;

    /// Every published transcript in the bucket, newest ordering left to the
    /// caller.
    ///
    /// One `HEAD` per entry, because `ListObjectsV2` does not return user
    /// metadata. A service does this on its own network; here it is over the
    /// internet, which is the main reason to put one in front eventually.
    fn discover(&self) -> Result<Vec<Discovered<ShareRef>>> {
        self.runtime.block_on(async {
            let mut found = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .inner
                    .list("", cursor.as_ref(), PAGE_SIZE)
                    .await
                    .map_err(|error| store_error(&error))?;
                for meta in page.objects {
                    found.push(Discovered {
                        meta: crate::common::Meta {
                            id: meta
                                .attrs
                                .get("id")
                                .unwrap_or_else(|| meta.key.session())
                                .to_string(),
                            timestamp: meta
                                .attrs
                                .get("timestamp")
                                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                                .map_or_else(chrono::Utc::now, |value| {
                                    value.with_timezone(&chrono::Utc)
                                }),
                            cwd: meta.attrs.get("cwd").map(str::to_string),
                            git_branch: meta.attrs.get("git_branch").map(str::to_string),
                            title: meta.attrs.get("title").map(str::to_string),
                            cli_version: None,
                            model: meta.attrs.get("model").map(str::to_string),
                        },
                        reference: ShareRef {
                            slug: meta.key.to_slug(),
                            etag: Some(meta.version.as_str().to_string()),
                        },
                    });
                }
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            Ok(found)
        })
    }

    fn load(&self, reference: &ShareRef) -> Result<Transcript<Share>> {
        let key = Key::parse(&reference.slug)
            .ok_or_else(|| remote("that is not a `<owner>/<session>` slug"))?;
        self.runtime.block_on(async {
            let object = self
                .inner
                .get(&key)
                .await
                .map_err(|error| store_error(&error))?
                .ok_or_else(|| remote("no such transcript"))?;
            let text = std::str::from_utf8(&object.body)
                .map_err(|error| remote(&format!("the transcript was not UTF-8: {error}")))?;
            let mut transcript = Share::from_text(text)?;
            if transcript.meta.id.is_empty() {
                transcript.meta.id = key.session().to_string();
            }
            Ok(transcript)
        })
    }

    /// Writes under this machine's configured owner prefix.
    ///
    /// Nothing here stops the prefix being someone else's — **the bucket
    /// policy is the boundary**, and with a permissive one this store will
    /// happily overwrite another publisher.
    fn save(&self, transcript: &Transcript<Share>) -> Result<Saved<ShareRef>> {
        let key = self.key_for(&transcript.meta.id)?;
        let text = Share::to_text(transcript)?;
        let doc: serde_json::Value = serde_json::from_str(&text)?;
        // The same projection the service writes, so a bucket written here
        // lists identically through a service placed in front of it.
        let attrs = attrs_for_document(&doc);

        self.runtime.block_on(async {
            let version = self
                .inner
                .put(&key, text.as_bytes(), &attrs, &Precondition::None)
                .await
                .map_err(|error| store_error(&error))?;
            Ok(Saved {
                id: transcript.meta.id.clone(),
                reference: ShareRef {
                    slug: key.to_slug(),
                    etag: Some(version.as_str().to_string()),
                },
            })
        })
    }

    fn delete(&self, reference: &ShareRef) -> Result<()> {
        let key = Key::parse(&reference.slug)
            .ok_or_else(|| remote("that is not a `<owner>/<session>` slug"))?;
        self.runtime.block_on(async {
            self.inner
                .delete(&key, &Precondition::None)
                .await
                .map_err(|error| store_error(&error))
        })
    }

    /// `ETag`s from the last [`discover`](Store::discover); no extra request.
    fn fingerprints(&self, refs: &[ShareRef]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|reference| (reference.key(), reference.etag.clone().unwrap_or_default()))
            .collect())
    }
}
