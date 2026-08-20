//! The persistent search cache behind `--cache`: extracted documents
//! ([`Extracted`]) keyed by session and change cursor, in one `SQLite` file.
//!
//! `query` is stateless by design — every run discovers, parses, and
//! extracts every session. That is the right default for a tool with no
//! install footprint, and too slow for a shell binding that opens the picker
//! on every keypress of muscle memory. The cache trades a file for that
//! time: a session whose cursor ([`txcript::local::fingerprints`]) still
//! matches the cached row is deserialized instead of parsed, which skips the
//! native-format read and the conversion to [`Common`](txcript::Common) and
//! pays only a UTF-32 transcode of the lines that survive extraction.
//!
//! The cache is disposable. Rows are keyed by the crate version that wrote
//! them — a new version starts from an empty table rather than trusting an
//! older extraction — and anything that goes wrong with the file degrades to
//! the stateless path with a warning, never to a failed command.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use txcript::search::{DocKey, Extracted};

/// The writer whose rows this cache trusts: the extraction logic lives in
/// the library, so a new release invalidates everything.
const WRITER: &str = concat!("txcript-cli ", env!("CARGO_PKG_VERSION"));

/// An open cache file.
pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open the cache at `path`, creating the file (and its parent
    /// directories) when absent, and discarding rows written by another
    /// version.
    ///
    /// # Errors
    /// When the directory can't be created or the database can't be opened
    /// or initialized.
    pub fn open(path: &Path) -> Result<Cache, String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        let conn =
            Connection::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
        // WAL keeps a concurrent reader (a second `query`, the MCP server)
        // from blocking on a writer; NORMAL sync is plenty for a cache.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS docs (
                 harness TEXT NOT NULL,
                 id TEXT NOT NULL,
                 source TEXT NOT NULL,
                 cursor TEXT NOT NULL,
                 doc BLOB NOT NULL,
                 PRIMARY KEY (harness, id, source)
             );",
        )
        .map_err(|e| e.to_string())?;
        let writer: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'writer'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        if writer.as_deref() != Some(WRITER) {
            conn.execute_batch("DELETE FROM docs;")
                .map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('writer', ?1)",
                params![WRITER],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Cache { conn })
    }

    /// The cached extraction for `key`, when its stored cursor equals
    /// `cursor`. An empty cursor means "unknown whether it changed" and never
    /// hits. A row that fails to deserialize is treated as a miss — the
    /// caller re-parses and overwrites it.
    #[must_use]
    pub fn get(&self, key: &DocKey, cursor: &str) -> Option<Extracted> {
        self.get_raw(key, cursor)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// [`Cache::get`] without the deserialization: the stored bytes, for a
    /// caller that thaws them on another thread. Deserialize with
    /// `serde_json::from_slice::<Extracted>`.
    #[must_use]
    pub fn get_raw(&self, key: &DocKey, cursor: &str) -> Option<Vec<u8>> {
        if cursor.is_empty() {
            return None;
        }
        self.conn
            .query_row(
                "SELECT doc FROM docs WHERE harness = ?1 AND id = ?2 AND source = ?3 AND cursor = ?4",
                params![key.harness.as_str(), key.id, source_of(key), cursor],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Store each `(document, cursor)` pair, replacing any row under the
    /// same key, in one transaction.
    ///
    /// # Errors
    /// When the transaction can't be started or committed, or a row can't
    /// be serialized or written.
    pub fn put_many<'a>(
        &mut self,
        docs: impl IntoIterator<Item = (&'a Extracted, &'a str)>,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR REPLACE INTO docs (harness, id, source, cursor, doc)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| e.to_string())?;
            for (doc, cursor) in docs {
                let key = doc.key();
                let bytes = serde_json::to_vec(doc).map_err(|e| e.to_string())?;
                insert
                    .execute(params![
                        key.harness.as_str(),
                        key.id,
                        source_of(key),
                        cursor,
                        bytes
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Drop every row whose key is not in `live` — sessions deleted since
    /// they were cached.
    ///
    /// # Errors
    /// When the rows can't be read or deleted.
    pub fn retain(&mut self, live: &HashSet<DocKey>) -> Result<(), String> {
        let stale: Vec<(String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT harness, id, source FROM docs")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok)
                .filter(|(harness, id, source): &(String, String, String)| {
                    let Ok(harness) = harness.parse() else {
                        // A harness this version doesn't know: not live.
                        return true;
                    };
                    let key = DocKey {
                        harness,
                        id: id.clone(),
                        source: (!source.is_empty()).then(|| source.clone()),
                    };
                    !live.contains(&key)
                })
                .collect()
        };
        if stale.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for (harness, id, source) in &stale {
            tx.execute(
                "DELETE FROM docs WHERE harness = ?1 AND id = ?2 AND source = ?3",
                params![harness, id, source],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

/// The `source` column: a key without one stores the empty string, which
/// `retain` maps back to `None`.
fn source_of(key: &DocKey) -> &str {
    key.source.as_deref().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use txcript::common::Meta;
    use txcript::{HarnessId, Transcript};

    fn doc(id: &str) -> Extracted {
        let meta = Meta {
            id: id.to_string(),
            timestamp: chrono::Utc::now(),
            cwd: Some("/work/app".to_string()),
            git_branch: None,
            title: Some(format!("title {id}")),
            cli_version: None,
            model: None,
        };
        let key = DocKey {
            harness: HarnessId::Codex,
            id: id.to_string(),
            source: Some(format!("/sessions/{id}.jsonl")),
        };
        Extracted::new(key, &Transcript::new(meta, Vec::new()))
    }

    #[test]
    fn round_trips_by_key_and_cursor_and_prunes_dead_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("cache.sqlite");
        let mut cache = Cache::open(&path).unwrap();
        let (a, b) = (doc("a"), doc("b"));
        cache.put_many([(&a, "1:1"), (&b, "2:2")]).unwrap();

        let hit = cache.get(a.key(), "1:1").expect("same cursor hits");
        assert_eq!(hit.key(), a.key());
        assert_eq!(hit.meta().title.as_deref(), Some("title a"));
        assert!(cache.get(a.key(), "9:9").is_none(), "changed cursor misses");
        assert!(cache.get(a.key(), "").is_none(), "unknown cursor misses");

        let live: HashSet<DocKey> = [a.key().clone()].into_iter().collect();
        cache.retain(&live).unwrap();
        assert!(cache.get(b.key(), "2:2").is_none(), "dead row pruned");
        assert!(cache.get(a.key(), "1:1").is_some(), "live row kept");
    }

    #[test]
    fn a_different_writer_version_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        let a = doc("a");
        Cache::open(&path).unwrap().put_many([(&a, "1:1")]).unwrap();
        // Forge a foreign writer stamp, as an older or newer release would.
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE meta SET value = 'txcript-cli 0.0.0'", [])
            .unwrap();
        let cache = Cache::open(&path).unwrap();
        assert!(cache.get(a.key(), "1:1").is_none());
    }
}
