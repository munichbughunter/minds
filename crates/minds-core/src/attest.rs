//! Der kanonische, signierbare Text einer Attribution.
//!
//! „Wer hat diese Zeilen geschrieben — Mensch oder Maschine, mit welchem Modell?"
//! beantwortet die [`Attribution`](crate::Attribution) *als Behauptung*. Eine
//! Signatur über genau diesen Text macht daraus einen **Nachweis**: Ein
//! Schlüsselinhaber steht dafür ein, dass diese Session (dieser exakte Inhalt,
//! über ihre [`SessionId`]) mit diesem Agenten und Modell entstand.
//!
//! # Warum das reicht
//!
//! Die `SessionId` ist der blake3-Hash der kanonischen Session; Agent und Modell
//! stehen *im* Envelope und damit *im* Hash. Den Payload zu signieren bindet also
//! den vollständigen Session-Inhalt — Agent und Modell stehen zusätzlich im
//! Klartext, damit ein Mensch die Zusage lesen kann, nicht nur ein Verifizierer.
//!
//! Rein und deterministisch: gleiche Session ⇒ byte-gleicher Payload. Signiert
//! und verifiziert wird außerhalb (die CLI ruft `ssh-keygen -Y sign/verify`);
//! `minds-core` hat kein I/O.
//!
//! # Fail-closed gegen Zeilen-Fälschung (#12)
//!
//! Der Payload ist zeilenbasiert, die Freitextfelder sind es nicht: Ein
//! `agent.version` von `"1.0\nmodel=openai/gpt"` erzeugte einen Payload mit
//! zwei `model=`-Zeilen — signiert wäre die menschenlesbare Zusage fälschbar,
//! obwohl der Hash korrekt bindet. Deshalb lehnen die Payload-Funktionen jedes
//! eingebettete Feld ab, das Zeilen- oder Steuerzeichen enthält; die Zeilenzahl
//! ist damit eine Invariante (Attestation: 4, Review: 5) und per Test fixiert.

use crate::{Session, SessionId};

/// Versions-/Domänen-Präfix des Payloads. Ändert sich das Format, ändert sich
/// die Version — eine alte Signatur verifiziert dann bewusst nicht mehr.
pub const ATTESTATION_VERSION: &str = "minds-attestation-v1";

/// Ein Feld, das in einen signierbaren Payload eingebettet werden sollte,
/// könnte dort eine Zeile fälschen.
///
/// Der Fehler **benennt** das Feld, zitiert aber nie seinen Wert — der Wert
/// ist genau das, was hier nicht in eine weitere Senke wandern soll.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("signierbarer Payload: Feld {field} enthält Zeilen- oder Steuerzeichen")]
pub struct PayloadError {
    /// Das betroffene Feld, z. B. `agent.version` oder `reviewer`.
    pub field: &'static str,
}

/// Lehnt Werte ab, die im signierten Klartext mehr wären als sichtbare
/// Zeichen: alles, was eine Zeile fälschen kann (`\n`, `\r`, NEL,
/// U+2028/U+2029) **oder** Text verstecken bzw. visuell umdeuten kann —
/// Bidi-Overrides, Unicode-Tags, Zero-Width, BOM (Kategorien `Cc`, `Cf`,
/// `Zl`, `Zp` und die unsichtbaren Nicht-`Cf`-Träger).
pub(crate) fn check_single_line(field: &'static str, value: &str) -> Result<(), PayloadError> {
    if value.chars().any(hides_or_forges) {
        return Err(PayloadError { field });
    }
    Ok(())
}

/// Ob dieses Zeichen mehr kann, als ein Zeichen zu sein.
///
/// Gefragt wird `str::escape_debug` mit einem Sentinel davor — dieselbe
/// Bauform wie die Log-Entschärfung in `minds-cli/src/text.rs`, dort
/// ausführlich begründet: Ohne Sentinel fielen kombinierende Akzente mit
/// durch, und ein Reviewer-Name in NFD-Form (`Mu\u{308}ller`) würde
/// fälschlich abgelehnt — eine Prüfung, die echte Namen reißen lässt, wird
/// abgeschaltet. Anders als dort wird hier nicht escapt, sondern
/// **abgelehnt**: In einem signierten Payload hat auch eine Escape-Sequenz
/// nichts verloren. Sichtbare Interpunktion, die `escape_debug` escapen
/// würde (`'`, `"`, `\`), bleibt deshalb ausdrücklich erlaubt — sie kann
/// weder eine Zeile fälschen noch Text verstecken.
fn hides_or_forges(c: char) -> bool {
    if matches!(c, '\'' | '"' | '\\') {
        return false;
    }
    // Unsichtbare Träger, die der Sentinel-Trick nicht erwischt — dieselben
    // Bereiche wie `INVISIBLE_CARRIERS` in `minds-cli/src/text.rs`.
    const INVISIBLE_CARRIERS: [std::ops::RangeInclusive<char>; 3] = [
        '\u{17B4}'..='\u{17B5}',
        '\u{180B}'..='\u{180F}',
        '\u{E0100}'..='\u{E01EF}',
    ];
    if INVISIBLE_CARRIERS.iter().any(|range| range.contains(&c)) {
        return true;
    }
    let mut buf = [0u8; 5];
    buf[0] = b'x';
    let len = 1 + c.encode_utf8(&mut buf[1..]).len();
    // Unerreichbar — beide Hälften sind gültiges UTF-8; der Rückfall ist der
    // sichere: ein Zeichen, also `!= 2`, also abgelehnt.
    let probe = std::str::from_utf8(&buf[..len]).unwrap_or("x");
    // Zwei Zeichen heißt: Sentinel und `c` selbst, unverändert. Alles Längere
    // ist eine Escape-Sequenz — und damit hier ein Ablehnungsgrund.
    probe.escape_debug().count() != 2
}

/// Der kanonische Text, über den signiert wird — fail-closed: Felder mit
/// Zeilen- oder Steuerzeichen erzeugen keinen Payload, sondern einen Fehler.
pub fn attestation_payload(id: SessionId, session: &Session) -> Result<String, PayloadError> {
    check_single_line("agent.name", &session.agent.name)?;
    check_single_line("agent.version", &session.agent.version)?;
    check_single_line("model.provider", &session.model.provider)?;
    check_single_line("model.id", &session.model.id)?;
    Ok(format!(
        "{ATTESTATION_VERSION}\n\
         session={id}\n\
         agent={} {}\n\
         model={}/{}\n",
        session.agent.name, session.agent.version, session.model.provider, session.model.id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agent, Intent, Model};

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn session() -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent::default(),
        )
    }

    #[test]
    fn payload_binds_session_agent_and_model() {
        let p = attestation_payload(sid(), &session()).unwrap();
        assert!(p.starts_with("minds-attestation-v1\n"));
        assert!(p.contains(&format!("session=b3-{}", "a".repeat(64))));
        assert!(p.contains("agent=claude-code 1.4.2"));
        assert!(p.contains("model=anthropic/claude-opus-4"));
    }

    #[test]
    fn payload_is_deterministic() {
        assert_eq!(
            attestation_payload(sid(), &session()).unwrap(),
            attestation_payload(sid(), &session()).unwrap()
        );
    }

    #[test]
    fn a_different_agent_changes_the_payload() {
        let mut other = session();
        other.agent.name = "codex".into();
        assert_ne!(
            attestation_payload(sid(), &session()).unwrap(),
            attestation_payload(sid(), &other).unwrap()
        );
    }

    // --- Fail-closed gegen Zeilen-Fälschung (#12) ---------------------------

    #[test]
    fn the_line_count_is_an_invariant() {
        let p = attestation_payload(sid(), &session()).unwrap();
        assert_eq!(p.lines().count(), 4, "{p:?}");
        assert!(p.ends_with('\n'), "{p:?}");
    }

    #[test]
    fn a_newline_in_the_agent_version_yields_no_payload() {
        // Der Angriff aus #12: eine zweite model=-Zeile über das Versionsfeld.
        let mut forged = session();
        forged.agent.version = "1.0\nmodel=openai/gpt".into();
        let err = attestation_payload(sid(), &forged).unwrap_err();
        assert!(err.to_string().contains("agent.version"), "{err}");
        // Der Fehler benennt das Feld, zitiert aber nie den Wert.
        assert!(!err.to_string().contains("openai"), "{err}");
    }

    #[test]
    fn carriage_return_and_unicode_line_breaks_are_rejected_too() {
        for injected in [
            "1.0\rdecision=x",
            "1.0\u{2028}x",
            "1.0\u{2029}x",
            // NEL — der klassisch vergessene Zeilenumbruch (C1, U+0085).
            "1.0\u{0085}model=openai/gpt",
        ] {
            let mut forged = session();
            forged.model.id = injected.into();
            assert!(
                attestation_payload(sid(), &forged).is_err(),
                "{injected:?} kam durch"
            );
        }
    }

    #[test]
    fn hidden_and_bidi_carriers_are_rejected() {
        // Die zweite Angriffshälfte: keinen Zeilenumbruch fälschen, sondern
        // Text verstecken oder die Anzeige umdrehen (Trojan-Source-Klasse).
        for injected in [
            "anna\u{202E}tcejer=noisiced", // RLO — visuelles Umdrehen
            "anna\u{2066}x\u{2069}",       // Bidi-Isolate
            "anna\u{E0041}\u{E0042}",      // Unicode-Tags — unsichtbar
            "anna\u{200B}@example.org",    // Zero-Width Space
            "\u{FEFF}anna@example.org",    // BOM
            "anna\u{E0100}@example.org",   // Variantenselektor-Nachtrag
        ] {
            let mut forged = session();
            forged.agent.name = injected.into();
            assert!(
                attestation_payload(sid(), &forged).is_err(),
                "{injected:?} kam durch"
            );
        }
    }

    #[test]
    fn real_names_and_scripts_pass() {
        // Gegen Überschärfung: NFD-Umlaute (kombinierende Akzente),
        // Nicht-ASCII-Schriften und sichtbare Interpunktion sind gültig.
        for fine in [
            "Mu\u{308}ller",
            "søren@example.org",
            "日本語エージェント",
            "claude-code 1.4",
            r#"anna "die Strenge" o'brien\x"#,
        ] {
            let mut ok = session();
            ok.agent.name = fine.into();
            assert!(
                attestation_payload(sid(), &ok).is_ok(),
                "{fine:?} wurde fälschlich abgelehnt"
            );
        }
    }
}
