//! Die Erstsicht-Gegenzeichnung (EA-19): CI unterschreibt, dass ein Seal
//! **spätestens** in einer bestimmten Pipeline existierte — mit einem
//! Schlüssel, den der Agent nie sieht.
//!
//! # Die Textform
//!
//! Genau fünf Zeilen, `\n`-getrennt, mit abschließendem `\n`:
//!
//! ```text
//! minds-anchor-v1
//! seal=b3-<64hex>
//! project=<CI_PROJECT_PATH>
//! pipeline=<CI_PIPELINE_ID>
//! at=<RFC 3339 nach der Uhr der CI>
//! ```
//!
//! Signiert wird genau dieser Text unter `minds-anchor`; die Signatur liegt
//! daneben (`anchor.sig`), nie darin. Abgelegt unter
//! `refs/minds/anchors/first-sight/<64 hex>` — die erste Sicht gewinnt, der
//! Ref wird nie überschrieben.
//!
//! Unter demselben Namespace signiert CI auch Replay-Records (EA-18b). Die
//! beiden halten nur ihre Formen auseinander: ein JSON-Objekt dort, fünf
//! Zeilen mit der Versionszeile `minds-anchor-v1` hier — kein Text ist
//! beides, eine Signatur über das eine verifiziert nie als das andere.
//!
//! Der Parser ist fail-closed wie der des Intent-Ankers: Der Text ist unser
//! eigenes, signiertes Artefakt. [`FirstSight::to_text`] erzeugt nur, was
//! [`FirstSight::parse`] byte-gleich zurückliest.
//!
//! # Was die Gegenzeichnung sagt — und was nicht
//!
//! Sie ist eine **obere Schranke**: Der Seal existierte spätestens zur Zeit
//! `at` der CI-Uhr. Über den Zeitpunkt seiner Entstehung, über den Inhalt
//! der Session oder darüber, wer ihn erzeugte, sagt sie nichts. Gezählt wird
//! sie erst zur Lesezeit (W2) — nur mit gültiger Signatur eines Principals,
//! der auf `minds-anchor` beschränkt ist.

use crate::ContentHash;

/// Versionszeile der Textform. Ändert sich das Format, ändert sich die
/// Version — eine alte Signatur verifiziert dann bewusst nicht mehr.
pub const FIRST_SIGHT_VERSION: &str = "minds-anchor-v1";

/// Die Textform hat genau so viele Zeilen.
pub const FIRST_SIGHT_LINES: usize = 5;

/// Höchstgröße der Textform — so viel liest der Store zurück. Fünf kurze
/// Zeilen; ein GitLab-Projektpfad hat höchstens einige hundert Zeichen.
pub const MAX_FIRST_SIGHT: usize = 4 * 1024;

/// Höchstlänge des Projektpfads (GitLab: 255 je Segment, praktisch weniger).
const MAX_PROJECT: usize = 1024;

/// Höchstlänge des Zeitstempels.
const MAX_AT: usize = 64;

/// Eine Erstsicht-Gegenzeichnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstSight {
    /// Die gegengezeichnete `seal_id`.
    pub seal: ContentHash,
    /// Der Projektpfad der Pipeline (`gruppe/projekt`).
    pub project: String,
    /// Die Pipeline-Id (`CI_PIPELINE_ID`).
    pub pipeline: u64,
    /// Der Zeitpunkt nach der Uhr der CI, RFC 3339.
    pub at: String,
}

/// Warum ein Text keine Gegenzeichnung ist. Nennt Zeile bzw. Feld, zitiert
/// nie den Wert.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirstSightError {
    /// Länger als [`MAX_FIRST_SIGHT`].
    #[error("a first-sight anchor is at most {MAX_FIRST_SIGHT} bytes")]
    TooLong,

    /// Falsche Zeilenzahl.
    #[error("a first-sight anchor has {FIRST_SIGHT_LINES} lines, this text has {0}")]
    Lines(usize),

    /// Der Text endet nicht mit genau einem `\n`.
    #[error("a first-sight anchor ends with a single line feed")]
    TrailingNewline,

    /// Unbekannte Versionszeile.
    #[error("unknown first-sight anchor version")]
    Version,

    /// Eine Zeile trägt nicht den erwarteten Schlüssel oder keinen gültigen
    /// Wert.
    #[error("first-sight anchor field {0} is missing or invalid")]
    Field(&'static str),

    /// Der Text liest nicht zu genau dieser Gegenzeichnung zurück.
    #[error("first-sight anchor does not read back to itself")]
    NotCanonical,
}

impl FirstSight {
    /// Die Textform — fail-closed: Erzeugt wird nur, was
    /// [`parse`](Self::parse) zu genau dieser Gegenzeichnung zurückliest.
    pub fn to_text(&self) -> Result<String, FirstSightError> {
        let text = format!(
            "{FIRST_SIGHT_VERSION}\nseal={}\nproject={}\npipeline={}\nat={}\n",
            self.seal, self.project, self.pipeline, self.at
        );
        // Ein Rundlauf statt einer zweiten Grammatik.
        if Self::parse(&text)? != *self {
            return Err(FirstSightError::NotCanonical);
        }
        Ok(text)
    }

    /// Liest die Textform zurück — strikt: exakt [`FIRST_SIGHT_LINES`]
    /// Zeilen, `\n` als Trenner und am Ende, bekannte Version, jede Zeile
    /// mit ihrem Schlüssel, nur kanonische Werte (Seal klein geschrieben,
    /// Pipeline ohne führende Null).
    pub fn parse(text: &str) -> Result<Self, FirstSightError> {
        if text.len() > MAX_FIRST_SIGHT {
            return Err(FirstSightError::TooLong);
        }
        let body = text
            .strip_suffix('\n')
            .ok_or(FirstSightError::TrailingNewline)?;
        let lines: Vec<&str> = body.split('\n').collect();
        if lines.len() != FIRST_SIGHT_LINES {
            return Err(FirstSightError::Lines(lines.len()));
        }
        // Nur druckbares ASCII: Jedes Feld hat eine ASCII-Grammatik, und
        // so kann keine Zeile eine andere fälschen oder Text verstecken
        // (`\r`, Bidi-Overrides, Zero-Width).
        for (line, name) in lines
            .iter()
            .zip(["version", "seal", "project", "pipeline", "at"])
        {
            if !line.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(FirstSightError::Field(name));
            }
        }
        if lines[0] != FIRST_SIGHT_VERSION {
            return Err(FirstSightError::Version);
        }
        fn field<'a>(line: &'a str, key: &'static str) -> Result<&'a str, FirstSightError> {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or(FirstSightError::Field(key))
        }
        let seal_text = field(lines[1], "seal")?;
        let seal: ContentHash = seal_text
            .parse()
            .map_err(|_| FirstSightError::Field("seal"))?;
        if seal.as_str() != seal_text {
            return Err(FirstSightError::Field("seal"));
        }
        let project = field(lines[2], "project")?;
        if project.len() > MAX_PROJECT || !crate::intent_anchor::project_path(project) {
            return Err(FirstSightError::Field("project"));
        }
        let pipeline = parse_pipeline(field(lines[3], "pipeline")?)
            .ok_or(FirstSightError::Field("pipeline"))?;
        let at = field(lines[4], "at")?;
        if !timestamp(at) {
            return Err(FirstSightError::Field("at"));
        }
        Ok(Self {
            seal,
            project: project.to_owned(),
            pipeline,
            at: at.to_owned(),
        })
    }
}

/// Eine Pipeline-Id: nur Ziffern, keine führende Null, nicht 0 — es gibt
/// genau eine Schreibweise je Zahl.
pub fn parse_pipeline(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) || value.starts_with('0') {
        return None;
    }
    value.parse().ok()
}

/// Ein GitLab-Projektpfad, wie ihn eine Gegenzeichnung tragen darf.
pub fn is_project_path(value: &str) -> bool {
    value.len() <= MAX_PROJECT && crate::intent_anchor::project_path(value)
}

/// RFC 3339 mit `Z` oder Offset (Sekundenbruchteile erlaubt), ohne
/// Zonen-Annotation. Kanonisch ist der Text, nicht der Zeitpunkt.
fn timestamp(value: &str) -> bool {
    value.len() <= MAX_AT
        && !value.contains(['[', ']'])
        && value.contains('T')
        && value.parse::<jiff::Timestamp>().is_ok()
}

/// Der Zeitpunkt `now` in der Form, die `minds anchor` schreibt:
/// `YYYY-MM-DDTHH:MM:SSZ`, auf Sekunden abgeschnitten.
pub fn format_at(now: jiff::Timestamp) -> String {
    let seconds = jiff::Timestamp::from_second(now.as_second()).unwrap_or(now);
    seconds.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FirstSight {
        FirstSight {
            seal: ContentHash::from_bytes([0xab; 32]),
            project: "group/sub/repo".into(),
            pipeline: 4711,
            at: "2026-10-08T12:34:56Z".into(),
        }
    }

    /// Golden: die eingefrorene Textform — genau fünf Zeilen, `\n` am Ende.
    /// Ändert sich ein Byte, verifiziert keine alte Signatur mehr.
    #[test]
    fn first_sight_text_golden() {
        let text = sample().to_text().unwrap();
        assert_eq!(
            text,
            "minds-anchor-v1\n\
             seal=b3-abababababababababababababababababababababababababababababababab\n\
             project=group/sub/repo\n\
             pipeline=4711\n\
             at=2026-10-08T12:34:56Z\n"
        );
        assert_eq!(text.lines().count(), FIRST_SIGHT_LINES);
        // Der Hash der Bytes, eingefroren (unabhängig nachgerechnet mit Pythons `blake3`).
        assert_eq!(
            blake3::hash(text.as_bytes()).to_hex().as_str(),
            "443fdb2201578d1aceaac230132f9c15f45985846df64a2b1909cf453a315f30"
        );
        assert_eq!(FirstSight::parse(&text).unwrap(), sample());
    }

    #[test]
    fn the_parser_refuses_every_non_canonical_form() {
        let good = sample().to_text().unwrap();
        let cases: Vec<(String, FirstSightError)> = vec![
            (good.trim_end().to_owned(), FirstSightError::TrailingNewline),
            (format!("{good}\n"), FirstSightError::Lines(6)),
            (
                good.replace("minds-anchor-v1", "minds-anchor-v2"),
                FirstSightError::Version,
            ),
            (
                good.replace("seal=b3-ab", "seal=b3-AB"),
                FirstSightError::Field("seal"),
            ),
            (
                good.replace("seal=", "seel="),
                FirstSightError::Field("seal"),
            ),
            (
                good.replace("project=group/sub/repo", "project=group/../repo"),
                FirstSightError::Field("project"),
            ),
            (
                good.replace("project=group/sub/repo", "project=group sub"),
                FirstSightError::Field("project"),
            ),
            (
                good.replace("pipeline=4711", "pipeline=04711"),
                FirstSightError::Field("pipeline"),
            ),
            (
                good.replace("pipeline=4711", "pipeline=0"),
                FirstSightError::Field("pipeline"),
            ),
            (
                good.replace("pipeline=4711", "pipeline=+4711"),
                FirstSightError::Field("pipeline"),
            ),
            (
                good.replace("at=2026-10-08T12:34:56Z", "at=yesterday"),
                FirstSightError::Field("at"),
            ),
            (
                good.replace("at=2026-10-08T12:34:56Z", "at=2026-10-08T12:34:56Z[UTC]"),
                FirstSightError::Field("at"),
            ),
            (
                good.replace('\n', "\r\n"),
                FirstSightError::Field("version"),
            ),
            (
                good.replace("group/sub/repo", "group/sub/re\u{202e}po"),
                FirstSightError::Field("project"),
            ),
            ("x".repeat(MAX_FIRST_SIGHT + 1), FirstSightError::TooLong),
        ];
        for (text, expected) in cases {
            assert_eq!(FirstSight::parse(&text), Err(expected), "{text:?}");
        }
    }

    /// Tolerant gelesen, wo die Uhr variiert: Offset und Bruchteile sind
    /// gültige RFC-3339-Zeitpunkte — und bleiben Byte für Byte erhalten.
    #[test]
    fn timestamps_keep_their_spelling() {
        for at in ["2026-10-08T12:34:56.123Z", "2026-10-08T14:34:56+02:00"] {
            let anchor = FirstSight {
                at: at.into(),
                ..sample()
            };
            let text = anchor.to_text().unwrap();
            assert_eq!(FirstSight::parse(&text).unwrap().at, at);
        }
    }

    #[test]
    fn the_written_clock_is_seconds_in_utc() {
        let now: jiff::Timestamp = "2026-10-08T12:34:56.987654Z".parse().unwrap();
        assert_eq!(format_at(now), "2026-10-08T12:34:56Z");
    }

    #[test]
    fn pipelines_have_one_spelling() {
        assert_eq!(parse_pipeline("1"), Some(1));
        assert_eq!(parse_pipeline("18446744073709551615"), Some(u64::MAX));
        for bad in ["", "0", "01", "-1", "1e3", "18446744073709551616", " 1"] {
            assert_eq!(parse_pipeline(bad), None, "{bad:?}");
        }
    }
}
