//! Push-Kontext: was der Host über einen Push weiß, bevor der Kern ihn
//! prüft. Das Modul enthält nur Datentypen; die Validierung des Kontexts
//! (gleiche Id-Länge) übernimmt die Pre-Phase. Woher die Daten stammen
//! (stdin, Gitaly, scion), ist Sache des Adapters.

use serde::{Deserialize, Serialize};

use crate::ids::{CommitId, Timestamp};

/// Eine einzelne Ref-Bewegung innerhalb eines Pushes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    /// Alter Stand; Null-Id bedeutet Ref-Anlage.
    pub old: CommitId,
    /// Neuer Stand; Null-Id bedeutet Ref-Löschung.
    pub new: CommitId,
    /// Vollständiger Ref-Name, etwa `refs/heads/main`.
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// Die Person bzw. das Konto, das den Push ausgelöst hat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pusher {
    /// Stabile Konto-Id beim Host (bei `PusherMode::Hashed` ein Hash-String).
    pub id: String,
    /// Anzeigename beim Host (bei `PusherMode::Hashed` ein Hash-String).
    pub username: String,
}

/// Art des Hosts, der den Push annimmt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerKind {
    /// GitLab mit Gitaly.
    Gitaly,
    /// scion als Host.
    Scion,
    /// In-Memory-Host für Tests.
    Memory,
}

/// Angaben zum Server, der den Befund erstellt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Hostname des Servers.
    pub host: String,
    /// Art des Hosts.
    pub kind: ServerKind,
    /// Serverzeit des Pushes; Form garantiert, Plausibilität nicht.
    pub time: Timestamp,
}

/// Alles, was der Kern über einen Push erfährt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushContext {
    /// Repository-Kennung beim Host, etwa `acme/payments`.
    pub repo: String,
    /// Ref-Bewegungen des Pushes in Host-Reihenfolge.
    pub updates: Vec<RefUpdate>,
    /// Auslöser des Pushes.
    pub pusher: Pusher,
    /// Serverangaben.
    pub server: ServerInfo,
}
