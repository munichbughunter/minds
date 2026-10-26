//! Das Format `minds-commit-binding-v1` (Spec S2 §4): ein vom lokalen,
//! getrennten Witness signiertes RFC-8785-Dokument, das einen Git-Commit
//! eines Projekts an Seals bindet. Das Modul liest strikt (unbekannte
//! Felder, abweichende Formen und nicht kanonische Bytes sind unlesbar)
//! und schreibt kanonisch. Es prüft keine Signatur und kein Vertrauen; das
//! tut [`crate::trust`].

use serde::{Deserialize, Serialize};

/// Wert des Feldes `kind`.
pub const KIND: &str = "commit_binding";
/// Wert des Feldes `schema`.
pub const SCHEMA: u64 = 1;
/// Voreinstellung für die Höchstzahl der Seals je Bindung (Spec S2 §9).
pub const DEFAULT_MAX_SEALS: usize = 32;
/// Präfix der Binding-Refs: `refs/minds/bindings/<commit-hex>/<id-hex>`.
pub const REF_PREFIX: &str = "refs/minds/bindings/";

/// Warum Bytes keine lesbare Bindung sind. Die Texte sind fest und tragen
/// nie Inhalte der Bindung.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    /// Kein JSON-Objekt der erwarteten Felder (auch: unbekanntes oder
    /// doppeltes Feld, falscher Typ).
    #[error("binding: kein gültiges Dokument")]
    Json,
    /// `kind` ist nicht `commit_binding`.
    #[error("binding: falsches kind")]
    Kind,
    /// `schema` ist nicht `1`.
    #[error("binding: falsches schema")]
    Schema,
    /// `commit` ist keine volle Commit-Id in Kleinbuchstaben-Hex.
    #[error("binding: commit ungültig")]
    Commit,
    /// `key` ist kein öffentlicher SSH-Schlüssel der Form `<typ> <base64>`.
    #[error("binding: key ungültig")]
    Key,
    /// `project` ist keine Projekt-Identität `<instanz>/project-<id>`.
    #[error("binding: project ungültig")]
    Project,
    /// `seals` ist leer, zu lang, unsortiert, doppelt oder enthält eine
    /// Id, die nicht `b3-<64 hex>` ist.
    #[error("binding: seals ungültig")]
    Seals,
    /// Die Bytes sind nicht exakt die kanonische Serialisierung.
    #[error("binding: nicht kanonisch")]
    NotCanonical,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    commit: String,
    key: String,
    kind: String,
    project: String,
    schema: u64,
    seals: Vec<String>,
}

/// Eine gelesene, formal gültige Commit-Bindung. Konstruierbar nur über
/// [`CommitBinding::new`] oder [`CommitBinding::parse`]; beide prüfen jede
/// Feldregel aus Spec S2 §4.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitBinding {
    commit: String,
    key: String,
    project: String,
    seals: Vec<String>,
}

impl CommitBinding {
    /// Baut eine Bindung und prüft jede Feldregel; `seals` werden nicht
    /// sortiert, sondern müssen schon aufsteigend und ohne Doppelte sein.
    pub fn new(
        commit: &str,
        key: &str,
        project: &str,
        seals: Vec<String>,
        max_seals: usize,
    ) -> Result<Self, BindingError> {
        if !is_commit_id(commit) {
            return Err(BindingError::Commit);
        }
        if !is_public_key(key) {
            return Err(BindingError::Key);
        }
        if !is_project_id(project) {
            return Err(BindingError::Project);
        }
        if seals.is_empty()
            || seals.len() > max_seals
            || !seals.iter().all(|s| is_seal_id(s))
            || !seals.windows(2).all(|w| w[0] < w[1])
        {
            return Err(BindingError::Seals);
        }
        Ok(Self {
            commit: commit.to_owned(),
            key: key.to_owned(),
            project: project.to_owned(),
            seals,
        })
    }

    /// Liest gespeicherte Bytes strikt: jede Feldregel, keine unbekannten
    /// Felder, und die Bytes müssen exakt der kanonischen Form entsprechen.
    pub fn parse(bytes: &[u8], max_seals: usize) -> Result<Self, BindingError> {
        let doc: Doc = serde_json::from_slice(bytes).map_err(|_| BindingError::Json)?;
        if doc.kind != KIND {
            return Err(BindingError::Kind);
        }
        if doc.schema != SCHEMA {
            return Err(BindingError::Schema);
        }
        let binding = Self::new(&doc.commit, &doc.key, &doc.project, doc.seals, max_seals)?;
        if binding.to_bytes() != bytes {
            return Err(BindingError::NotCanonical);
        }
        Ok(binding)
    }

    /// Die kanonischen Bytes (RFC 8785).
    pub fn to_bytes(&self) -> Vec<u8> {
        let doc = Doc {
            commit: self.commit.clone(),
            key: self.key.clone(),
            kind: KIND.to_owned(),
            project: self.project.clone(),
            schema: SCHEMA,
            seals: self.seals.clone(),
        };
        // Spec S2 §4.1: Das Dokument besteht nur aus Strings, einer
        // Ganzzahl und einer String-Liste; die Serialisierung kann nicht
        // scheitern.
        minds_core::to_canonical_json(&doc).expect("Binding ist immer serialisierbar")
    }

    /// `binding_id = "b3-" + hex(blake3(kanonische Bytes))`.
    pub fn id(&self) -> String {
        id_of_bytes(&self.to_bytes())
    }

    /// Die gebundene Commit-Id.
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// Der öffentliche Schlüssel des Signierers (`<typ> <base64>`).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Die Projekt-Identität.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Die gebundenen Seal-Ids, aufsteigend.
    pub fn seals(&self) -> &[String] {
        &self.seals
    }

    /// Der Ref-Name, unter dem diese Bindung liegt.
    pub fn ref_name(&self) -> String {
        format!("{REF_PREFIX}{}/{}", self.commit, &self.id()[3..])
    }
}

/// `"b3-" + hex(blake3(bytes))` über gespeicherte Bytes.
pub fn id_of_bytes(bytes: &[u8]) -> String {
    format!("b3-{}", hex::encode(blake3::hash(bytes).as_bytes()))
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Volle Commit-Id: 40 oder 64 Zeichen Kleinbuchstaben-Hex.
pub fn is_commit_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && is_lower_hex(s)
}

/// Seal- oder Binding-Id `b3-<64 hex>` in Kleinbuchstaben.
pub fn is_seal_id(s: &str) -> bool {
    s.strip_prefix("b3-")
        .is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

/// Öffentlicher Schlüssel `<typ> <base64>`: genau ein Leerzeichen, kein
/// Kommentar, ein unterstützter Typ, und der Typ im Blob stimmt.
pub fn is_public_key(s: &str) -> bool {
    let Some((kind, blob)) = s.split_once(' ') else {
        return false;
    };
    let supported = matches!(
        kind,
        "ssh-ed25519" | "ssh-rsa" | "sk-ssh-ed25519@openssh.com"
    ) || kind.starts_with("ecdsa-sha2-nistp")
        || kind == "sk-ecdsa-sha2-nistp256@openssh.com";
    supported
        && !blob.is_empty()
        && blob
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        && minds_attest::public_key_blob(s).is_some()
}

/// Projekt-Identität `<instanz>/project-<id>` (Spec S2 §4.2): Instanz aus
/// `[a-z0-9.-]`, nicht leer, optional `:<port>`; Id dezimal ≥ 1 ohne
/// führende Null; sonst nichts.
pub fn is_project_id(s: &str) -> bool {
    let Some((instance, project)) = s.split_once('/') else {
        return false;
    };
    let (host, port) = match instance.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (instance, None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'));
    let port_ok = port.is_none_or(|p| {
        !p.is_empty()
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && !p.starts_with('0')
            && p.parse::<u32>().is_ok_and(|n| n <= 65_535)
    });
    let id_ok = project.strip_prefix("project-").is_some_and(|id| {
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !id.starts_with('0')
    });
    host_ok && port_ok && id_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_ids() {
        for ok in [
            "git.test/project-42",
            "gitlab.example.com/project-123",
            "gitlab.example.com:8443/project-1",
        ] {
            assert!(is_project_id(ok), "{ok}");
        }
        for bad in [
            "Git.test/project-42",
            "git.test/project-042",
            "git.test/project-0",
            "git.test/group/repo",
            "git.test/project-42/x",
            "/project-42",
            "git.test/project-",
            "git.test:/project-1",
            "git.test:0443/project-1",
            "git.test:99999/project-1",
            "git test/project-1",
        ] {
            assert!(!is_project_id(bad), "{bad}");
        }
    }
}
