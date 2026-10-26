//! `ReceiptStore` im Ausgabeverzeichnis des Test-Hooks:
//! `<out>/<commit>/<receipt-id-hex>.json` und `<out>/keys.json`, mit der
//! Konfliktregel aus S1 (byteidentisch: `AlreadyExists`, sonst `Conflict`).

use std::path::PathBuf;

use minds_witness_core::{
    CommitId, HashStr, PublicKeyEntry, PutResult, ReceiptKey, ReceiptStore, StoreError,
};

/// Ablage in einem Verzeichnis.
#[derive(Debug)]
pub struct DirReceiptStore {
    root: PathBuf,
}

impl DirReceiptStore {
    /// Ein Store unter `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, key: &ReceiptKey) -> PathBuf {
        self.root
            .join(key.commit.as_str())
            .join(format!("{}.json", &key.id.as_str()["blake3:".len()..]))
    }

    fn keys_path(&self) -> PathBuf {
        self.root.join("keys.json")
    }
}

fn io(_: std::io::Error) -> StoreError {
    StoreError::Internal("io".to_owned())
}

impl ReceiptStore for DirReceiptStore {
    fn put(&mut self, key: &ReceiptKey, bytes: &[u8]) -> Result<PutResult, StoreError> {
        let path = self.path(key);
        if let Ok(existing) = std::fs::read(&path) {
            return if existing == bytes {
                Ok(PutResult::AlreadyExists)
            } else {
                Err(StoreError::Conflict(key.id.as_str().to_owned()))
            };
        }
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| StoreError::Internal("path".to_owned()))?,
        )
        .map_err(io)?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io)?;
        f.write_all(bytes).map_err(io)?;
        Ok(PutResult::Stored)
    }

    fn get(&self, key: &ReceiptKey) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(self.path(key)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }

    fn list(&self, commit: &CommitId) -> Result<Vec<ReceiptKey>, StoreError> {
        let dir = self.root.join(commit.as_str());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for e in entries {
            let name = e.map_err(io)?.file_name();
            let Some(hex) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if let Ok(id) = HashStr::parse(&format!("blake3:{hex}")) {
                out.push(ReceiptKey {
                    commit: commit.clone(),
                    id,
                });
            }
        }
        out.sort();
        Ok(out)
    }

    fn public_keys(&self) -> Result<Vec<PublicKeyEntry>, StoreError> {
        match std::fs::read(self.keys_path()) {
            Ok(b) => {
                serde_json::from_slice(&b).map_err(|_| StoreError::Internal("keys".to_owned()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io(e)),
        }
    }

    fn register_public_key(&mut self, entry: &PublicKeyEntry) -> Result<(), StoreError> {
        let mut keys = self.public_keys()?;
        let same: Vec<&PublicKeyEntry> = keys.iter().filter(|k| k.key == entry.key).collect();
        if same.iter().any(|k| k.valid_from == entry.valid_from) {
            return Ok(());
        }
        if !same.is_empty() {
            return Err(StoreError::Conflict(entry.key.as_str().to_owned()));
        }
        keys.push(entry.clone());
        std::fs::create_dir_all(&self.root).map_err(io)?;
        let bytes =
            serde_json::to_vec(&keys).map_err(|_| StoreError::Internal("keys".to_owned()))?;
        std::fs::write(self.keys_path(), bytes).map_err(io)
    }
}
