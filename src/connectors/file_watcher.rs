use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicI64;
use std::sync::Mutex;

use crate::connectors::Connector;
use crate::models::RawDocument;

/// Configuration for the filesystem watcher connector.
#[derive(Clone)]
pub struct FileWatcherConfig {
    pub enabled: bool,
    pub paths: Vec<String>,
    pub extensions: Vec<String>,
    #[allow(dead_code)]
    pub debounce_ms: u64,
}

impl Default for FileWatcherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paths: vec![],
            extensions: vec![".md".to_string(), ".txt".to_string()],
            debounce_ms: 1500,
        }
    }
}

/// Connector that watches configured directories for .md/.txt/.json files.
/// Uses the `notify` crate to detect file changes and generates RawDocuments.
pub struct FileWatcher {
    config: FileWatcherConfig,
    last_sync: AtomicI64,
    file_hashes: Mutex<HashMap<PathBuf, String>>,
}

impl FileWatcher {
    pub fn new(config: FileWatcherConfig) -> Self {
        Self {
            config,
            last_sync: AtomicI64::new(0),
            file_hashes: Mutex::new(HashMap::new()),
        }
    }

    /// Compute a simple content hash (not cryptographic — just for dedup).
    fn content_hash(contents: &str) -> String {
        let mut h: u64 = 5381;
        for b in contents.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        format!("{:x}", h)
    }

    /// Scan configured directories and return documents for changed/new files.
    fn scan_directories(&self) -> Result<Vec<RawDocument>, String> {
        let mut docs = Vec::new();

        for path_str in &self.config.paths {
            let expanded = if path_str.starts_with("~/") {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| "/root".to_string());
                path_str.replacen("~", &home, 1)
            } else {
                path_str.clone()
            };

            let base = PathBuf::from(&expanded);
            if !base.exists() {
                continue;
            }

            self.scan_dir(&base, &mut docs)?;
        }

        Ok(docs)
    }

    fn scan_dir(&self, dir: &PathBuf, docs: &mut Vec<RawDocument>) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("Cannot read dir {:?}: {}", dir, e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Dir entry error: {}", e))?;
            let path = entry.path();

            if path.is_dir() {
                // Skip hidden dirs and common non-content dirs
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                self.scan_dir(&path, docs)?;
                continue;
            }

            if !path.is_file() {
                continue;
            }

            // Filter by extension
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let ext_with_dot = format!(".{}", ext);
            if !self.config.extensions.iter().any(|e| e == &ext_with_dot) {
                continue;
            }

            // Read file content
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue, // skip binary/unreadable files
            };

            // Skip if unchanged
            let hash = Self::content_hash(&content);
            {
                let mut hashes = self.file_hashes.lock().expect("file_hashes mutex poisoned");
                if hashes.get(&path) == Some(&hash) {
                    continue;
                }
                hashes.insert(path.clone(), hash);
            }

            // Generate document
            let rel_path = path.to_string_lossy().to_string();
            let key = rel_path.replace(['/', '\\'], "-");
            let body = serde_json::json!({
                "path": rel_path,
                "content": content,
            });

            docs.push(RawDocument {
                key,
                category: "file".to_string(),
                body_json: body.to_string(),
                tags: vec![format!("ext:{}", ext)],
            });
        }

        Ok(())
    }
}

impl Connector for FileWatcher {
    fn name(&self) -> &str {
        "file_watcher"
    }

    fn fetch(&self) -> Result<Vec<RawDocument>, String> {
        if !self.config.enabled {
            return Err("File watcher connector is not enabled".to_string());
        }
        self.scan_directories()
    }

    fn last_sync(&self) -> &AtomicI64 {
        &self.last_sync
    }
}
