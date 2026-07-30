//! Wo eine Session im Baum liegt.
//!
//! ```text
//! sessions/b3/<64 Hex-Zeichen>.json
//! ```
//!
//! Ein Pfad, eine Funktion hin ([`path_of`]) und eine zurück ([`id_of_path`]) —
//! mehr ist das Layout nicht. Es steht trotzdem in einem eigenen Modul, weil es
//! die **einzige Zusage ist, die beide Backends teilen** (Plan: „identischer
//! Baum, nur in einem separaten Repo"). Der `ChildRepoStore` unterscheidet sich
//! im Repo-Handle; hier unterscheidet er sich nicht.
//!
//! # Die drei Bestandteile
//!
//! - **`sessions/`** — Platz für Nachbarn. Der Reader-Index (`index.json`) liegt
//!   später daneben, nicht dazwischen.
//! - **`b3/`** — der Hash-Algorithmus, dasselbe `b3` wie im Präfix der Textform.
//!   Käme je ein zweiter dazu, bekäme er ein eigenes Verzeichnis, und alte
//!   Sessions blieben liegen, wo sie sind. Ein Algorithmus-Wechsel ist damit
//!   eine Ergänzung, keine Migration.
//! - **`.json`** — damit `git show` und jedes andere Werkzeug sofort richtig
//!   liegt. Der Store hält kein Eigenformat.
//!
//! # Flach, nicht gefächert
//!
//! Alle Sessions liegen in *einem* Verzeichnis, ohne Fanout-Ebene
//! (`b3/ab/cdef…`). Das ist die Vorgabe aus dem Plan und für die Größenordnung
//! richtig, um die es geht: Bei ein paar tausend Sessions ist der Baum ein
//! Objekt von einigen hundert Kilobyte, das Git bei jedem `put` neu schreibt —
//! dieselben Kosten, die jedes Git-Verzeichnis dieser Größe hat. Sollte das je
//! stören, ist ein Fanout eine **Layout-Änderung**: sichtbar, für beide Backends
//! gleichzeitig und mit einem Umzug der Bestandsdaten. Kein Detail, das man
//! nebenbei dreht — deshalb steht die Entscheidung hier und nicht im Store.
//!
//! # Fremde Pfade sind keine Sessions
//!
//! [`id_of_path`] liefert `None` für alles, was nicht exakt diesem Muster folgt
//! — `index.json` ebenso wie `sessions/b3/kaputt.json`. Der Store überspringt
//! solche Einträge beim Auflisten, statt sie zu melden: Nachbarn sind
//! vorgesehen, und was *aussieht* wie eine Session, aber keine ist, gehört in
//! den Bericht von `minds fsck` (M6) und nicht in jede Liste.

use minds_core::{SESSION_ID_PREFIX, SessionId};

/// Verzeichnis, unter dem alle Sessions liegen.
const SESSIONS_DIR: &str = "sessions";

/// Verzeichnis, das den Hash-Algorithmus benennt — das `b3` aus
/// [`SESSION_ID_PREFIX`], ohne den Trennstrich. Ein Test hält beide zusammen.
const ALGORITHM_DIR: &str = "b3";

/// Endung jeder Session-Datei.
const EXTENSION: &str = ".json";

/// Der Pfad, unter dem `id` liegt.
pub(crate) fn path_of(id: SessionId) -> String {
    let text = id.to_string();
    let hex = text
        .strip_prefix(SESSION_ID_PREFIX)
        .expect("die Textform einer SessionId trägt immer ihr Präfix");

    format!("{SESSIONS_DIR}/{ALGORITHM_DIR}/{hex}{EXTENSION}")
}

/// Die ID, die unter `path` liegt — `None`, wenn `path` keine Session ist.
pub(crate) fn id_of_path(path: &str) -> Option<SessionId> {
    let hex = path
        .strip_prefix(SESSIONS_DIR)?
        .strip_prefix('/')?
        .strip_prefix(ALGORITHM_DIR)?
        .strip_prefix('/')?
        .strip_suffix(EXTENSION)?;

    let id: SessionId = format!("{SESSION_ID_PREFIX}{hex}").parse().ok()?;

    // Gegenprobe statt bloßer Syntaxprüfung: `SessionId` liest auch
    // Großschreibung (ein hand-editierter Trailer soll auflösbar bleiben),
    // geschrieben wird aber ausschließlich klein. Ohne diesen Vergleich meldete
    // `list` bei einem Pfad mit `AB…` eine ID, deren `path_of` woanders
    // hinzeigt — und `get` fände unter der gemeldeten ID nichts.
    (path_of(id) == path).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::redacted;

    fn sample_id() -> SessionId {
        redacted("Retry-Test reparieren").session().id().unwrap()
    }

    #[test]
    fn the_algorithm_folder_matches_the_id_prefix() {
        // Beide benennen denselben Algorithmus. Wandert das Präfix, muss das
        // Verzeichnis mitwandern — hier fällt es auf.
        assert_eq!(format!("{ALGORITHM_DIR}-"), SESSION_ID_PREFIX);
    }

    #[test]
    fn the_path_follows_the_documented_layout() {
        let id: SessionId = format!("b3-{}", "ab".repeat(32)).parse().unwrap();

        assert_eq!(path_of(id), format!("sessions/b3/{}.json", "ab".repeat(32)));
    }

    #[test]
    fn path_and_id_round_trip() {
        let id = sample_id();
        assert_eq!(id_of_path(&path_of(id)), Some(id));
    }

    #[test]
    fn neighbours_in_the_tree_are_not_sessions() {
        // `index.json` ist vorgesehen, nicht kaputt — es darf nur nicht als
        // Session gezählt werden.
        for foreign in [
            "index.json",
            "README.md",
            "sessions",
            "sessions/index.json",
            "sessions/b3",
        ] {
            assert_eq!(id_of_path(foreign), None, "{foreign:?} ist keine Session");
        }
    }

    #[test]
    fn a_path_that_only_looks_like_a_session_is_refused() {
        let hex = "ab".repeat(32);

        let wrong = [
            "sessions/b3/kaputt.json".to_owned(),
            format!("sessions/b3/{hex}"),     // ohne Endung
            format!("sessions/b3/{hex}.txt"), // falsche Endung
            format!("sessions/{hex}.json"),   // ohne Algorithmus-Ebene
            format!("sessions/sha256/{hex}.json"),
            format!("sessions/b3/tief/{hex}.json"),
            format!("/sessions/b3/{hex}.json"),         // absolut
            format!("sessions/b3/{}.json", &hex[..62]), // zu kurz
        ];

        for path in wrong {
            assert_eq!(id_of_path(&path), None, "{path:?} ist keine Session");
        }
    }

    #[test]
    fn uppercase_hex_is_not_the_canonical_path() {
        // Lesbar wäre die ID — aber `path_of` zeigte woanders hin, und dann
        // meldete `list` etwas, das `get` nicht findet.
        let upper = format!("sessions/b3/{}.json", "AB".repeat(32));
        assert_eq!(id_of_path(&upper), None);
    }

    #[test]
    fn different_sessions_never_share_a_path() {
        let first = redacted("Fall A").session().id().unwrap();
        let second = redacted("Fall B").session().id().unwrap();

        assert_ne!(path_of(first), path_of(second));
    }
}
