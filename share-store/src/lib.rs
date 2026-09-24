//! The `ObjectStore` and `Catalog` seams.
//!
//! [`txcript_share_core`] decides *whether*; this crate defines *where the
//! bytes live* and *how a listing is answered*. Both are traits with more
//! than one implementation on purpose — an interface with a single
//! implementation is a description of that implementation, not an
//! abstraction.
//!
//! The implementations here need no network: [`InMemory`] for determinism
//! and fault injection, [`Filesystem`] for a backing store whose semantics
//! differ from S3 in the ways that matter. `R2` and `S3` live in their hosts and
//! must pass the same [`conformance`] suite unchanged — if either needs a
//! trait widened, the seam was wrong.
//!
//! # Why `Filesystem` is here
//!
//! It is the alternate that earns its keep. `S3` returns user metadata inline
//! from LIST; a directory does not. Requiring a directory to satisfy
//! [`ObjectStore::list`] forces metadata to have a *declared home* — here, a
//! sidecar file — instead of being an S3 accident the caller silently
//! depends on. That assumption is the single biggest unverified one in the
//! Worker prototype.

pub mod catalog;
pub mod filesystem;
pub mod memory;
#[cfg(feature = "s3")]
pub mod s3;

pub use catalog::{Catalog, CatalogError, Entry, Query, StoreDerived};
pub use filesystem::Filesystem;
pub use memory::InMemory;
#[cfg(feature = "s3")]
pub use s3::S3;

use std::collections::BTreeMap;
use std::fmt;

use txcript_share_core::Key;
use txcript_share_core::plan::Precondition;

/// An opaque version token — an `ETag`, a generation number, a content hash.
/// Compared, never parsed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(String);

impl Version {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Caller-supplied metadata stored alongside an object.
///
/// Deliberately a flat string map with a byte budget rather than a typed
/// struct: every backend caps it differently (`R2` at roughly 2 KiB across
/// keys and values), and a budget that is checked in one place cannot be
/// forgotten by one backend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attrs(BTreeMap<String, String>);

/// The ceiling every backend must honour, set by the tightest of them.
pub const MAX_ATTRS_BYTES: usize = 1800;

impl Attrs {
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Insert, truncating the value on a **UTF-8 character boundary** to fit
    /// the remaining budget. Counting characters against a byte budget is
    /// how the Worker prototype turned a non-ASCII title into a failed
    /// upload.
    #[must_use]
    pub fn set(mut self, name: &str, value: &str) -> Self {
        let used = self.byte_len();
        let overhead = name.len();
        if used + overhead >= MAX_ATTRS_BYTES {
            return self;
        }
        let room = MAX_ATTRS_BYTES - used - overhead;
        let mut clipped = value;
        while clipped.len() > room {
            let mut cut = room;
            while cut > 0 && !clipped.is_char_boundary(cut) {
                cut -= 1;
            }
            clipped = &clipped[..cut];
        }
        if !clipped.is_empty() {
            self.0.insert(name.to_string(), clipped.to_string());
        }
        self
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// A copy without `name`. Used by backends that persist bookkeeping
    /// alongside caller attributes and must not hand it back.
    #[must_use]
    pub fn without(mut self, name: &str) -> Self {
        self.0.remove(name);
        self
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.iter().map(|(k, v)| k.len() + v.len()).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The attributes a listing shows for a Simple transcript document.
///
/// Shared so that a service and a direct-to-store client write the *same*
/// metadata: the two paths are meant to be interchangeable, and a bucket
/// written by one has to list correctly through the other. `Attrs` owns the
/// byte budget, so a long non-ASCII title is clipped rather than failing the
/// write.
#[must_use]
pub fn attrs_for_document(doc: &serde_json::Value) -> Attrs {
    let text = |name: &str| {
        doc.get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    };
    let messages = doc
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len);
    [
        ("id", text("id").to_string()),
        ("messages", messages.to_string()),
        ("timestamp", text("timestamp").to_string()),
        ("model", text("model").to_string()),
        ("git_branch", text("git_branch").to_string()),
        ("title", text("title").to_string()),
        ("cwd", text("cwd").to_string()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .fold(Attrs::new(), |attrs, (name, value)| attrs.set(name, &value))
}

/// What a listing returns per object: metadata only, never a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: Key,
    pub version: Version,
    pub size: u64,
    pub attrs: Attrs,
}

/// A stored object with its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub meta: ObjectMeta,
    pub body: Vec<u8>,
}

/// An opaque pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub String);

/// One page of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub objects: Vec<ObjectMeta>,
    /// `Some` when more pages remain.
    pub next: Option<Cursor>,
}

/// Why a store operation failed.
///
/// `PreconditionFailed` is separate from `Backend` because it is the
/// caller's business — a concurrent writer won, and the host must return 412
/// rather than 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    PreconditionFailed,
    Backend(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::PreconditionFailed => f.write_str("object changed since it was read"),
            StoreError::Backend(detail) => write!(f, "store backend failed: {detail}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Bytes at keys, with conditional writes.
///
/// Every method is `async` because the real backends are networked. The
/// suite in [`conformance`] is generic over this trait, so a backend proves
/// itself by passing the same tests as every other.
pub trait ObjectStore {
    /// # Errors
    /// [`StoreError::PreconditionFailed`] when `precondition` does not hold.
    fn put(
        &self,
        key: &Key,
        body: &[u8],
        attrs: &Attrs,
        precondition: &Precondition,
    ) -> impl Future<Output = Result<Version, StoreError>>;

    /// # Errors
    /// When the backend itself fails. A missing object is `Ok(None)`.
    fn get(&self, key: &Key) -> impl Future<Output = Result<Option<Object>, StoreError>>;

    /// Metadata without the body — what a `decide` call needs to build
    /// [`ObjectFacts`](txcript_share_core::plan::ObjectFacts).
    ///
    /// # Errors
    /// When the backend itself fails.
    fn head(&self, key: &Key) -> impl Future<Output = Result<Option<ObjectMeta>, StoreError>>;

    /// Removing an absent key succeeds: delete is idempotent, so a retried
    /// request cannot turn into a spurious error.
    ///
    /// # Errors
    /// [`StoreError::PreconditionFailed`] when `precondition` does not hold.
    fn delete(
        &self,
        key: &Key,
        precondition: &Precondition,
    ) -> impl Future<Output = Result<(), StoreError>>;

    /// One page of keys under `prefix`, in lexicographic order, **with
    /// attributes attached**. The ordering and the attributes are both part
    /// of the contract precisely because S3 gives them away for free and a
    /// filesystem does not.
    ///
    /// # Errors
    /// When the backend itself fails.
    fn list(
        &self,
        prefix: &str,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> impl Future<Output = Result<Page, StoreError>>;
}

pub mod conformance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_truncate_on_character_boundaries_within_the_byte_budget() {
        let attrs = Attrs::new().set("title", &"た".repeat(2000));
        assert!(attrs.byte_len() <= MAX_ATTRS_BYTES);
        // Truncation must not have produced invalid UTF-8 or a partial char.
        let title = attrs.get("title").expect("title kept");
        assert!(title.chars().all(|c| c == 'た'));
    }

    #[test]
    fn attrs_stop_adding_once_the_budget_is_exhausted() {
        let attrs = Attrs::new()
            .set("cwd", &"/".repeat(MAX_ATTRS_BYTES))
            .set("title", "this no longer fits");
        assert!(attrs.byte_len() <= MAX_ATTRS_BYTES);
        assert_eq!(attrs.get("title"), None);
    }
}
