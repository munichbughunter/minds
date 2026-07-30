//! Kanonische JSON-Serialisierung für Content-Adressierung.
//!
//! Die `SessionId` ist `blake3(canonical_json(session))`. Damit dieser Hash
//! eine stabile, verifizierbare Identität ist, muss die Byte-Repräsentation
//! vollständig deterministisch sein — unabhängig von Struct-Feldreihenfolge,
//! Map-Iterationsreihenfolge, Plattform und serde-Version.
//!
//! Umgesetzt ist die JSON-Canonicalization nach RFC 8785 (JCS), mit einer
//! bewusst erzwungenen Einschränkung: Das Datenmodell darf keine
//! Gleitkommazahlen und keine Ganzzahlen außerhalb des sicher darstellbaren
//! Bereichs (|n| > 2^53−1) enthalten. Das Minds-Envelope hält ausschließlich
//! Ganzzahlen (Token-Zähler, Zeilen-Zähler) und Strings — der einzige wirklich
//! schwierige Teil von JCS (Zahlformatierung nach ECMAScript / IEEE-754-double)
//! entfällt damit und wird abgelehnt statt approximiert.
//!
//! Warum RFC 8785 und keine Haus-Kanonisierung: Der Wert des Records ist seine
//! Nachweisbarkeit. Ein dritter Leser (Python, Go, ein Auditor) muss denselben
//! Hash reproduzieren können. JCS ist ein Standard mit Implementierungen in
//! vielen Sprachen; eine Eigenerfindung wäre nicht extern verifizierbar. Weil
//! wir uns strikt auf den sicheren Ganzzahlbereich beschränken, ist unsere
//! Ausgabe ein echter Teilbereich von JCS: Jede konforme Implementierung bildet
//! bitgleich dieselben Bytes — auch für große Zähler.

use serde::Serialize;
use serde_json::Value;

/// Fehler bei der Kanonisierung.
#[derive(Debug, thiserror::Error)]
pub enum CanonError {
    /// Der Wert ließ sich nicht nach JSON serialisieren.
    #[error("Wert konnte nicht nach JSON serialisiert werden: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Kanonisches JSON in Minds erlaubt nur Ganzzahlen. Gleitkommazahlen
    /// werden abgelehnt, weil ihre deterministische Formatierung (ECMAScript
    /// Number::toString) nicht Teil des Vertrags ist.
    #[error("kanonisches JSON erlaubt keine Gleitkommazahlen")]
    NonIntegerNumber,

    /// Ganzzahl außerhalb des JCS-sicheren Bereichs (|n| > 2^53−1).
    /// Jenseits davon weicht die exakte Dezimaldarstellung von der
    /// double-basierten Formatierung ab, die RFC 8785 vorschreibt — ein
    /// JCS-Leser in einer anderen Sprache würde einen anderen Wert bilden und
    /// damit einen anderen Hash. Fail-closed statt still von JCS abweichen.
    #[error("Ganzzahl außerhalb des JCS-sicheren Bereichs (|n| > 2^53-1): {0}")]
    IntegerOutOfSafeRange(i128),
}

/// Serialisiert `value` deterministisch nach RFC 8785 und gibt die Bytes zurück.
///
/// Das ist die Eingabe für den Hash. Reihenfolge der Objekt-Schlüssel, Zahlen-
/// und String-Formatierung sind fixiert; identischer Inhalt ergibt identische
/// Bytes.
pub fn to_canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonError> {
    let value = serde_json::to_value(value)?;
    let mut out = Vec::new();
    write_value(&value, &mut out)?;
    Ok(out)
}

/// Wie [`to_canonical_json`], aber als `String`. Die Ausgabe ist immer gültiges
/// UTF-8 (nur ASCII-Strukturbytes plus gültiges UTF-8 aus String-Werten).
pub fn to_canonical_string<T: Serialize>(value: &T) -> Result<String, CanonError> {
    let bytes = to_canonical_json(value)?;
    Ok(String::from_utf8(bytes).expect("kanonische Ausgabe ist immer gültiges UTF-8"))
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<(), CanonError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => write_number(n, out)?,
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Schlüssel explizit sortieren — nicht auf die interne Ordnung der
            // Map verlassen. serde_json nutzt je nach `preserve_order`-Feature
            // BTreeMap oder IndexMap; explizites Sortieren macht uns dagegen
            // immun. Sortiert wird nach UTF-16-Code-Units gemäß RFC 8785.
            let mut entries: Vec<(&str, &Value)> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            entries.sort_unstable_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));

            out.push(b'{');
            for (i, &(key, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(val, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn write_number(n: &serde_json::Number, out: &mut Vec<u8>) -> Result<(), CanonError> {
    // JCS behandelt Zahlen als IEEE-754-doubles. Exakt und damit sprach-
    // übergreifend reproduzierbar sind nur Ganzzahlen bis 2^53−1
    // (Number.MAX_SAFE_INTEGER). In diesem Bereich ist die Display-Form einer
    // i64/u64 identisch mit der JCS-Formatierung (kein '+', keine führenden
    // Nullen, kein Exponent). Darüber hinaus brechen wir ab, statt still von
    // JCS abzuweichen. Gleitkommazahlen (`as_i64`/`as_u64` == None) ebenso.
    const MAX_SAFE: i128 = (1_i128 << 53) - 1;

    let value: i128 = if let Some(i) = n.as_i64() {
        i128::from(i)
    } else if let Some(u) = n.as_u64() {
        i128::from(u)
    } else {
        return Err(CanonError::NonIntegerNumber);
    };

    if value.abs() > MAX_SAFE {
        return Err(CanonError::IntegerOutOfSafeRange(value));
    }

    out.extend_from_slice(value.to_string().as_bytes());
    Ok(())
}

/// String-Serialisierung nach RFC 8785: minimales Escaping, Kleinbuchstaben in
/// `\u`-Escapes, alles außer Steuerzeichen / `"` / `\` unverändert als UTF-8.
fn write_string(s: &str, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{0008}' => out.extend_from_slice(b"\\b"),
            '\u{0009}' => out.extend_from_slice(b"\\t"),
            '\u{000A}' => out.extend_from_slice(b"\\n"),
            '\u{000C}' => out.extend_from_slice(b"\\f"),
            '\u{000D}' => out.extend_from_slice(b"\\r"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.extend_from_slice(b"\\u00");
                out.push(HEX[((code >> 4) & 0xf) as usize]);
                out.push(HEX[(code & 0xf) as usize]);
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;

    #[test]
    fn object_keys_are_sorted() {
        let out = to_canonical_string(&json!({"b": 1, "a": 2, "c": 3})).unwrap();
        assert_eq!(out, r#"{"a":2,"b":1,"c":3}"#);
    }

    #[test]
    fn struct_field_order_does_not_matter() {
        // serde serialisiert Felder in Deklarationsreihenfolge (z, a, m); die
        // Kanonisierung sortiert um. Beweist Unabhängigkeit von der Feldfolge.
        #[derive(Serialize)]
        struct Unsorted {
            z: u32,
            a: u32,
            m: u32,
        }
        let out = to_canonical_string(&Unsorted { z: 1, a: 2, m: 3 }).unwrap();
        assert_eq!(out, r#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn nested_objects_are_sorted() {
        let out = to_canonical_string(&json!({
            "outer": {"y": 1, "x": 2},
            "arr": [{"b": 1, "a": 2}]
        }))
        .unwrap();
        assert_eq!(out, r#"{"arr":[{"a":2,"b":1}],"outer":{"x":2,"y":1}}"#);
    }

    #[test]
    fn no_insignificant_whitespace() {
        let out = to_canonical_string(&json!({"k": [1, 2, 3]})).unwrap();
        assert_eq!(out, r#"{"k":[1,2,3]}"#);
    }

    #[test]
    fn empty_containers() {
        assert_eq!(to_canonical_string(&json!({})).unwrap(), "{}");
        assert_eq!(to_canonical_string(&json!([])).unwrap(), "[]");
    }

    #[test]
    fn integers_pass_through() {
        let out =
            to_canonical_string(&json!({"neg": -7, "big": 9_000_000_000_u64, "zero": 0})).unwrap();
        assert_eq!(out, r#"{"big":9000000000,"neg":-7,"zero":0}"#);
    }

    #[test]
    fn floats_are_rejected() {
        let err = to_canonical_json(&json!({"ratio": 0.73})).unwrap_err();
        assert!(matches!(err, CanonError::NonIntegerNumber));
    }

    #[test]
    fn safe_integer_boundary_passes() {
        // 2^53−1 = Number.MAX_SAFE_INTEGER — noch exakt, also erlaubt.
        let out = to_canonical_string(&json!({ "n": 9_007_199_254_740_991_i64 })).unwrap();
        assert_eq!(out, r#"{"n":9007199254740991}"#);
    }

    #[test]
    fn integers_beyond_safe_range_are_rejected() {
        // 2^53 = erster Wert, ab dem exakte Dezimal- und double-Formatierung
        // divergieren können. Fail-closed statt still von JCS abweichen.
        let err = to_canonical_json(&json!({ "n": 9_007_199_254_740_992_u64 })).unwrap_err();
        assert!(matches!(err, CanonError::IntegerOutOfSafeRange(_)));
    }

    #[test]
    fn string_escaping_follows_rfc8785() {
        // Quote, Backslash, benannte Steuerzeichen, ein unbenanntes Steuerzeichen
        // (VT, 0x0B → \u000b), und Nicht-ASCII bleibt unverändert (UTF-8).
        let out = to_canonical_string(&json!("a\"b\\c\n\td\u{000b}e ü 🦀")).unwrap();
        assert_eq!(out, "\"a\\\"b\\\\c\\n\\td\\u000be ü 🦀\"");
    }

    #[test]
    fn output_is_deterministic() {
        let a = to_canonical_json(&json!({"b": 1, "a": [3, 2, 1]})).unwrap();
        let b = to_canonical_json(&json!({"a": [3, 2, 1], "b": 1})).unwrap();
        assert_eq!(a, b);
    }
}
