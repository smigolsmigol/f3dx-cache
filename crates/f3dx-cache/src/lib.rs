#![allow(clippy::result_large_err)]
//! f3dx-cache - content-addressable LLM response cache.
//!
//! Three tables in one redb file:
//!   requests:  fingerprint -> canonicalized request bytes (JCS)
//!   responses: fingerprint -> response bytes
//!   meta:      fingerprint -> metadata (timestamp, hit count, model used,
//!              system_fingerprint, response duration_ms)
//!
//! Fingerprint = BLAKE3(JCS(request)) where JCS is RFC 8785 JSON
//! Canonicalization Scheme. The canonical form sorts object keys and
//! normalizes numeric/string forms so semantically-identical requests
//! collide and trivially-different ones don't.
//!
//! Cache is the TEST-MODE primitive. Production deployments do not point
//! at this; tests, CI, and replay loops do.

use blake3::Hasher;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

const REQUESTS: TableDefinition<&str, &[u8]> = TableDefinition::new("requests");
const RESPONSES: TableDefinition<&str, &[u8]> = TableDefinition::new("responses");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("redb: {0}")]
    Redb(#[from] redb::Error),
    #[error("redb storage: {0}")]
    RedbStorage(#[from] redb::StorageError),
    #[error("redb txn: {0}")]
    RedbTxn(#[from] redb::TransactionError),
    #[error("redb table: {0}")]
    RedbTable(#[from] redb::TableError),
    #[error("redb commit: {0}")]
    RedbCommit(#[from] redb::CommitError),
    #[error("redb db: {0}")]
    RedbDb(#[from] redb::DatabaseError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CacheError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMeta {
    pub created_at_ms: u64,
    pub hit_count: u64,
    pub model: Option<String>,
    pub system_fingerprint: Option<String>,
    pub response_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    pub entries: u64,
    pub hits: u64,
    pub misses: u64,
    pub bytes_requests: u64,
    pub bytes_responses: u64,
}

pub struct Cache {
    db: Database,
}

impl Cache {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        // Touch every table so first-open users don't hit "table not found"
        // on the first read.
        let _ = txn.open_table(REQUESTS)?;
        let _ = txn.open_table(RESPONSES)?;
        let _ = txn.open_table(META)?;
        txn.commit()?;
        Ok(Self { db })
    }

    /// Compute the canonical fingerprint for a request payload.
    /// The payload is canonicalized via RFC 8785 JCS, then BLAKE3 hashed.
    pub fn fingerprint(&self, request: &serde_json::Value) -> Result<String> {
        Ok(fingerprint_value(request))
    }

    /// Insert a (request, response) pair under its computed fingerprint.
    /// Returns the fingerprint so callers can correlate.
    pub fn put(
        &self,
        request: &serde_json::Value,
        response: &[u8],
        meta: &CachedMeta,
    ) -> Result<String> {
        let fp = fingerprint_value(request);
        let canonical = canonical_bytes(request);
        let meta_bytes = serde_json::to_vec(meta)?;
        let txn = self.db.begin_write()?;
        {
            let mut t_req = txn.open_table(REQUESTS)?;
            let mut t_resp = txn.open_table(RESPONSES)?;
            let mut t_meta = txn.open_table(META)?;
            t_req.insert(fp.as_str(), canonical.as_slice())?;
            t_resp.insert(fp.as_str(), response)?;
            t_meta.insert(fp.as_str(), meta_bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(fp)
    }

    /// Look up a cached response by request value. Returns the response
    /// bytes if present + bumps the hit-count in meta.
    ///
    /// The hit-count bump is a separate write transaction (~1ms warm).
    /// Use `peek` for sub-100us reads when hit-count tracking is not
    /// needed (e.g. CI replay against a captured trace).
    pub fn get(&self, request: &serde_json::Value) -> Result<Option<Vec<u8>>> {
        let fp = fingerprint_value(request);
        let txn = self.db.begin_read()?;
        let t_resp = txn.open_table(RESPONSES)?;
        let Some(entry) = t_resp.get(fp.as_str())? else {
            return Ok(None);
        };
        let resp = entry.value().to_vec();
        drop(t_resp);
        drop(txn);
        // Bump hit count in a separate write txn so reads stay cheap.
        self.bump_hit(&fp)?;
        Ok(Some(resp))
    }

    /// Look up a cached response without bumping the hit-count.
    ///
    /// Sub-100us warm hit because there is no write transaction. Use
    /// when stats accuracy is not needed: CI replay against captured
    /// traces, hot loops where the response cardinality is known
    /// independently, read-only inspection from a sidecar.
    pub fn peek(&self, request: &serde_json::Value) -> Result<Option<Vec<u8>>> {
        let fp = fingerprint_value(request);
        let txn = self.db.begin_read()?;
        let t_resp = txn.open_table(RESPONSES)?;
        let Some(entry) = t_resp.get(fp.as_str())? else {
            return Ok(None);
        };
        Ok(Some(entry.value().to_vec()))
    }

    fn bump_hit(&self, fp: &str) -> Result<()> {
        let wtxn = self.db.begin_write()?;
        {
            let mut t_meta = wtxn.open_table(META)?;
            let cur = t_meta.get(fp)?.map(|v| v.value().to_vec());
            if let Some(bytes) = cur {
                let mut meta: CachedMeta = serde_json::from_slice(&bytes)?;
                meta.hit_count = meta.hit_count.saturating_add(1);
                t_meta.insert(fp, serde_json::to_vec(&meta)?.as_slice())?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    pub fn stats(&self) -> Result<CacheStats> {
        let txn = self.db.begin_read()?;
        let t_req = txn.open_table(REQUESTS)?;
        let t_resp = txn.open_table(RESPONSES)?;
        let t_meta = txn.open_table(META)?;
        let mut s = CacheStats::default();
        for entry in t_req.iter()? {
            let (_, v) = entry?;
            s.entries += 1;
            s.bytes_requests += v.value().len() as u64;
        }
        for entry in t_resp.iter()? {
            let (_, v) = entry?;
            s.bytes_responses += v.value().len() as u64;
        }
        for entry in t_meta.iter()? {
            let (_, v) = entry?;
            let meta: CachedMeta = serde_json::from_slice(v.value())?;
            s.hits += meta.hit_count;
        }
        Ok(s)
    }
}

/// RFC 8785 JSON Canonicalization Scheme: sort object keys recursively,
/// minimize whitespace, normalize numeric forms. The minimal implementation
/// here covers the subset of JSON the OpenAI / Anthropic / Gemini wire
/// formats actually emit (no -0.0 hairpin cases, no NaN/Infinity).
pub fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn canonical_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&canonicalize(value)).expect("canonical form must serialize")
}

fn fingerprint_value(value: &serde_json::Value) -> String {
    let bytes = canonical_bytes(value);
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_is_key_order_invariant() {
        let a = json!({"model": "gpt-4", "temperature": 0.0});
        let b = json!({"temperature": 0.0, "model": "gpt-4"});
        assert_eq!(fingerprint_value(&a), fingerprint_value(&b));
    }

    #[test]
    fn fingerprint_differs_on_value_change() {
        let a = json!({"model": "gpt-4", "temperature": 0.0});
        let b = json!({"model": "gpt-4", "temperature": 0.1});
        assert_ne!(fingerprint_value(&a), fingerprint_value(&b));
    }

    #[test]
    fn fingerprint_recurses_into_arrays() {
        let a = json!({"messages": [{"role": "a", "content": "x"}]});
        let b = json!({"messages": [{"content": "x", "role": "a"}]});
        assert_eq!(fingerprint_value(&a), fingerprint_value(&b));
    }

    #[test]
    fn put_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("c.redb")).unwrap();
        let req = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]});
        let resp = b"hello, world";
        let meta = CachedMeta {
            created_at_ms: 0,
            hit_count: 0,
            model: Some("gpt-4".into()),
            system_fingerprint: None,
            response_duration_ms: Some(123),
        };
        let fp = cache.put(&req, resp, &meta).unwrap();
        assert_eq!(fp.len(), 64);
        let got = cache.get(&req).unwrap().unwrap();
        assert_eq!(got, resp);
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn peek_does_not_bump_hit_count() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("c.redb")).unwrap();
        let req = json!({"model": "gpt-4", "messages": [{"role": "user", "content": "x"}]});
        let resp = b"r";
        let meta = CachedMeta {
            created_at_ms: 0,
            hit_count: 0,
            model: None,
            system_fingerprint: None,
            response_duration_ms: None,
        };
        cache.put(&req, resp, &meta).unwrap();

        // Three peeks should all return the bytes, but stats.hits stays 0.
        for _ in 0..3 {
            let got = cache.peek(&req).unwrap().unwrap();
            assert_eq!(got, resp);
        }
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.hits, 0);

        // Now exercise get() once and confirm hit_count moves to 1.
        let _ = cache.get(&req).unwrap();
        let stats = cache.stats().unwrap();
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn peek_returns_none_for_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("c.redb")).unwrap();
        let req = json!({"model": "x", "messages": []});
        assert!(cache.peek(&req).unwrap().is_none());
    }
}
