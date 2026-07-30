//! Der Tombstone: was von einer vergessenen Session bleibt.
//!
//! `minds forget` ersetzt die Nutzlast einer Session durch diesen Marker — die
//! content-adressierte Referenz (der Trailer im Commit) bleibt auflösbar, der
//! Inhalt verschwindet aus dem aktuellen Stand des Stores. Das ist die Antwort
//! auf die DSGVO-Löschung, die reines Git strukturell nicht kann: Referenz
//! behalten, Inhalt entfernen.
//!
//! # Append-only bleibt gewahrt
//!
//! Der Tombstone wird als neuer Commit **angehängt** (er überschreibt den Blob im
//! aktuellen Baum), nicht als Objekt gelöscht. Die vollständige Tilgung aus der
//! *Historie* des Refs ist ein separater, schwererer Schritt (History-Rewrite) —
//! hier nicht getan und ehrlich benannt.

/// Das Marker-Feld, an dem ein Tombstone erkannt wird.
const MARKER: &str = "minds_tombstone";

/// Voreingestellter Grund, wenn der Aufrufer keinen nennt.
pub const DEFAULT_REASON: &str = "vergessen";

/// Die Bytes eines Tombstones. Deterministisch — `serde_json::Map` ist nach
/// Schlüsseln sortiert.
pub fn bytes(reason: &str) -> Vec<u8> {
    let value = serde_json::json!({
        MARKER: 1,
        "reason": reason,
    });
    serde_json::to_vec(&value).expect("json! serialisiert immer")
}

/// Der Grund, wenn `bytes` ein Tombstone ist — sonst `None`.
pub fn reason(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value.get(MARKER)?;
    Some(
        value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tombstone_carries_its_reason() {
        let t = bytes("DSGVO-Antrag #42");
        assert_eq!(reason(&t).as_deref(), Some("DSGVO-Antrag #42"));
    }

    #[test]
    fn a_tombstone_is_deterministic() {
        assert_eq!(bytes("x"), bytes("x"));
    }

    #[test]
    fn ordinary_session_json_is_not_a_tombstone() {
        assert_eq!(reason(br#"{"agent":{"name":"x"}}"#), None);
        assert_eq!(reason(b"kein json"), None);
    }
}
