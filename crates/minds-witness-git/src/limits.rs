//! Ressourcengrenzen des Git-Adapters mit den Voreinstellungen aus Spec S2
//! §9. Push-weite Grenzen sind verpflichtend; ihre Überschreitung ist ein
//! technischer Fehler (fail-closed), kein Urteil über Evidence.

use std::time::Duration;

/// Alle Grenzen eines Hook-Aufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterLimits {
    /// Objektgröße, zugleich `alloc_limit_bytes`.
    pub max_object_bytes: u64,
    /// Eltern je Commit.
    pub max_parents: usize,
    /// Baumtiefe.
    pub max_tree_depth: usize,
    /// Geänderte Pfade je Commit.
    pub max_changed_paths: usize,
    /// Ref-Updates je Push.
    pub max_refs_per_push: usize,
    /// Kandidaten je Präfix, die an den Inspektor gehen.
    pub max_binding_candidates: usize,
    /// Refs je Präfix, die für die Auswahl betrachtet werden.
    pub max_binding_enumeration: usize,
    /// Bewertete Commits je Push.
    pub max_total_commits_per_push: u32,
    /// Bindungen je Commit beim Anlegen.
    pub max_bindings_per_commit: usize,
    /// Seals je Bindung.
    pub max_seals_per_binding: usize,
    /// Größe einer Bindung.
    pub max_binding_bytes: u64,
    /// Größe eines Observation-Objekts.
    pub max_observations_bytes: u64,
    /// Gelesene Evidence-Bytes je Push.
    pub max_evidence_bytes_per_push: u64,
    /// `ssh-keygen`-Prozesse je Push.
    pub max_ssh_keygen_processes: u32,
    /// Timeout je `ssh-keygen`-Prozess.
    pub ssh_keygen_timeout: Duration,
    /// Wanduhr-Deadline ab Start; harte Obergrenze.
    pub deadline: Duration,
}

impl Default for AdapterLimits {
    fn default() -> Self {
        Self {
            max_object_bytes: 16 * 1024 * 1024,
            max_parents: 64,
            max_tree_depth: 256,
            max_changed_paths: 10_000,
            max_refs_per_push: 2_000,
            max_binding_candidates: 64,
            max_binding_enumeration: 256,
            max_total_commits_per_push: 500,
            max_bindings_per_commit: 4,
            max_seals_per_binding: 32,
            max_binding_bytes: 64 * 1024,
            max_observations_bytes: 8 * 1024 * 1024,
            max_evidence_bytes_per_push: 512 * 1024 * 1024,
            max_ssh_keygen_processes: 2_048,
            ssh_keygen_timeout: Duration::from_secs(2),
            deadline: Duration::from_secs(60),
        }
    }
}

impl AdapterLimits {
    /// Die Grenzen für `minds_git::quarantine`.
    pub fn repo_limits(&self) -> minds_git::quarantine::Limits {
        minds_git::quarantine::Limits {
            max_object_bytes: self.max_object_bytes,
            max_parents: self.max_parents,
            max_tree_depth: self.max_tree_depth,
            max_changed_paths: self.max_changed_paths,
            max_prefix_refs: self.max_binding_enumeration,
        }
    }
}
