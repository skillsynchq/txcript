//! The S3 backend against the **same** conformance suite as every other.
//!
//! This is the test the seam was designed for. `S3` is the backend the
//! service ships on; `InMemory` and `Filesystem` were written first. If `S3`
//! had needed a single assertion relaxed, or a trait method widened, the
//! abstraction would have been shaped around a filesystem rather than around
//! storage — so the suite is imported verbatim, with no S3-specific case and
//! no skip.
//!
//! Requires a live S3-compatible endpoint. Set `TXCRIPT_S3_ENDPOINT` (for
//! example a local Silo container) together with credentials; the test skips
//! when it is unset so a normal `cargo test` needs no infrastructure.
//!
//! Silo is the community-maintained fork of `MinIO`, kept protocol- and
//! format-compatible with it: the same S3 surface, the same `MINIO_*`
//! variables, and the same `minioadmin` default credentials.
//!
//! ```sh
//! docker run -d --name txcript-silo -p 127.0.0.1:19000:9000 \
//!   docker.io/pgsty/silo:latest server /data
//! TXCRIPT_S3_ENDPOINT=http://127.0.0.1:19000 \
//!   AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//!   AWS_REGION=us-east-1 \
//!   cargo test -p txcript-share-store --features s3 --test s3_conformance
//! ```

#![cfg(feature = "s3")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use txcript_share_store::{S3, conformance};

/// A client for the configured endpoint, or `None` when none is configured.
fn client() -> Option<Client> {
    let endpoint = std::env::var("TXCRIPT_S3_ENDPOINT").ok()?;
    let key = std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "minioadmin".into());
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(
            std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into()),
        ))
        .endpoint_url(endpoint)
        // Silo and most self-hosted gateways serve path-style only.
        .force_path_style(true)
        .credentials_provider(Credentials::new(key, secret, None, None, "txcript-test"))
        .build();
    Some(Client::from_conf(config))
}

async fn bucket(client: &Client, name: &str) {
    // Creating an existing bucket is not an error worth failing on.
    let _ = client.create_bucket().bucket(name).send().await;
}

#[tokio::test]
async fn s3_satisfies_the_store_contract() {
    let Some(client) = client() else {
        eprintln!("skipping: TXCRIPT_S3_ENDPOINT is not set");
        return;
    };
    let name = "txcript-share-conformance";
    bucket(&client, name).await;

    // Each conformance case gets its own key-space root, which is how a
    // shared bucket stands in for the fresh store the suite expects. The
    // factory is `Fn`, so the counter is shared rather than captured by
    // value.
    let counter = std::cell::Cell::new(0u32);
    let run = run_id();
    conformance::all(|| {
        counter.set(counter.get() + 1);
        S3::new(client.clone(), name).with_root(format!("case-{}-{run}", counter.get()))
    })
    .await;
}

/// A per-run prefix, so a re-run does not inherit the last run's objects.
fn run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |since| since.as_nanos().to_string())
}

/// The property a filesystem cannot provide: two racing creates, exactly one
/// winner. This is why `Precondition::IfAbsent` exists as a distinct variant
/// rather than being folded into an unconditional write.
#[tokio::test]
async fn a_conditional_create_is_atomic() {
    use txcript_share_core::plan::Precondition;
    use txcript_share_store::{Attrs, ObjectStore, StoreError};

    let Some(client) = client() else {
        eprintln!("skipping: TXCRIPT_S3_ENDPOINT is not set");
        return;
    };
    let name = "txcript-share-conformance";
    bucket(&client, name).await;
    let store = S3::new(client, name).with_root(format!("race-{}", run_id()));
    let key = conformance::key("alice", "contested");

    let first = store
        .put(&key, b"first", &Attrs::new(), &Precondition::IfAbsent)
        .await;
    let second = store
        .put(&key, b"second", &Attrs::new(), &Precondition::IfAbsent)
        .await;

    assert!(first.is_ok(), "the first create must win");
    assert!(
        matches!(second, Err(StoreError::PreconditionFailed)),
        "the second must lose, got {second:?}"
    );
    let body = store.get(&key).await.expect("get").expect("present").body;
    assert_eq!(body, b"first", "the loser must not have overwritten");
}
