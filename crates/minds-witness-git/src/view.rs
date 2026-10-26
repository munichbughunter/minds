//! Die Objektsicht des Inspektors (Spec S2 §8.1): lesend, für einen
//! `pre_receive`-Aufruf fest (S1 v1.5 V4). Der Git-Adapter implementiert sie
//! über Snapshot und Quarantäne (Spec S2 §7); [`MemoryView`] ist die
//! In-Memory-Referenz, mit der Bindung und Inspektor ohne Git geprüft werden.

use std::collections::{BTreeMap, BTreeSet};

/// Modus eines regulären Blobs.
pub const MODE_BLOB: u32 = 0o100644;
/// Modus einer ausführbaren Datei.
pub const MODE_EXEC: u32 = 0o100755;
/// Modus eines Symlinks.
pub const MODE_LINK: u32 = 0o120000;
/// Modus eines Gitlinks.
pub const MODE_GITLINK: u32 = 0o160000;
/// Modus eines Baums.
pub const MODE_TREE: u32 = 0o040000;

/// Ein Baumeintrag: Name, Modus, Objekt-Id und Größe (bei Blobs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Name des Eintrags (eine Pfadkomponente) bzw. bei
    /// [`EvidenceView::entry`] der volle Pfad.
    pub name: String,
    /// Git-Modus (`0o100644` …).
    pub mode: u32,
    /// Objekt-Id in Hex.
    pub oid: String,
    /// Größe des Objekts in Bytes.
    pub size: u64,
}

/// Technischer Fehler der Sicht; im Kern `Internal`. Der Text ist fest und
/// trägt keine Objektinhalte.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("object view failed: {0}")]
pub struct ViewError(pub &'static str);

/// Lesesicht auf Refs, Commits, Bäume und Blobs eines Pushes.
pub trait EvidenceView {
    /// Ziel des Refs `name` (ein Commit): zuerst aus den Updates des Pushes,
    /// dann aus dem Vorzustand; die erste Lesung gilt (memoisiert).
    fn resolve_ref(&self, name: &str) -> Result<Option<String>, ViewError>;
    /// Die Einträge der obersten Ebene des Baums von `commit`.
    fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, ViewError>;
    /// Der Eintrag unter `path` (`/`-getrennt) im Baum von `commit`.
    fn entry(&self, commit: &str, path: &str) -> Result<Option<TreeEntry>, ViewError>;
    /// Die Eltern von `commit`.
    fn parents(&self, commit: &str) -> Result<Vec<String>, ViewError>;
    /// Geänderte Pfade nach Spec S2 §7.4 (Modus zählt mit).
    fn changed_paths(&self, commit: &str) -> Result<BTreeSet<String>, ViewError>;
    /// Bytes des Blobs `oid`, oder `None`, wenn er größer als `max` ist.
    fn blob(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, ViewError>;
}

/// In-Memory-Sicht für Tests und als Referenz des Vertrags. Objekt-Ids sind
/// die ersten 40 Hex-Zeichen von blake3 über Typ und Inhalt.
#[derive(Debug, Default, Clone)]
pub struct MemoryView {
    blobs: BTreeMap<String, Vec<u8>>,
    commits: BTreeMap<String, MemCommit>,
    refs: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct MemCommit {
    parents: Vec<String>,
    /// Voller Pfad → (Modus, Oid). Bäume sind implizit.
    files: BTreeMap<String, (u32, String)>,
}

fn oid(kind: &str, parts: &[&[u8]]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(kind.as_bytes());
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    hex::encode(&h.finalize().as_bytes()[..20])
}

impl MemoryView {
    /// Eine leere Sicht.
    pub fn new() -> Self {
        Self::default()
    }

    /// Legt einen Blob ab und liefert seine Oid.
    pub fn add_blob(&mut self, bytes: &[u8]) -> String {
        let id = oid("blob", &[bytes]);
        self.blobs.insert(id.clone(), bytes.to_vec());
        id
    }

    /// Legt einen Commit mit `parents` und Dateien (voller Pfad, Modus,
    /// Inhalt) an und liefert seine Id. `salt` unterscheidet sonst gleiche
    /// Commits.
    pub fn add_commit(
        &mut self,
        parents: &[String],
        files: &[(&str, u32, &[u8])],
        salt: &str,
    ) -> String {
        let mut map = BTreeMap::new();
        let mut parts: Vec<Vec<u8>> = vec![salt.as_bytes().to_vec()];
        for p in parents {
            parts.push(p.as_bytes().to_vec());
        }
        for (path, mode, bytes) in files {
            let blob = self.add_blob(bytes);
            parts.push(format!("{path}\0{mode:o}\0{blob}").into_bytes());
            map.insert((*path).to_owned(), (*mode, blob));
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let id = oid("commit", &refs);
        self.commits.insert(
            id.clone(),
            MemCommit {
                parents: parents.to_vec(),
                files: map,
            },
        );
        id
    }

    /// Setzt `name` auf `target`.
    pub fn set_ref(&mut self, name: &str, target: &str) {
        self.refs.insert(name.to_owned(), target.to_owned());
    }

    /// Alle Refs mit diesem Präfix, aufsteigend nach Name.
    pub fn refs_with_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        self.refs
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn commit_of(&self, id: &str) -> Result<&MemCommit, ViewError> {
        self.commits.get(id).ok_or(ViewError("commit not found"))
    }
}

impl EvidenceView for MemoryView {
    fn resolve_ref(&self, name: &str) -> Result<Option<String>, ViewError> {
        Ok(self.refs.get(name).cloned())
    }

    fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, ViewError> {
        let c = self.commit_of(commit)?;
        let mut out: BTreeMap<String, TreeEntry> = BTreeMap::new();
        for (path, (mode, blob)) in &c.files {
            match path.split_once('/') {
                None => {
                    out.insert(
                        path.clone(),
                        TreeEntry {
                            name: path.clone(),
                            mode: *mode,
                            oid: blob.clone(),
                            size: self.blobs.get(blob).map_or(0, |b| b.len() as u64),
                        },
                    );
                }
                Some((dir, _)) => {
                    out.entry(dir.to_owned()).or_insert(TreeEntry {
                        name: dir.to_owned(),
                        mode: MODE_TREE,
                        oid: oid("tree", &[dir.as_bytes(), commit.as_bytes()]),
                        size: 0,
                    });
                }
            }
        }
        Ok(out.into_values().collect())
    }

    fn entry(&self, commit: &str, path: &str) -> Result<Option<TreeEntry>, ViewError> {
        let c = self.commit_of(commit)?;
        Ok(c.files.get(path).map(|(mode, blob)| TreeEntry {
            name: path.to_owned(),
            mode: *mode,
            oid: blob.clone(),
            size: self.blobs.get(blob).map_or(0, |b| b.len() as u64),
        }))
    }

    fn parents(&self, commit: &str) -> Result<Vec<String>, ViewError> {
        Ok(self.commit_of(commit)?.parents.clone())
    }

    fn changed_paths(&self, commit: &str) -> Result<BTreeSet<String>, ViewError> {
        let c = self.commit_of(commit)?;
        let parents: Vec<&MemCommit> = c
            .parents
            .iter()
            .map(|p| self.commit_of(p))
            .collect::<Result<_, _>>()?;
        let mut all: BTreeSet<&String> = c.files.keys().collect();
        for p in &parents {
            all.extend(p.files.keys());
        }
        Ok(all
            .into_iter()
            .filter(|path| {
                let here = c.files.get(*path);
                parents.is_empty() || parents.iter().all(|p| p.files.get(*path) != here)
            })
            .cloned()
            .collect())
    }

    fn blob(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, ViewError> {
        let bytes = self.blobs.get(oid).ok_or(ViewError("blob not found"))?;
        Ok((bytes.len() as u64 <= max).then(|| bytes.clone()))
    }
}
