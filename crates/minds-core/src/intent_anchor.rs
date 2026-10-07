//! Der Intent-Anker (EA-14, ADR-0012 Entscheidung 5): eine inhalts-
//! adressierte, versionierte Anforderung als erstes Glied der Kette einer
//! Session.
//!
//! # Die Textform
//!
//! Genau vier Zeilen, `\n`-getrennt, mit abschließendem `\n`:
//!
//! ```text
//! minds-intent-v1
//! source=file:<repo-relativer Pfad>@<Git-Blob-SHA> | issue:<Projekt>#<iid>@<updated_at> | prompt
//! content=b3-<64hex>
//! scope=<glob>[,<glob>…] | -
//! ```
//!
//! Der Parser ist fail-closed wie der des Seals ([`crate::evidence::Seal::parse`]):
//! Der Anker ist unser eigenes kanonisches Artefakt und wird signiert —
//! Toleranz wäre hier keine Freundlichkeit, sondern eine Angriffsfläche.
//! [`IntentAnchor::to_text`] erzeugt nur, was [`IntentAnchor::parse`]
//! byte-gleich zurückliest.
//!
//! # Zwei Hash-Domänen
//!
//! - `content = derive_key("minds/intent/v1/content", Snapshot-Bytes)` —
//!   über den **redigierten** Snapshot. Damit ist der Hash aus abgelegtem
//!   Material nachrechenbar und nie ein Orakel für ein entferntes Secret.
//! - `anchor_id = derive_key("minds/intent/v1/anchor", Ankertext)`.
//!
//! Beide sind eigene Kontexte neben `minds/evidence/v1/*` (W4): Ein
//! Anker-Hash kann nie als Event-, Seal- oder Payload-Hash durchgehen.
//!
//! # Das Ketten-Event
//!
//! Der Witness stellt den aktiven Anker als synthetisches Event
//! [`INTENT_EVENT_KIND`] mit Payload [`IntentEventPayload`] an den Anfang
//! einer neuen Session (und hängt einen Wechsel während der Session an).
//! Was davon im gespeicherten Envelope landet, ist nur die Beobachtung
//! [`IntentEvent`] — welche Anker-Id an welcher Stelle der Kette stand.
//! Ob das „gebunden", „verkettet" oder „mitten in der Session gewechselt"
//! heißt, rechnet erst der Reader (W2).

use serde::{Deserialize, Serialize};

use crate::ContentHash;

/// Versionszeile der Textform. Ändert sich das Format, ändert sich die
/// Version — eine alte Signatur verifiziert dann bewusst nicht mehr.
pub const INTENT_VERSION: &str = "minds-intent-v1";

/// Die Textform hat genau so viele Zeilen.
pub const INTENT_LINES: usize = 4;

/// Höchstgröße der Textform. So viel liest der Store zurück (`refs/minds/intents/`);
/// ein größerer Anker ließe sich aktivieren, aber nie ablegen.
pub const MAX_ANCHOR: usize = 64 * 1024;

/// Höchstgröße eines Snapshots — ebenfalls so viel, wie der Store zurückliest.
pub const MAX_SNAPSHOT: usize = 4 * 1024 * 1024;

/// Hash-Domäne des Snapshot-Inhalts (`content=`).
pub const CTX_INTENT_CONTENT: &str = "minds/intent/v1/content";

/// Hash-Domäne der Anker-Identität (`anchor_id`).
pub const CTX_INTENT_ANCHOR: &str = "minds/intent/v1/anchor";

/// Der `raw_kind` des synthetischen Ketten-Events. Reserviert: Der Witness
/// nimmt ein Hook-Event mit diesem Namen nie von der Agent-Seite an.
pub const INTENT_EVENT_KIND: &str = "minds.intent";

/// Woher die Anforderung stammt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentSource {
    /// Eine Datei im Repository in einer bestimmten Fassung.
    File {
        /// Repo-relativer Pfad mit `/` als Trenner.
        path: String,
        /// Git-Blob-SHA (40 oder 64 Hex-Zeichen, klein).
        blob: String,
    },
    /// Ein Issue in einer bestimmten Fassung (EA-16).
    Issue {
        /// Projektpfad (`gruppe/projekt`).
        project: String,
        /// Die projektinterne Issue-Nummer.
        iid: u64,
        /// `updated_at` der Fassung, RFC 3339.
        updated_at: String,
    },
    /// Nur der Prompt — keine externe Anforderung.
    Prompt,
}

/// Ein Intent-Anker: Quelle, Inhalts-Hash des redigierten Snapshots und der
/// erwartete Wirkungsbereich.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAnchor {
    /// Woher die Anforderung stammt.
    pub source: IntentSource,
    /// `derive_key(CTX_INTENT_CONTENT, redigierter Snapshot)` — siehe
    /// [`content_hash`].
    pub content: ContentHash,
    /// Pfad-Globs, die die Arbeit berühren soll. Leer heißt `scope=-`
    /// (kein Bereich angegeben), nicht „nichts".
    pub scope: Vec<String>,
}

/// Warum ein Text kein Intent-Anker ist. Nennt Zeile bzw. Feld, zitiert nie
/// den Wert.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntentAnchorError {
    /// Länger als [`MAX_ANCHOR`].
    #[error("an intent anchor is at most {MAX_ANCHOR} bytes")]
    TooLong,

    /// Falsche Zeilenzahl.
    #[error("an intent anchor has {INTENT_LINES} lines, this text has {0}")]
    Lines(usize),

    /// Der Text endet nicht mit genau einem `\n`.
    #[error("an intent anchor ends with a single line feed")]
    TrailingNewline,

    /// Unbekannte Versionszeile.
    #[error("unknown intent anchor version")]
    Version,

    /// Eine Zeile trägt nicht den erwarteten Schlüssel oder keinen gültigen
    /// Wert.
    #[error("intent anchor field {0} is missing or invalid")]
    Field(&'static str),

    /// Die Anker-Id des Payloads ist nicht der Hash seines Ankertexts.
    #[error("intent anchor id does not match the anchor text")]
    IdMismatch,

    /// Die Signatur hat nicht die Form einer armierten `ssh-sig`-Signatur.
    #[error("intent signature is not an armored ssh signature")]
    Signature,
}

/// Der Inhalts-Hash eines (redigierten) Snapshots.
pub fn content_hash(snapshot: &[u8]) -> ContentHash {
    ContentHash::from_bytes(blake3::derive_key(CTX_INTENT_CONTENT, snapshot))
}

impl IntentAnchor {
    /// Die Textform — fail-closed: Erzeugt wird nur, was [`parse`](Self::parse)
    /// zu genau diesem Anker zurückliest. Ein Feld, das eine Zeile fälschen,
    /// Text verstecken oder die Grammatik verbiegen könnte, ergibt einen
    /// Fehler statt eines Ankers.
    pub fn to_text(&self) -> Result<String, IntentAnchorError> {
        let source = match &self.source {
            IntentSource::File { path, blob } => format!("file:{path}@{blob}"),
            IntentSource::Issue {
                project,
                iid,
                updated_at,
            } => format!("issue:{project}#{iid}@{updated_at}"),
            IntentSource::Prompt => "prompt".to_owned(),
        };
        let scope = if self.scope.is_empty() {
            "-".to_owned()
        } else {
            self.scope.join(",")
        };
        let text = format!(
            "{INTENT_VERSION}\nsource={source}\ncontent={content}\nscope={scope}\n",
            content = self.content,
        );
        // Ein Rundlauf statt einer zweiten Grammatik: Was der Parser nicht
        // byte-gleich zurückliest, wird nicht geschrieben — gemeldet wird
        // das Feld, das abweicht.
        let back = Self::parse(&text)?;
        if back.source != self.source {
            return Err(IntentAnchorError::Field("source"));
        }
        if back.scope != self.scope {
            return Err(IntentAnchorError::Field("scope"));
        }
        Ok(text)
    }

    /// Die Identität eines Ankertexts: `derive_key(CTX_INTENT_ANCHOR, text)`.
    pub fn id_of_text(text: &str) -> ContentHash {
        ContentHash::from_bytes(blake3::derive_key(CTX_INTENT_ANCHOR, text.as_bytes()))
    }

    /// Liest die Textform zurück — strikt: exakt [`INTENT_LINES`] Zeilen,
    /// `\n` als Trenner und am Ende, bekannte Version, jede Zeile mit ihrem
    /// Schlüssel, nur kanonische Werte.
    pub fn parse(text: &str) -> Result<Self, IntentAnchorError> {
        if text.len() > MAX_ANCHOR {
            return Err(IntentAnchorError::TooLong);
        }
        let body = text
            .strip_suffix('\n')
            .ok_or(IntentAnchorError::TrailingNewline)?;
        let lines: Vec<&str> = body.split('\n').collect();
        if lines.len() != INTENT_LINES {
            return Err(IntentAnchorError::Lines(lines.len()));
        }
        // Jede Zeile einzeilig und ohne Versteckzeichen — `\r`, NEL,
        // Bidi-Overrides, Zero-Width (dieselbe Prüfung wie die
        // Attestation-Payloads, #12).
        for (line, name) in lines.iter().zip(["version", "source", "content", "scope"]) {
            crate::attest::check_single_line(name, line)
                .map_err(|_| IntentAnchorError::Field(name))?;
        }
        if lines[0] != INTENT_VERSION {
            return Err(IntentAnchorError::Version);
        }
        fn field<'a>(line: &'a str, key: &'static str) -> Result<&'a str, IntentAnchorError> {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or(IntentAnchorError::Field(key))
        }
        let source = parse_source(field(lines[1], "source")?)?;
        let content_text = field(lines[2], "content")?;
        let content: ContentHash = content_text
            .parse()
            .map_err(|_| IntentAnchorError::Field("content"))?;
        // Nur die kanonische Schreibweise: Der Anker ist signiert, und es
        // gibt genau eine Bytefolge je Inhalt.
        if content.as_str() != content_text {
            return Err(IntentAnchorError::Field("content"));
        }
        let scope = parse_scope(field(lines[3], "scope")?)?;
        Ok(Self {
            source,
            content,
            scope,
        })
    }
}

/// `file:…@…`, `issue:…#…@…` oder `prompt`.
fn parse_source(value: &str) -> Result<IntentSource, IntentAnchorError> {
    const BAD: IntentAnchorError = IntentAnchorError::Field("source");
    if value == "prompt" {
        return Ok(IntentSource::Prompt);
    }
    if let Some(rest) = value.strip_prefix("file:") {
        // Am letzten `@` trennen: Ein Pfad darf `@` enthalten, ein Blob-SHA
        // nicht.
        let (path, blob) = rest.rsplit_once('@').ok_or(BAD)?;
        if !repo_relative(path) || !blob_sha(blob) {
            return Err(BAD);
        }
        return Ok(IntentSource::File {
            path: path.to_owned(),
            blob: blob.to_owned(),
        });
    }
    if let Some(rest) = value.strip_prefix("issue:") {
        let (project, tail) = rest.split_once('#').ok_or(BAD)?;
        let (iid, updated_at) = tail.split_once('@').ok_or(BAD)?;
        if !project_path(project) {
            return Err(BAD);
        }
        // Kanonisch: nur Ziffern, keine führende Null, nicht 0.
        if iid.is_empty() || !iid.bytes().all(|b| b.is_ascii_digit()) || iid.starts_with('0') {
            return Err(BAD);
        }
        let iid: u64 = iid.parse().map_err(|_| BAD)?;
        // RFC 3339, wie die Quelle es liefert (Offset und Sekundenbruchteile
        // erlaubt) — ohne Zonen-Annotation (`[Europe/Berlin]`) und ohne
        // Leerraum. Kanonisch ist der Text, nicht der Zeitpunkt: Zwei
        // Schreibweisen desselben Moments sind zwei Anker.
        if updated_at.contains(['[', ']', '@'])
            || updated_at.chars().any(char::is_whitespace)
            || updated_at.parse::<jiff::Timestamp>().is_err()
        {
            return Err(BAD);
        }
        return Ok(IntentSource::Issue {
            project: project.to_owned(),
            iid,
            updated_at: updated_at.to_owned(),
        });
    }
    Err(BAD)
}

/// Repo-relativ: nicht leer, kein führender `/`, kein `\`, kein `:`
/// (Laufwerksbuchstabe, Windows-Datenstrom), keine leeren, `.`- oder
/// `..`-Segmente und nichts unter `.git` — ein Snapshot aus dem
/// Git-Verzeichnis (etwa `.git/config` mit Zugangs-URL) ist keine
/// Anforderung. Aufgelöst wird der Pfad nur zusammen mit der Blob-SHA, nie
/// über das Dateisystem — NTFS-Aliase wie `.git.` oder `GIT~1` führen so
/// nicht ins Git-Verzeichnis.
fn repo_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && path.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..") && !segment.eq_ignore_ascii_case(".git")
        })
}

/// Ein GitLab-Projektpfad (`gruppe/untergruppe/projekt`): Segmente aus
/// `[A-Za-z0-9._-]`, nicht leer, kein `.`/`..` — EA-16 baut daraus eine
/// API-URL, ein `?`, `%` oder `..` hätte dort eine andere Bedeutung.
fn project_path(project: &str) -> bool {
    !project.is_empty()
        && project.split('/').all(|segment| {
            !matches!(segment, "" | "." | "..")
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// Erste und letzte Zeile einer `ssh-sig`-Signatur.
const ARMOR_BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const ARMOR_END: &str = "-----END SSH SIGNATURE-----";

/// Höchstgröße einer armierten Signatur. Ein `ssh-sig` über Ed25519 oder
/// einen `sk`-Schlüssel braucht weniger als ein KiB.
pub const MAX_SIGNATURE: usize = 16 * 1024;

/// Ob `text` die Form einer armierten `ssh-sig`-Signatur hat: begrenzt,
/// `BEGIN`-Zeile, Base64-Zeilen, `END`-Zeile, `\n` als Trenner. Eine
/// Formprüfung, keine Verifikation — die ist Sache des Lesers (EA-15). Sie
/// hält Freitext aus Zustand und Store, die eine Signatur tragen.
pub fn is_armored_signature(text: &str) -> bool {
    if text.len() > MAX_SIGNATURE {
        return false;
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = body.split('\n').collect();
    let [first, middle @ .., last] = lines.as_slice() else {
        return false;
    };
    *first == ARMOR_BEGIN
        && *last == ARMOR_END
        && !middle.is_empty()
        && middle.iter().all(|line| {
            !line.is_empty()
                && line
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        })
}

/// Ein Git-Objektname: 40 (SHA-1) oder 64 (SHA-256) Hex-Zeichen, klein.
fn blob_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `-` oder eine Komma-Liste nicht-leerer Globs ohne Rand-Leerraum.
fn parse_scope(value: &str) -> Result<Vec<String>, IntentAnchorError> {
    const BAD: IntentAnchorError = IntentAnchorError::Field("scope");
    if value == "-" {
        return Ok(Vec::new());
    }
    if value.is_empty() {
        return Err(BAD);
    }
    value
        .split(',')
        .map(|glob| {
            if glob.is_empty() || glob == "-" || glob.trim() != glob {
                Err(BAD)
            } else {
                Ok(glob.to_owned())
            }
        })
        .collect()
}

/// Der Payload des Ketten-Events [`INTENT_EVENT_KIND`] — so, wie der
/// Witness ihn ins Journal schreibt:
/// `{"anchor_id":"b3-…","anchor":"<text>","signature":"<armored>"|null}`,
/// am Anfang einer Session zusätzlich `"opens_session":true`.
///
/// Die Signatur ist hier nur der Form nach geprüft
/// ([`is_armored_signature`]); verifiziert wird sie zur Lesezeit unter dem
/// Namespace `minds-intent` (EA-15), nie beim Erfassen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Strikt: Ein Hook-Payload der Agent-Seite trägt immer `session_id` oder
// `transcript_path` (ohne beides kein Session-Schlüssel) — er kann so nie als
// dieser Payload durchgehen, auch nicht aus einem Journal von vor EA-14.
#[serde(deny_unknown_fields)]
pub struct IntentEventPayload {
    /// `derive_key(CTX_INTENT_ANCHOR, anchor)`.
    pub anchor_id: ContentHash,
    /// Der Ankertext.
    pub anchor: String,
    /// Die `ssh-sig`-Signatur über den Ankertext, falls übergeben.
    pub signature: Option<String>,
    /// Der Witness hat das Event als **erstes** der Session geschrieben —
    /// vor ihrem ersten Hook-Event. Der positive Beleg für „die Kette
    /// beginnt beim Intent"; ein fehlendes `previous` im Seal ist keiner
    /// (dort heißt `None` nur „nicht belegt").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opens_session: bool,
}

impl IntentEventPayload {
    /// Baut den Payload aus einem Ankertext — nur für einen gültigen Anker
    /// und eine Signatur in `ssh-sig`-Form.
    pub fn new(anchor: &str, signature: Option<String>) -> Result<Self, IntentAnchorError> {
        IntentAnchor::parse(anchor)?;
        if signature
            .as_deref()
            .is_some_and(|signature| !is_armored_signature(signature))
        {
            return Err(IntentAnchorError::Signature);
        }
        Ok(Self {
            anchor_id: IntentAnchor::id_of_text(anchor),
            anchor: anchor.to_owned(),
            signature,
            opens_session: false,
        })
    }

    /// Prüft einen gelesenen Payload: Der Ankertext ist gültig, die Id ist
    /// sein Hash, eine Signatur hat `ssh-sig`-Form. Gibt den geparsten Anker
    /// zurück.
    pub fn check(&self) -> Result<IntentAnchor, IntentAnchorError> {
        let anchor = IntentAnchor::parse(&self.anchor)?;
        if IntentAnchor::id_of_text(&self.anchor) != self.anchor_id {
            return Err(IntentAnchorError::IdMismatch);
        }
        if self
            .signature
            .as_deref()
            .is_some_and(|signature| !is_armored_signature(signature))
        {
            return Err(IntentAnchorError::Signature);
        }
        Ok(anchor)
    }
}

/// Die Beobachtung im gespeicherten Envelope: An dieser Stelle der Kette
/// stand ein vom Witness verkettetes [`INTENT_EVENT_KIND`]-Event mit dieser
/// Anker-Id.
///
/// Bewusst nur Fakten (W2): Nummer, Id und ob der Witness es als erstes
/// Event der Session geschrieben hat ([`IntentEventPayload::opens_session`])
/// — wie `lineage.closed` eine Beobachtung, keine Bewertung. Signatur und
/// Ankertext stehen nicht hier: Eine armierte Signatur sähe für die
/// Redaction wie ein Token aus, und beides liegt ohnehin unter
/// `refs/minds/intents/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentEvent {
    /// Die Journal-Nummer des Events.
    pub seq: u64,
    /// Die Anker-Id aus dem geprüften Payload.
    pub anchor_id: ContentHash,
    /// Der Witness hat es vor dem ersten Hook-Event der Session geschrieben.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opens_session: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_anchor() -> IntentAnchor {
        IntentAnchor {
            source: IntentSource::File {
                path: "docs/fachliche-anforderung.md".into(),
                blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
            },
            content: content_hash(b"Die Retry-Logik soll exponentiell zurueckfallen.\n"),
            scope: vec!["src/retry/**".into(), "tests/retry_*.rs".into()],
        }
    }

    /// Eingefrorene Textform und Identität — ändert sich eins davon, ist
    /// jede bestehende Signatur ungültig.
    #[test]
    fn intent_anchor_text_golden() {
        let anchor = file_anchor();
        let text = anchor.to_text().unwrap();
        assert_eq!(
            text,
            "minds-intent-v1\n\
             source=file:docs/fachliche-anforderung.md@3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b\n\
             content=b3-9b588b7bb5ea2bb3635b0156f4379aacb8b41fbc5931c4d64bc4210a88fea8e8\n\
             scope=src/retry/**,tests/retry_*.rs\n"
        );
        assert_eq!(
            IntentAnchor::id_of_text(&text).as_str(),
            "b3-e53d0de39ca27db29b61bd5629d75fdc4a4830dda16f674485c05a37f1fc0444"
        );
        assert_eq!(IntentAnchor::parse(&text).unwrap(), anchor);

        let issue = IntentAnchor {
            source: IntentSource::Issue {
                project: "team/minds".into(),
                iid: 42,
                updated_at: "2026-10-01T08:15:00Z".into(),
            },
            content: content_hash(b"{\"description\":\"x\",\"title\":\"y\"}"),
            scope: Vec::new(),
        };
        let text = issue.to_text().unwrap();
        assert_eq!(
            text,
            format!(
                "minds-intent-v1\nsource=issue:team/minds#42@2026-10-01T08:15:00Z\ncontent={}\nscope=-\n",
                issue.content
            )
        );
        assert_eq!(IntentAnchor::parse(&text).unwrap(), issue);

        let prompt = IntentAnchor {
            source: IntentSource::Prompt,
            content: content_hash(b"mach x"),
            scope: vec!["src/**".into()],
        };
        let text = prompt.to_text().unwrap();
        assert!(text.contains("\nsource=prompt\n"));
        assert_eq!(IntentAnchor::parse(&text).unwrap(), prompt);
    }

    #[test]
    fn content_and_anchor_hashes_are_domain_separated() {
        let bytes = b"same bytes";
        assert_ne!(
            content_hash(bytes),
            IntentAnchor::id_of_text(std::str::from_utf8(bytes).unwrap())
        );
        assert_ne!(content_hash(bytes), crate::evidence::payload_hash(bytes));
        assert_eq!(content_hash(bytes), content_hash(bytes));
    }

    #[test]
    fn intent_anchor_parser_is_fail_closed() {
        let good = file_anchor().to_text().unwrap();
        let content = file_anchor().content;
        let line = |source: &str, scope: &str| {
            format!("minds-intent-v1\nsource={source}\ncontent={content}\nscope={scope}\n")
        };
        let blob = "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b";
        let cases: Vec<(String, IntentAnchorError)> = vec![
            // Zeilen und Zeilenenden
            (
                good.trim_end_matches('\n').to_owned(),
                IntentAnchorError::TrailingNewline,
            ),
            (format!("{good}\n"), IntentAnchorError::Lines(5)),
            (format!("{good}extra=1\n"), IntentAnchorError::Lines(5)),
            (
                good.replacen(&format!("content={content}\n"), "", 1),
                IntentAnchorError::Lines(3),
            ),
            (
                good.replace('\n', "\r\n"),
                IntentAnchorError::Field("version"),
            ),
            (String::new(), IntentAnchorError::TrailingNewline),
            // Version und Schlüssel
            (
                good.replacen("minds-intent-v1", "minds-intent-v2", 1),
                IntentAnchorError::Version,
            ),
            (
                good.replacen("source=", "src=", 1),
                IntentAnchorError::Field("source"),
            ),
            (
                good.replacen("scope=", "scope =", 1),
                IntentAnchorError::Field("scope"),
            ),
            // Steuer- und Versteckzeichen
            (
                line(&format!("file:docs/a\u{7}.md@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:docs/a\u{202E}.md@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("prompt", "src/\u{200B}**"),
                IntentAnchorError::Field("scope"),
            ),
            (
                line("prompt", "src/\u{1b}[31m**"),
                IntentAnchorError::Field("scope"),
            ),
            // Hash
            (
                good.replacen(content.as_str(), "b3-1234", 1),
                IntentAnchorError::Field("content"),
            ),
            (
                good.replacen(content.as_str(), &content.as_str().to_uppercase(), 1),
                IntentAnchorError::Field("content"),
            ),
            (
                good.replacen(content.as_str(), content.hex(), 1),
                IntentAnchorError::Field("content"),
            ),
            (
                good.replacen(content.as_str(), &format!("b3-{}", "g".repeat(64)), 1),
                IntentAnchorError::Field("content"),
            ),
            // Scope
            (line("prompt", ""), IntentAnchorError::Field("scope")),
            (line("prompt", "src/**,"), IntentAnchorError::Field("scope")),
            (line("prompt", ",src/**"), IntentAnchorError::Field("scope")),
            (line("prompt", "a,,b"), IntentAnchorError::Field("scope")),
            (line("prompt", " src/**"), IntentAnchorError::Field("scope")),
            (line("prompt", "a,-"), IntentAnchorError::Field("scope")),
            // Quelle
            (line("", "-"), IntentAnchorError::Field("source")),
            (line("Prompt", "-"), IntentAnchorError::Field("source")),
            (line("url:x", "-"), IntentAnchorError::Field("source")),
            (
                line(&format!("file:/etc/passwd@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:../x.md@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:a//b.md@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:a\\b.md@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:@{blob}"), "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("file:a.md@abc", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(&format!("file:a.md@{}", blob.to_uppercase()), "-"),
                IntentAnchorError::Field("source"),
            ),
            (line("file:a.md", "-"), IntentAnchorError::Field("source")),
            (
                line("issue:team/minds#42", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:team/minds#042@2026-10-01T08:15:00Z", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:team/minds#0@2026-10-01T08:15:00Z", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:team/minds#x@2026-10-01T08:15:00Z", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:#42@2026-10-01T08:15:00Z", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:team minds#42@2026-10-01T08:15:00Z", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line("issue:team/minds#42@yesterday", "-"),
                IntentAnchorError::Field("source"),
            ),
            (
                line(
                    "issue:team/minds#42@2026-10-01T08:15:00Z[Europe/Berlin]",
                    "-",
                ),
                IntentAnchorError::Field("source"),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                IntentAnchor::parse(&text),
                Err(expected.clone()),
                "{text:?} must be rejected as {expected:?}"
            );
        }
        // Und das Gegenstück: der gute Text, unverändert.
        assert!(IntentAnchor::parse(&good).is_ok());
    }

    #[test]
    fn to_text_refuses_what_parse_would_reject() {
        let mut anchor = file_anchor();
        anchor.scope = vec!["src/**\nsource=prompt".into()];
        assert!(anchor.to_text().is_err());

        let mut anchor = file_anchor();
        anchor.scope = vec![String::new()];
        assert!(anchor.to_text().is_err());

        let mut anchor = file_anchor();
        anchor.scope = vec!["a,b".into()];
        assert!(
            anchor.to_text().is_err(),
            "a comma inside one glob would read back as two globs"
        );

        let anchor = IntentAnchor {
            source: IntentSource::File {
                path: "/abs.md".into(),
                blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
            },
            ..file_anchor()
        };
        assert!(anchor.to_text().is_err());

        let anchor = IntentAnchor {
            source: IntentSource::Issue {
                project: "a#b".into(),
                iid: 1,
                updated_at: "2026-10-01T08:15:00Z".into(),
            },
            ..file_anchor()
        };
        assert!(anchor.to_text().is_err());
    }

    #[test]
    fn a_path_may_contain_an_at_sign() {
        let anchor = IntentAnchor {
            source: IntentSource::File {
                path: "docs/@team/spec.md".into(),
                blob: "a".repeat(64),
            },
            ..file_anchor()
        };
        let text = anchor.to_text().unwrap();
        assert_eq!(IntentAnchor::parse(&text).unwrap(), anchor);
    }

    #[test]
    fn event_payload_binds_id_to_text() {
        let text = file_anchor().to_text().unwrap();
        let payload = IntentEventPayload::new(&text, None).unwrap();
        assert_eq!(payload.anchor_id, IntentAnchor::id_of_text(&text));
        assert_eq!(payload.check().unwrap(), file_anchor());
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(
                "{{\"anchor_id\":\"{}\",\"anchor\":{},\"signature\":null}}",
                payload.anchor_id,
                serde_json::to_string(&text).unwrap()
            )
        );

        let mut forged = payload.clone();
        forged.anchor_id = content_hash(b"other");
        assert_eq!(forged.check(), Err(IntentAnchorError::IdMismatch));

        assert!(IntentEventPayload::new("not an anchor\n", None).is_err());
    }

    #[test]
    fn intent_event_serializes_additively() {
        let event = IntentEvent {
            seq: 3,
            anchor_id: content_hash(b"x"),
            opens_session: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("opens_session"), "{json}");
        let back: IntentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    const SIGNATURE: &str = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAg\nAAAA+/=\n-----END SSH SIGNATURE-----\n";

    #[test]
    fn only_armored_signatures_pass() {
        assert!(is_armored_signature(SIGNATURE));
        assert!(is_armored_signature(SIGNATURE.trim_end_matches('\n')));
        for bad in [
            "",
            "sig",
            "-----BEGIN SSH SIGNATURE-----\n-----END SSH SIGNATURE-----\n",
            "-----BEGIN SSH SIGNATURE-----\nAAAA\n",
            "-----BEGIN SSH SIGNATURE-----\nAA AA\n-----END SSH SIGNATURE-----\n",
            "-----BEGIN SSH SIGNATURE-----\nfree text here\n-----END SSH SIGNATURE-----\n",
            "-----BEGIN SSH SIGNATURE-----\r\nAAAA\r\n-----END SSH SIGNATURE-----\r\n",
            "-----BEGIN SSH SIGNATURE-----\nAAAA\n\n-----END SSH SIGNATURE-----\n",
            "x-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n",
        ] {
            assert!(!is_armored_signature(bad), "{bad:?}");
        }
        let huge = format!(
            "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
            "A".repeat(MAX_SIGNATURE)
        );
        assert!(!is_armored_signature(&huge));

        let text = file_anchor().to_text().unwrap();
        assert!(IntentEventPayload::new(&text, Some(SIGNATURE.into())).is_ok());
        assert_eq!(
            IntentEventPayload::new(&text, Some("Hallo Welt".into())),
            Err(IntentAnchorError::Signature)
        );
        let mut payload = IntentEventPayload::new(&text, None).unwrap();
        payload.signature = Some("free text".into());
        assert_eq!(payload.check(), Err(IntentAnchorError::Signature));
    }

    #[test]
    fn opens_session_is_additive_and_round_trips() {
        let text = file_anchor().to_text().unwrap();
        let mut payload = IntentEventPayload::new(&text, None).unwrap();
        assert!(
            !serde_json::to_string(&payload)
                .unwrap()
                .contains("opens_session")
        );
        payload.opens_session = true;
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.ends_with(",\"opens_session\":true}"), "{json}");
        let back: IntentEventPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn paths_and_projects_that_would_mean_something_else_are_rejected() {
        let blob = "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b";
        let content = content_hash(b"x");
        let with = |source: &str| {
            IntentAnchor::parse(&format!(
                "minds-intent-v1\nsource={source}\ncontent={content}\nscope=-\n"
            ))
        };
        for source in [
            format!("file:C:/Windows/x.md@{blob}"),
            format!("file:docs/a.md:stream@{blob}"),
            format!("file:.git/config@{blob}"),
            format!("file:sub/.GIT/config@{blob}"),
            "issue:team/../admin#1@2026-10-01T08:15:00Z".into(),
            "issue:team/minds?x=1#1@2026-10-01T08:15:00Z".into(),
            "issue:team/%2e%2e#1@2026-10-01T08:15:00Z".into(),
            "issue:/team#1@2026-10-01T08:15:00Z".into(),
            "issue:team/#1@2026-10-01T08:15:00Z".into(),
        ] {
            assert_eq!(
                with(&source),
                Err(IntentAnchorError::Field("source")),
                "{source}"
            );
        }
        for source in [
            format!("file:docs/.gitignore@{blob}"),
            "issue:group/sub.group/my_project-2#7@2026-10-01T08:15:00.123+02:00".into(),
        ] {
            assert!(with(&source).is_ok(), "{source}");
        }
    }

    #[test]
    fn an_anchor_larger_than_the_store_reads_is_refused() {
        let mut anchor = file_anchor();
        anchor.scope = (0..10_000).map(|i| format!("src/{i}/**")).collect();
        assert_eq!(anchor.to_text(), Err(IntentAnchorError::TooLong));
    }

    #[test]
    fn to_text_names_the_field_that_does_not_round_trip() {
        let mut anchor = file_anchor();
        anchor.scope = vec!["a,b".into()];
        assert_eq!(anchor.to_text(), Err(IntentAnchorError::Field("scope")));
    }
}
