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
    /// `git receive-pack` ohne Gitaly (S1 v1.6, Spec S3 ADR-S3-004).
    Git,
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

/// Höchstlänge eines Ref-Namens in Meldungen an den Client (Zeichen,
/// ohne die Kürzungsmarke `...`).
pub const MAX_REF_DISPLAY: usize = 200;

/// Ref-Name für Meldungen an den Client (Ausgabeübergang): Druckbares
/// ASCII ohne Leerzeichen bleibt, jedes andere Byte erscheint als `\xNN`,
/// ein Backslash als `\\`. Länger als [`MAX_REF_DISPLAY`] wird gekürzt und
/// mit `...` markiert; ein leerer Name erscheint als `<ref>`. Die Ausgabe
/// enthält damit nie Steuerzeichen (auch keine 8-Bit-Steuerzeichen wie
/// U+009B) und bleibt begrenzt. Ereignisfelder tragen den Namen
/// unverändert; sie gehen nur ins Log.
pub fn display_ref(name: &str) -> String {
    if name.is_empty() {
        return "<ref>".to_owned();
    }
    let mut out = String::new();
    for b in name.bytes() {
        let token = match b {
            b'\\' => "\\\\".to_owned(),
            0x21..=0x7e => char::from(b).to_string(),
            _ => format!("\\x{b:02x}"),
        };
        if out.len() + token.len() > MAX_REF_DISPLAY {
            out.push_str("...");
            return out;
        }
        out.push_str(&token);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_ref_escapes_and_bounds() {
        assert_eq!(display_ref("refs/heads/main"), "refs/heads/main");
        assert_eq!(display_ref(""), "<ref>");
        assert_eq!(display_ref("refs/heads/\u{1b}[31m"), "refs/heads/\\x1b[31m");
        assert_eq!(
            display_ref("refs/heads/x\u{9b}y"),
            "refs/heads/x\\xc2\\x9by"
        );
        assert_eq!(display_ref("refs/heads/a b\\c"), "refs/heads/a\\x20b\\\\c");
        let long = format!("refs/heads/{}", "a".repeat(500));
        let shown = display_ref(&long);
        assert_eq!(shown.len(), MAX_REF_DISPLAY + 3);
        assert!(shown.ends_with("..."));
        // Eine Escape-Sequenz wird nie zerschnitten.
        let edge = format!("{}\u{1b}", "a".repeat(MAX_REF_DISPLAY - 2));
        assert_eq!(
            display_ref(&edge),
            format!("{}...", "a".repeat(MAX_REF_DISPLAY - 2))
        );
        for s in [&long, &edge, "refs/heads/\u{9b}\u{7}\n"] {
            assert!(display_ref(s).bytes().all(|b| (0x20..0x7f).contains(&b)));
        }
    }
}
