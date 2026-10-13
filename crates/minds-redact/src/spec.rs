//! Die fail-closed-Garantie für das Soll — dieselbe Bauform wie beim
//! Intent-Anker ([`crate::RedactedIntent`]): [`RedactedSpec`] hat keinen
//! öffentlichen Konstruktor, `minds-store` nimmt nur ihn entgegen.
//!
//! Das Soll wird nicht umgeschrieben, sondern **geprüft**: Es wird signiert,
//! ein Platzhalter darin wäre ein anderes Soll als das, das der Mensch
//! gelesen hat. Findet die Policy etwas — in einem einzelnen Wert (unescapt,
//! auch mehrzeilig) oder in der ganzen kanonischen Form —, gibt es nichts
//! abzulegen.

use minds_core::ContentHash;
use minds_core::spec::{SignedSpec, Spec};

use crate::pipeline::RedactionPipeline;
use crate::session::{Field, RedactionAudit, RedactionError};

/// Ein signierbares Soll (Umschlag mit Pins), das die Redaction
/// **nachweislich** unverändert durchlaufen hat. Einziger Weg dorthin:
/// [`RedactionPipeline::redact_spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedSpec {
    signed: SignedSpec,
    audit: RedactionAudit,
}

impl RedactedSpec {
    /// Der Umschlag.
    pub fn signed(&self) -> &SignedSpec {
        &self.signed
    }

    /// Das Soll darin.
    pub fn spec(&self) -> &Spec {
        self.signed.spec()
    }

    /// Die kanonische Form des Umschlags — genau die Bytes, die abgelegt und
    /// signiert werden.
    pub fn text(&self) -> &str {
        self.signed.text()
    }

    /// Die Id des Solls (`derive_key("minds/spec/v1", soll)`).
    pub fn id(&self) -> &ContentHash {
        self.signed.id()
    }

    /// Der Nachweis des Laufs.
    pub fn audit(&self) -> &RedactionAudit {
        &self.audit
    }
}

impl RedactionPipeline {
    /// Prüft ein signierbares Soll (Soll und Pins). Jeder Fehler heißt:
    /// **es wird nicht abgelegt**.
    pub fn redact_spec(&self, signed: SignedSpec) -> Result<RedactedSpec, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        let mut audit = RedactionAudit::default();
        for (index, value) in signed.strings().into_iter().enumerate() {
            self.unchanged_spec(Field::SpecValue(index), value, &mut audit)?;
        }
        self.unchanged_spec(Field::Spec, signed.text().to_owned(), &mut audit)?;
        Ok(RedactedSpec { signed, audit })
    }

    fn unchanged_spec(
        &self,
        field: Field,
        value: String,
        audit: &mut RedactionAudit,
    ) -> Result<(), RedactionError> {
        if self.redact_field(field, value.clone(), audit)? != value {
            return Err(RedactionError::SpecRedacted { field });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RedactionConfig;

    fn pipeline() -> RedactionPipeline {
        RedactionConfig::default().pipeline().unwrap()
    }

    const TOKEN: &str = "ghp_R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00";

    fn spec(description: &str) -> SignedSpec {
        let value = serde_json::json!({
            "version": "minds-spec-v1",
            "description": description,
            "rules": [{"id": "a", "kind": "graph", "expect": "session"}],
        });
        let spec = Spec::parse(serde_json::to_string(&value).unwrap().as_bytes()).unwrap();
        SignedSpec::new(spec, Default::default(), &"a".repeat(40)).unwrap()
    }

    #[test]
    fn clean_spec_passes_unchanged() {
        let clean = spec("Architektur");
        let redacted = pipeline().redact_spec(clean.clone()).unwrap();
        assert_eq!(redacted.text(), clean.text());
        assert_eq!(redacted.id(), clean.id());
    }

    /// MUST_REDACT: ein Token im Freitext — auch mehrzeilig, also in der
    /// kanonischen Form nur escapt sichtbar.
    #[test]
    fn spec_with_secret_is_refused() {
        for description in [
            format!("token {TOKEN}"),
            format!("Zeile eins\n{TOKEN}\nZeile drei"),
        ] {
            let err = pipeline().redact_spec(spec(&description)).unwrap_err();
            assert!(
                matches!(err, RedactionError::SpecRedacted { .. }),
                "{err:?}"
            );
            assert!(!err.to_string().contains(TOKEN));
        }
    }

    fn raw(text: &str) -> SignedSpec {
        let spec = Spec::parse(text.as_bytes()).unwrap();
        SignedSpec::new(spec, Default::default(), &"a".repeat(40)).unwrap()
    }

    /// MUST_REDACT: ein Secret in einem Schlüssel, `\u`-maskiert im Rohtext,
    /// eine URL mit Zugangsdaten in einem unbekannten Regelfeld.
    #[test]
    fn secrets_anywhere_in_the_spec_are_refused() {
        let escaped: String = TOKEN
            .chars()
            .map(|c| format!("\\u{:04x}", c as u32))
            .collect();
        for text in [
            format!(r#"{{"version":"minds-spec-v1","rules":[],"{TOKEN}":1}}"#),
            format!(r#"{{"version":"minds-spec-v1","rules":[],"description":"{escaped}"}}"#),
            r#"{"version":"minds-spec-v1","rules":[{"id":"a","kind":"x","url":"https://user:p4ssw0rd@host/x"}]}"#.to_owned(),
        ] {
            let err = pipeline().redact_spec(raw(&text)).unwrap_err();
            assert!(matches!(err, RedactionError::SpecRedacted { .. }), "{text}: {err:?}");
        }
    }

    /// MUST_REDACT: ein Token als Regel-Id, als Schema-Pfad (dann auch als
    /// Pin-Schlüssel), in einer Liste eines unbekannten Felds.
    #[test]
    fn secrets_in_ids_pins_and_lists_are_refused() {
        let cases = [
            r#"{"version":"minds-spec-v1","rules":[{"id":"AKIAIOSFODNN7EXAMPLE","kind":"graph","expect":"session"}]}"#.to_owned(),
            format!(r#"{{"version":"minds-spec-v1","rules":[{{"id":"{TOKEN}","kind":"graph","expect":"session"}}]}}"#),
            // Getrennt geschrieben, wie in `secret.rs`: Kein zusammenhängendes
            // Token im Quelltext (Push-Schutz der Hoster).
            format!(
                r#"{{"version":"minds-spec-v1","rules":[{{"id":"a","kind":"x","notes":["x","{}"]}}]}}"#,
                concat!("xoxb", "-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx")
            ),
        ];
        for text in cases {
            let err = pipeline().redact_spec(raw(&text)).unwrap_err();
            assert!(
                matches!(err, RedactionError::SpecRedacted { .. }),
                "{text}: {err:?}"
            );
        }
        let path = format!(
            "schemas/{}.json",
            concat!("sk_", "live_4eC39HqLyjWDarjtT1zdp7dc")
        );
        let path = path.as_str();
        let text = format!(
            r#"{{"version":"minds-spec-v1","rules":[{{"id":"a","kind":"json-schema","schema":"{path}","files":"x.json"}}]}}"#
        );
        let spec = Spec::parse(text.as_bytes()).unwrap();
        let pins =
            std::collections::BTreeMap::from([(path.to_owned(), minds_core::spec::pin_of(b"{}"))]);
        let signed = SignedSpec::new(spec, pins, &"a".repeat(40)).unwrap();
        assert!(pipeline().redact_spec(signed).is_err());
    }

    /// MUST_SURVIVE: gewöhnliche Globs, Crate- und Pfadnamen, Regel-Ids —
    /// sonst ließe sich kein Soll signieren.
    #[test]
    fn ordinary_rules_survive() {
        let text = r#"{"version":"minds-spec-v1","rules":[
            {"id":"core-is-leaf","kind":"no-dependency","crate":"minds-core","on":["minds-cli","minds-store"]},
            {"id":"capture","kind":"no-import","paths":"crates/minds-capture/src/**","import":["minds_reader::index","crate::evidence"]},
            {"id":"replay-policy","kind":"json-schema","schema":"schemas/replay.json","files":".minds/replay.json"},
            {"id":"agents","kind":"graph","expect":"intent","paths":["crates/**","src/**/*.rs"]},
            {"id":"tokens","kind":"no-import","paths":["crates/**/token_*.rs"],"import":"crate::redactor::Finding"}
        ]}"#;
        let spec = Spec::parse(text.as_bytes()).unwrap();
        let pins = std::collections::BTreeMap::from([(
            "schemas/replay.json".to_owned(),
            minds_core::spec::pin_of(b"{}"),
        )]);
        // Pins (`b3-` + 64 Hex) und `at` (40 und 64 Hex) überstehen auch in
        // der ganzen kanonischen Form.
        for at in [
            "0123456789abcdef".repeat(4),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
        ] {
            let signed = SignedSpec::new(spec.clone(), pins.clone(), &at).unwrap();
            let result = pipeline().redact_spec(signed);
            assert!(result.is_ok(), "{at}: {result:?}");
        }
    }

    #[test]
    fn empty_pipeline_refuses() {
        let empty = RedactionPipeline::new();
        assert!(matches!(
            empty.redact_spec(spec("x")),
            Err(RedactionError::NoDetectors)
        ));
    }
}
