pub mod file_watcher;
pub mod github;

use crate::models::RawDocument;
use std::sync::atomic::AtomicI64;
use std::time::{SystemTime, UNIX_EPOCH};

/// Trait for external data connectors that ingest documents into Mimir.
pub trait Connector: Send + Sync {
    fn name(&self) -> &str;
    fn fetch(&self) -> Result<Vec<RawDocument>, String>;
    fn last_sync(&self) -> &AtomicI64;
}

/// Helper: current unix timestamp in milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Simple connector that produces a fixed set of documents (useful for tests).
#[allow(dead_code)]
pub struct StaticConnector {
    name_str: String,
    docs: Vec<RawDocument>,
    last_sync_ts: AtomicI64,
}

impl StaticConnector {
    #[allow(dead_code)]
    pub fn new(name: &str, docs: Vec<RawDocument>) -> Self {
        Self {
            name_str: name.to_string(),
            docs,
            last_sync_ts: AtomicI64::new(0),
        }
    }
}

impl Connector for StaticConnector {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn fetch(&self) -> Result<Vec<RawDocument>, String> {
        Ok(self.docs.clone())
    }
    fn last_sync(&self) -> &AtomicI64 {
        &self.last_sync_ts
    }
}
