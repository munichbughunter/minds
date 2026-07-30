//! Herkunft, Beziehungen und Beweismittel — wer arbeitete wann, in wessen
//! Auftrag, und woher wir das wissen.
//!
//! Das Envelope aus [`crate::session`] beantwortet „was ist passiert". Dieses
//! Modul beantwortet „in welcher Ordnung, und wie sicher". Es entstand aus
//! einer konkreten Anforderung: Claude Code plant, Codex reviewt, Claude Code
//! implementiert die Review-Punkte. Drei Sessions, drei Agents, eine
//! Reihenfolge — und keines der drei Transkripte weiß vom jeweils anderen.
//!
//! # Die Leitregel: Kanten haben eine Herkunft
//!
//! Ein einzelner, ungekennzeichneter Pfeil zwischen zwei Sessions wäre eine
//! Behauptung, die wir nicht decken können. In einem Record, dessen einziger
//! Wert seine Nachweisbarkeit ist, ist das der Sündenfall. Jede [`Edge`] trägt
//! deshalb ein [`Evidence`]: beobachtet, per Inhalts-Hash belegt, von einem
//! Menschen erklärt oder heuristisch vermutet. Der Reader darf danach „sicher"
//! von „vermutet" unterscheiden; ohne dieses Feld müsste er alles gleich
//! behandeln und wäre damit im Zweifel unehrlich.
//!
//! # Symbolische Endpunkte, keine `SessionId`
//!
//! [`Endpoint::Session`] verweist über `(agent, local_id)` — nicht über eine
//! [`SessionId`](crate::SessionId). Der Grund ist hart und nicht verhandelbar:
//! Die `SessionId` ist der Hash des fertigen Envelopes. Zeigte ein Kind per
//! Hash auf seine Eltern, müsste die Eltern-Session abgeschlossen sein, bevor
//! das Kind gehasht werden kann — beim Orchestrator, der nach dem Sub-Agenten
//! weiterläuft, ist sie das nie. Ein Inhalts-Hash kann keine Vorwärtsreferenz
//! enthalten. Die Auflösung symbolisch → Hash macht der Store-Index beim
//! Schreiben; sie ist wiederholbar, die Kante selbst ist es nicht.
//!
//! # Was hier bewusst *nicht* steht: abgeleitete Kanten
//!
//! Dass Codex genau die Bytes gelesen hat, die Claude Code geschrieben hat, ist
//! eine Aussage über *zwei* Sessions. Ein Adapter sieht immer nur eine. Deshalb
//! erfasst die Capture-Schicht nur **Beweismittel** — [`Effect`] mit Pfad und
//! [`ContentHash`] am einzelnen Tool-Call — und die *Ableitung* daraus
//! ([`EdgeKind::ContinuedFrom`] mit [`Evidence::Content`]) passiert später über
//! den Store-Index, der beide Sessions kennt.
//!
//! Die Trennlinie ist der Grund für den Schnitt: **Beweismittel sind
//! unwiederbringlich, Ableitungen sind wiederholbar.** Was wir heute nicht
//! mitschreiben, ist weg. Was wir heute nicht ausrechnen, rechnen wir nächsten
//! Monat aus — dann sogar besser.
//!
//! # Hash-Stabilität
//!
//! Alle Felder, die dieses Modul dem Envelope hinzufügt, sind `Option` bzw.
//! `Vec` mit `skip_serializing_if`. Eine Session ohne Herkunft serialisiert
//! damit **byte-identisch** wie vor diesem Commit und behält ihre `SessionId`.
//! Das ist als Test formuliert, nicht bloß als Zusage (siehe
//! `session::tests::additive_fields_do_not_change_canonical_form`).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Präfix der kanonischen Textform eines [`ContentHash`] — gleiche Konvention
/// wie [`SessionId`](crate::SessionId), damit ein Leser beide auf einen Blick
/// als blake3 erkennt.
pub const CONTENT_HASH_PREFIX: &str = "b3-";

/// Länge des Hex-Anteils: blake3 liefert 32 Byte.
const HEX_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Herkunft
// ---------------------------------------------------------------------------

/// Woher eine Session stammt: ihre agent-eigene Kennung und ihr Zeitfenster.
///
/// Der Namensraum von `local_id` ist **`(Session::agent.name, local_id)`** —
/// nicht global. Zwei Agents dürfen dieselbe UUID vergeben; das kollidiert
/// nicht, weil der Agentname immer danebensteht.
///
/// Ein früherer Entwurf hatte hier zusätzlich ein `run_id` („der gemeinsame
/// Lauf"). Das war falsch: Claude Code und Codex laufen in getrennten
/// Prozessen ohne gemeinsame Klammer. Ein `run_id` wäre entweder erfunden oder
/// für jede Session verschieden und damit wertlos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    /// Die Session-/Thread-Kennung des Agenten (Claude Code: `session_id`).
    pub local_id: String,

    /// Beginn, RFC 3339 in UTC — **aus dem Transkript bzw. dem Hook-Event**,
    /// niemals `SystemTime::now()` im Adapter. Sonst wäre der Adapter nicht
    /// deterministisch und die Fixture-Tests aus M5 nicht schreibbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,

    /// Ende, gleiche Regel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,

    /// Arbeitsverzeichnis zum Zeitpunkt der Session.
    ///
    /// **Achtung, Redaction:** Dieser Wert enthält in aller Regel einen
    /// Benutzernamen (`/Users/anna/…`, `/home/anna/…`) und ist damit PII. Die
    /// Pipeline muss ihn scannen; das ist kein optionales Feld für den
    /// Feldlauf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl Lineage {
    /// Herkunft mit nur der Kennung; Zeitfenster und `cwd` folgen, wenn der
    /// Adapter sie belegen kann.
    pub fn new(local_id: impl Into<String>) -> Self {
        Self {
            local_id: local_id.into(),
            started_at: None,
            ended_at: None,
            cwd: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Zeitangaben
// ---------------------------------------------------------------------------
//
// Zeitpunkte stehen im Envelope als schlichter `String` (RFC 3339, UTC).
//
// Ein validierender Newtype wäre verlockend, wäre hier aber schlechter: Eine
// reine Formprüfung ohne echten Parser gäbe falsche Sicherheit, und ein echter
// Parser hieße eine Datums-Dependency in `minds-core` — einem Crate, das
// bewusst nur `serde` und `blake3` kennt und kein I/O hat.
//
// Daraus folgt eine Regel, die *nicht* aus dem Typsystem kommt und deshalb hier
// stehen muss: **Zeitstempel werden nie per String-Vergleich sortiert.** Das
// wäre nur bei durchnormalisiertem Format korrekt (gleiche Nachkommastellen,
// immer `Z`), und normalisiert ist hier nichts — was der Agent liefert, wird
// treu übernommen. Die Ordnungsrelation gehört in die Ableitungsschicht und
// parst dort richtig.

// ---------------------------------------------------------------------------
// Kanten
// ---------------------------------------------------------------------------

/// Eine gerichtete Beziehung *von dieser Session* zu etwas anderem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Art der Beziehung. Die Richtung steckt im Namen ([`EdgeKind::Spawned`]
    /// vs. [`EdgeKind::SpawnedBy`]), damit kein zusätzliches
    /// Richtungs-Feld nötig ist, das man vergessen kann.
    pub kind: EdgeKind,

    /// Das andere Ende.
    pub to: Endpoint,

    /// Woher wir diese Kante wissen.
    pub evidence: Evidence,
}

/// Was für eine Beziehung.
///
/// Bewusst klein gehalten und auf **Session↔Session** und **Session↔Commit**
/// beschränkt. Kanten zu Dateien gibt es hier absichtlich nicht: Dass eine
/// Session `Plan.md` gelesen hat, steht bereits als [`Effect`] am Tool-Call und
/// wäre hier eine Denormalisierung, die auseinanderlaufen kann.
///
/// Geschlossen für Schema v1 — wie [`Role`](crate::Role). Eine neue Variante
/// wäre eine Schema-Änderung mit Versions-Bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Diese Session ist ein Sub-Agent von `to`.
    SpawnedBy,

    /// Diese Session hat `to` als Sub-Agent gestartet.
    Spawned,

    /// Diese Session setzt die Arbeit von `to` fort — der Übergabefall
    /// („Claude plant, Codex reviewt"). Kommt entweder als
    /// [`Evidence::Declared`] (jemand sagte `--after`) oder als
    /// [`Evidence::Content`] (die gelesenen Bytes sind die geschriebenen).
    ContinuedFrom,

    /// Diese Session hat zu `to` (einem Commit) geführt.
    Produced,
}

/// Das andere Ende einer [`Edge`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Endpoint {
    /// Eine andere Session, **symbolisch** referenziert. Siehe Modul-Doku:
    /// eine `SessionId` wäre eine Vorwärtsreferenz und damit unmöglich.
    Session {
        /// Agentname, wie in [`Agent::name`](crate::Agent::name).
        agent: String,
        /// Die agent-eigene Kennung, wie in [`Lineage::local_id`].
        local_id: String,
    },

    /// Ein Git-Commit, als voller Hex-Hash.
    Commit {
        /// Commit-Hash in Hex.
        id: String,
    },
}

/// Woher eine Kante bekannt ist — aufsteigend nach Verlässlichkeit sortiert,
/// damit `max()` über mehrere Belege das Richtige tut.
///
/// Die Reihenfolge der Varianten ist deshalb Teil des Vertrags und nicht
/// alphabetisch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Heuristik — z. B. Textähnlichkeit zwischen zwei Prompts. Im Reader grau
    /// statt schwarz.
    Inferred,

    /// Ein Mensch hat es behauptet (`minds capture --after <id>`). Auch das ist
    /// eine Tatsache, nur eine über den Menschen.
    Declared,

    /// Nachrechenbar: die von B gelesenen Bytes sind exakt die von A
    /// geschriebenen. Kein Zeitstempel nötig, keine Uhr im Spiel.
    Content,

    /// Stand so im Transkript bzw. im Hook-Event (`parentUuid`, `SubagentStop`,
    /// zwei Hook-Aufrufe im selben Journal). Der Beobachter hat es gesehen.
    Observed,
}

// ---------------------------------------------------------------------------
// Beweismittel am Tool-Call
// ---------------------------------------------------------------------------

/// Was ein Tool-Call in der Welt getan hat, normalisiert.
///
/// Zwei Schulden auf einmal:
///
/// 1. `minds-redact::secretfile` kündigt seit M2 an, dass die Mauer vor
///    Zugangsdaten-Dateien „in `minds-capture` durchgesetzt" wird — dafür
///    braucht der Adapter den Pfad als normalisiertes Feld, nicht vergraben im
///    agent-spezifischen `arguments`-String.
/// 2. Die Übergabe zwischen zwei Agents wird über `path` + `content`
///    nachrechenbar (siehe [`Evidence::Content`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    /// Art des Zugriffs.
    pub kind: EffectKind,

    /// Betroffener Pfad, repo-relativ wo möglich.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// blake3 über die **Rohbytes** des Gelesenen bzw. Geschriebenen.
    ///
    /// Zwei Regeln, die beide fail-closed sind:
    ///
    /// - Der Hash wird **vor** der Redaction gebildet. Danach gebildet würde er
    ///   nie matchen, und die ganze Kante wäre tot.
    /// - Für Dateien, die die Secretfile-Mauer trifft (`.env`, `id_rsa`,
    ///   `*.pem`), wird er **nicht** gebildet. Bei einer kurzen, ratbaren Datei
    ///   wäre ein Hash ein Orakel — man probiert Kandidaten durch, bis es
    ///   passt. Fail-closed gilt auch für Fingerabdrücke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentHash>,
}

/// Art eines [`Effect`].
///
/// `Other` existiert, weil Tool-Vokabulare wachsen und ein unbekanntes Tool
/// keinen Adapter-Fehler auslösen soll. Geschlossen im Sinne des Schemas: die
/// Menge ändert sich nicht ohne Versions-Bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Read,
    Write,
    Delete,
    Exec,
    Other,
}

// ---------------------------------------------------------------------------
// ContentHash
// ---------------------------------------------------------------------------

/// blake3 über einen Byte-Strom, in derselben Textform wie
/// [`SessionId`](crate::SessionId): `b3-` plus 64 Hex-Zeichen, klein
/// geschrieben.
///
/// Eigener Typ statt `SessionId`, obwohl das Format identisch ist: Ein
/// Datei-Inhalt ist keine Session. Die beiden zu vermischen hieße, dass man sie
/// versehentlich vergleichen oder verwechseln kann — und der Store könnte einen
/// Datei-Hash als Session-Referenz auflösen wollen.
///
/// **Lesen tolerant, Schreiben kanonisch** (wie im restlichen Crate): [`FromStr`]
/// akzeptiert Groß- und Kleinschreibung sowie ein fehlendes Präfix,
/// [`fmt::Display`] gibt ausschließlich die kanonische Form aus.
///
/// # Verhältnis zur Redaction
///
/// Eine berechtigte Sorge: Frisst der `HighEntropyRedactor` unsere eigenen
/// Hashes? Nein — Hex trägt höchstens 4 bit pro Zeichen und bleibt damit unter
/// der Entropieschwelle. Genau dieser Fall ist in
/// `minds-redact::session` bereits als Begründung dafür dokumentiert, warum die
/// Ausnahmeliste leer bleiben darf.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Aus 32 Rohbytes (der üblichen blake3-Ausgabe).
    pub fn from_bytes(digest: [u8; 32]) -> Self {
        let mut s = String::with_capacity(CONTENT_HASH_PREFIX.len() + HEX_LEN);
        s.push_str(CONTENT_HASH_PREFIX);
        for b in digest {
            // Kleinbuchstaben, feste Breite — die kanonische Form.
            s.push(char::from_digit((b >> 4) as u32, 16).expect("nibble"));
            s.push(char::from_digit((b & 0x0f) as u32, 16).expect("nibble"));
        }
        Self(s)
    }

    /// Der Hex-Anteil ohne Präfix.
    pub fn hex(&self) -> &str {
        &self.0[CONTENT_HASH_PREFIX.len()..]
    }

    /// Die kanonische Textform mit Präfix.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ContentHash> for String {
    fn from(h: ContentHash) -> Self {
        h.0
    }
}

impl TryFrom<String> for ContentHash {
    type Error = ContentHashParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Praefix case-insensitiv abstreifen (`b3-` wie `B3-`), damit „Lesen
        // tolerant" auch fuer die Grossschreibung gilt. Ein blosser Hex-String
        // ohne Praefix bleibt unberuehrt: Seine ersten drei Zeichen koennen
        // `b3-` nie gleichen, weil Hex kein `-` enthaelt.
        let hex = match s.get(..CONTENT_HASH_PREFIX.len()) {
            Some(p) if p.eq_ignore_ascii_case(CONTENT_HASH_PREFIX) => {
                &s[CONTENT_HASH_PREFIX.len()..]
            }
            _ => s,
        };
        if hex.len() != HEX_LEN {
            return Err(ContentHashParseError::Length(hex.len()));
        }
        if let Some(c) = hex.chars().find(|c| !c.is_ascii_hexdigit()) {
            return Err(ContentHashParseError::NotHex(c));
        }
        Ok(Self(format!(
            "{CONTENT_HASH_PREFIX}{}",
            hex.to_ascii_lowercase()
        )))
    }
}

/// Warum eine Zeichenkette kein [`ContentHash`] ist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentHashParseError {
    #[error("Content-Hash braucht {HEX_LEN} Hex-Zeichen, hat aber {0}")]
    Length(usize),

    #[error("Content-Hash enthält ein Nicht-Hex-Zeichen: {0:?}")]
    NotHex(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_reads_tolerantly_writes_canonically() {
        let upper = "B3-".to_string() + &"AB".repeat(32);
        let bare = "ab".repeat(32);

        let a: ContentHash = upper.parse().unwrap();
        let b: ContentHash = bare.parse().unwrap();

        // Grossschreibung und fehlendes Praefix fuehren auf dieselbe Form.
        assert_eq!(a, b);
        assert_eq!(a.to_string(), format!("b3-{}", "ab".repeat(32)));
        assert_eq!(a.hex().len(), HEX_LEN);
    }

    #[test]
    fn content_hash_rejects_wrong_shape() {
        assert!(matches!(
            "b3-abc".parse::<ContentHash>(),
            Err(ContentHashParseError::Length(3))
        ));
        assert!(matches!(
            ("zz".repeat(32)).parse::<ContentHash>(),
            Err(ContentHashParseError::NotHex('z'))
        ));
    }

    #[test]
    fn content_hash_from_bytes_matches_parse() {
        let digest = [0xabu8; 32];
        let from_bytes = ContentHash::from_bytes(digest);
        let parsed: ContentHash = ("ab".repeat(32)).parse().unwrap();
        assert_eq!(from_bytes, parsed);
    }

    #[test]
    fn content_hash_roundtrips_through_serde_as_a_plain_string() {
        let h = ContentHash::from_bytes([0x01; 32]);
        let json = serde_json::to_string(&h).unwrap();
        // Kein Objekt, keine Wrapper — eine schlichte Zeichenkette. Das haelt
        // die kanonische JSON-Form klein und fuer Fremdleser offensichtlich.
        assert!(json.starts_with("\"b3-"));
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn evidence_orders_by_trustworthiness() {
        // Die Variantenreihenfolge ist Vertrag, nicht Zufall: `max()` ueber
        // mehrere Belege muss den staerksten liefern.
        assert!(Evidence::Observed > Evidence::Content);
        assert!(Evidence::Content > Evidence::Declared);
        assert!(Evidence::Declared > Evidence::Inferred);
    }

    #[test]
    fn endpoint_is_tagged_so_a_reader_can_branch() {
        let e = Endpoint::Session {
            agent: "claude-code".into(),
            local_id: "31f3f224".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"session""#));

        let c = Endpoint::Commit {
            id: "deadbeef".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""type":"commit""#));
    }
}
