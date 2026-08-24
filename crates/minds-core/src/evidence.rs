//! Die Evidence-Chain-Primitive: Hashes über Beobachtetes, Lücken als
//! Kettenglieder, ein Fold zum Root (ADR-0011).
//!
//! Reine Funktionen, kein I/O — dieselbe Rolle wie [`crate::canonical`] für
//! Sessions: Ein externer Prüfer (Python, ein Shell-Skript, ein Auditor ohne
//! Minds) muss jeden Hash hier aus den Rohdaten nachrechnen können.
//!
//! # Warum nicht die kanonische JSON-Form?
//!
//! [`crate::canonical`] lehnt Ganzzahlen jenseits von ±(2⁵³−1) ab — mit
//! Absicht, wegen JCS. Ein Journal-Event trägt aber `at_nanos` (~1,7·10¹⁸),
//! weit darüber. Genau deshalb wurde das Journal-Format bisher „nie gehasht".
//! Die Kodierung hier ist stattdessen binär und längenpräfixiert: je Feld
//! eine u64-Länge (little-endian) plus die Bytes, Optionen mit einem
//! Tag-Byte. Keine Escapes, keine Zahlformatierung, keine Injektion — zwei
//! verschiedene Feldfolgen können nie dieselben Bytes ergeben.
//!
//! # Domain Separation
//!
//! Jede Hash-Sorte läuft über `blake3::derive_key` mit eigenem Kontext-String
//! ([`CTX_PAYLOAD`] …). Ein Payload-Hash kann dadurch nie als Event-Hash
//! durchgehen und umgekehrt — auch nicht bei identischem Input.
//!
//! # Was gehasht wird — und was nicht
//!
//! Gehasht werden nur **beobachtete** Fakten ([`EventFacts`]): Sequenz, Zeit,
//! roher Event-Name, Payload-Hash. Die Klassifikation (`kind`) ist
//! Interpretation und bleibt draußen — Interpretation ist wiederholbar und
//! darf sich ändern, ohne die Evidence zu brechen. Der Payload-Hash entsteht
//! über den Payload **nach** der Secretwall; für Secret-Dateien existiert
//! damit nie ein Hash über geheimen Inhalt (Orakel-Regel, siehe
//! [`Effect::content`](crate::Effect)).
//!
//! # Eine Lücke ist ein Kettenglied
//!
//! Der Fold ([`chain`]) nimmt Events **und** [`GapRecord`]s. Eine erkannte
//! Lücke steht damit selbst in der kryptographischen Geschichte — wer sie
//! wegließe, bekäme einen anderen Root. „Da war halt nichts" ist keine
//! mögliche Behauptung mehr; möglich ist nur „nicht erfasst", explizit.

use crate::ContentHash;

// ---------------------------------------------------------------------------
// Domain-Kontexte
// ---------------------------------------------------------------------------
//
// Versioniert im String (`v1`): Eine künftige Format-Änderung bekommt neue
// Kontexte, alte Hashes bleiben nachrechenbar.

/// Kontext für den Hash über den (gewallten) Roh-Payload eines Events.
pub const CTX_PAYLOAD: &str = "minds/evidence/v1/payload";

/// Kontext für den Hash über die beobachteten Fakten eines Events.
pub const CTX_EVENT: &str = "minds/evidence/v1/event";

/// Kontext für den Hash über einen [`GapRecord`].
pub const CTX_GAP: &str = "minds/evidence/v1/gap";

/// Kontext für jeden Schritt des Chain-Folds.
pub const CTX_CHAIN: &str = "minds/evidence/v1/chain";

/// Kontext für die Identität eines Seals (`seal_id` über seine Bytes).
pub const CTX_SEAL: &str = "minds/evidence/v1/seal";

/// Fold-Tag: das Glied ist ein Event mit gestempeltem Hash.
const TAG_EVENT: u8 = 0x01;

/// Fold-Tag: das Glied ist eine Lücke.
const TAG_GAP: u8 = 0x02;

/// Fold-Tag: das Glied ist ein Alt-Event ohne gestempelte Hashes (Bestand vor
/// der Evidence-Chain).
const TAG_PRE_CHAIN: u8 = 0x03;

// ---------------------------------------------------------------------------
// Hashes
// ---------------------------------------------------------------------------

/// Der Hash über den Roh-Payload eines Events — **nach** der Secretwall.
pub fn payload_hash(raw: &[u8]) -> ContentHash {
    ContentHash::from_bytes(blake3::derive_key(CTX_PAYLOAD, raw))
}

/// Die beobachteten Fakten eines Journal-Events — genau die Felder, die
/// gehasht werden.
///
/// Bewusst **ohne** `kind`: Die Klassifikation ist Interpretation. `raw_kind`
/// dagegen ist der wörtliche Name aus dem Hook und damit Beobachtung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFacts<'a> {
    /// Die Sequenznummer, wie vom Journal vergeben.
    pub seq: u64,

    /// Zeitstempel (RFC 3339), wie beobachtet.
    pub at: &'a str,

    /// Nanosekunden-Sortierschlüssel, wie beobachtet.
    pub at_nanos: u64,

    /// Der wörtliche Event-Name des Agenten.
    pub raw_kind: &'a str,

    /// Arbeitsverzeichnis, falls im Event vorhanden.
    pub cwd: Option<&'a str>,

    /// Transkript-Pfad, falls im Event vorhanden.
    pub transcript_path: Option<&'a str>,

    /// Der [`payload_hash`] des Events.
    pub payload_hash: &'a ContentHash,
}

/// Der Hash über die beobachteten Fakten eines Events.
pub fn event_hash(facts: &EventFacts<'_>) -> ContentHash {
    let mut buf = Vec::with_capacity(160);
    put_u64(&mut buf, facts.seq);
    put_bytes(&mut buf, facts.at.as_bytes());
    put_u64(&mut buf, facts.at_nanos);
    put_bytes(&mut buf, facts.raw_kind.as_bytes());
    put_opt(&mut buf, facts.cwd.map(str::as_bytes));
    put_opt(&mut buf, facts.transcript_path.map(str::as_bytes));
    put_bytes(&mut buf, facts.payload_hash.as_str().as_bytes());
    ContentHash::from_bytes(blake3::derive_key(CTX_EVENT, &buf))
}

/// Eine Lücke im beobachteten Bereich — selbst Evidence, kein Schweigen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapRecord {
    /// Sequenznummern, die zwischen erstem und letztem gelesenen Event fehlen
    /// (beide Grenzen einschließlich).
    Missing {
        /// Erste fehlende Sequenznummer.
        from: u64,
        /// Letzte fehlende Sequenznummer.
        to: u64,
    },

    /// Eine Datei, die da ist, aber kein lesbares Event trägt — leere
    /// Reservierung, `.tmp`-Rest, kaputtes JSON.
    Damaged {
        /// Die Sequenznummer, falls aus dem Dateinamen ablesbar.
        seq: Option<u64>,
        /// Hash über die vorgefundenen Bytes, falls es welche gab — damit
        /// auch der Schaden selbst adressierbar ist.
        bytes: Option<ContentHash>,
    },
}

/// Der Hash über einen [`GapRecord`].
pub fn gap_hash(gap: &GapRecord) -> ContentHash {
    let mut buf = Vec::with_capacity(48);
    match gap {
        GapRecord::Missing { from, to } => {
            buf.push(0x01);
            put_u64(&mut buf, *from);
            put_u64(&mut buf, *to);
        }
        GapRecord::Damaged { seq, bytes } => {
            buf.push(0x02);
            match seq {
                None => buf.push(0x00),
                Some(seq) => {
                    buf.push(0x01);
                    put_u64(&mut buf, *seq);
                }
            }
            put_opt(&mut buf, bytes.as_ref().map(|h| h.as_str().as_bytes()));
        }
    }
    ContentHash::from_bytes(blake3::derive_key(CTX_GAP, &buf))
}

// ---------------------------------------------------------------------------
// Der Fold
// ---------------------------------------------------------------------------

/// Ein Glied der Kette, in Seq-Reihenfolge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainItem {
    /// Ein Event mit gestempeltem [`event_hash`].
    Event {
        /// Seine Sequenznummer.
        seq: u64,
        /// Sein gestempelter Hash.
        hash: ContentHash,
    },

    /// Ein Alt-Event ohne gestempelte Hashes. Es zählt zur Coverage (es wurde
    /// gelesen), aber sein Inhalt ist nicht gebunden — der Seal weist die
    /// Zahl solcher Glieder als [`Coverage::pre_chain`] aus.
    PreChain {
        /// Seine Sequenznummer.
        seq: u64,
    },

    /// Eine Lücke.
    Gap(GapRecord),
}

/// Was ein Seal über seinen Bereich aussagt — nur über den tatsächlich
/// gelesenen Bereich, nie mehr (Crash-Ehrlichkeit, ADR-0011 Entscheidung 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// Kleinste gelesene Sequenznummer (0, wenn nichts gelesen wurde).
    pub first_seq: u64,

    /// Größte gelesene Sequenznummer (0, wenn nichts gelesen wurde).
    pub last_seq: u64,

    /// Zahl der Events mit gestempeltem Hash.
    pub events: u64,

    /// Die Lücken, in Kettenreihenfolge.
    pub gaps: Vec<GapRecord>,

    /// Zahl der Alt-Events ohne gestempelte Hashes.
    pub pre_chain: u64,
}

impl Coverage {
    /// Ohne bekannte Lücken **und** ohne ungebundene Alt-Events?
    ///
    /// Das ist die Integritäts-Hälfte von „Coverage vollständig"; ob die
    /// Epochenkette geschlossen ist und die Session gespeichert wurde, wissen
    /// erst Seal und Verifier.
    pub fn is_gap_free(&self) -> bool {
        self.gaps.is_empty() && self.pre_chain == 0
    }
}

/// Root und Coverage eines Folds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainResult {
    /// Der Chain-Root über alle Glieder.
    pub root: ContentHash,

    /// Was der Bereich abdeckt.
    pub coverage: Coverage,
}

/// Faltet die Glieder in gegebener Reihenfolge zum Root — Start sind 32
/// Nullbytes. Für Seals, die eine Forge erreichen, gehört stattdessen
/// [`chain_salted`] verwendet (Anti-Orakel, siehe dort); die ungesalzene Form
/// bleibt für lokale Nachrechnung und Golden-Vektoren.
pub fn chain(items: &[ChainItem]) -> ChainResult {
    chain_from([0u8; 32], items)
}

/// Wie [`chain`], aber der Fold startet auf `derive_key(CTX_CHAIN, salt)`.
///
/// **Warum ein Salt:** Der Root reist im Seal auf die Forge, und `seq`,
/// `last_event_at` und Teile der Fakten stehen dort im Klartext daneben. Ohne
/// Salt wäre der Root für eine Ein-Event-Epoche ein Offline-Orakel: Wer den
/// Payload rät (kurzes Passwort, PIN), kann den Root nachrechnen und die
/// Vermutung bestätigen. Der Salt ist **lokal** (er liegt neben dem
/// Epochen-Zustand, wird nie gepusht) und macht genau das unmöglich, ohne die
/// lokale Nachrechnung zu verlieren — `fsck` kann ihn lesen. Der Preis steht
/// im Nachweis-Leitfaden: Ein Externer rechnet den Root nicht aus geratenen
/// Payloads nach — das ist hier der Zweck, kein Mangel.
pub fn chain_salted(salt: &[u8; 32], items: &[ChainItem]) -> ChainResult {
    chain_from(blake3::derive_key(CTX_CHAIN, salt), items)
}

/// Der Fold selbst: `h_i = derive_key(CTX_CHAIN, h_{i-1} ‖ tag ‖ glied)`.
/// Ein Event trägt seine 32 Hash-Rohbytes bei, eine Lücke ihren
/// [`gap_hash`], ein Alt-Event seine Sequenznummer. Wer ein Glied weglässt,
/// umsortiert oder umdeutet, bekommt einen anderen Root.
///
/// Der Aufrufer liefert die Glieder in Seq-Reihenfolge (das Journal liest
/// sortiert); die Funktion ordnet nicht um — sie bindet die **gegebene**
/// Reihenfolge.
fn chain_from(start: [u8; 32], items: &[ChainItem]) -> ChainResult {
    let mut state = start;
    let mut first_seq: Option<u64> = None;
    let mut last_seq: u64 = 0;
    let mut events: u64 = 0;
    let mut pre_chain: u64 = 0;
    let mut gaps = Vec::new();

    for item in items {
        let mut buf = Vec::with_capacity(80);
        buf.extend_from_slice(&state);
        match item {
            ChainItem::Event { seq, hash } => {
                buf.push(TAG_EVENT);
                buf.extend_from_slice(&hash.to_bytes());
                events += 1;
                first_seq.get_or_insert(*seq);
                last_seq = last_seq.max(*seq);
            }
            ChainItem::PreChain { seq } => {
                buf.push(TAG_PRE_CHAIN);
                put_u64(&mut buf, *seq);
                pre_chain += 1;
                first_seq.get_or_insert(*seq);
                last_seq = last_seq.max(*seq);
            }
            ChainItem::Gap(gap) => {
                buf.push(TAG_GAP);
                buf.extend_from_slice(&gap_hash(gap).to_bytes());
                gaps.push(gap.clone());
            }
        }
        state = blake3::derive_key(CTX_CHAIN, &buf);
    }

    ChainResult {
        root: ContentHash::from_bytes(state),
        coverage: Coverage {
            first_seq: first_seq.unwrap_or(0),
            last_seq,
            events,
            gaps,
            pre_chain,
        },
    }
}

// ---------------------------------------------------------------------------
// Der Seal
// ---------------------------------------------------------------------------

/// Versionszeile des Seal-Formats.
pub const SEAL_VERSION: &str = "minds-seal-v1";

/// Zeilenzahl des Seal-Textes — testfixiert wie bei den Attestation-Payloads:
/// Eine Zeile mehr oder weniger ist ein Format-Bruch, kein Zufall.
pub const SEAL_LINES: usize = 13;

/// Die Beobachtungsgrenze der heutigen Erfassung: Agent-Hooks, Version 1.
///
/// „Vollständig" heißt immer **vollständig innerhalb dieser Grenze** — 100 %
/// Journal-Coverage sind nicht 100 % Systemaktivität. Ein Subprozess, den der
/// Agent startet, ein Netzwerkeffekt, ein Plugin außerhalb der Hooks: alles
/// jenseits der Grenze, und der Seal behauptet nichts darüber. Die Version
/// steigt, wenn sich die Grenze selbst ändert (andere Hook-Menge, andere
/// Quelle) — damit ein Prüfer weiß, *welche* Grenze „vollständig" meinte.
pub const SCOPE_AGENT_HOOKS_V1: &str = "agent-hooks/v1";

/// Was der Checkpoint mit der Session gemacht hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealOutcome {
    /// Session redigiert und gespeichert; die Zeile `session=` trägt ihre Id.
    Stored {
        /// Die `SessionId` in Textform (`b3-…`).
        session: String,
    },

    /// Die Speicher-Policy hat die Nutzlast zurückgewiesen (fail-closed
    /// Redaction). Es gibt keine Session und keine SessionId — der Seal ist
    /// der einzige Beleg, dass der Bereich existierte (ADR-0011,
    /// Entscheidung 3).
    Rejected,
}

/// Der Coverage-Seal eines Checkpoint-Laufs: was versiegelt wurde, worauf es
/// folgt, was daraus wurde.
///
/// Eine **Textform** für alles — Identität (`seal_id` = Hash über die Bytes),
/// Ablage und Signatur laufen über dieselben Bytes; JSON daneben gäbe zwei
/// Wahrheiten, die auseinanderlaufen können. Deterministisch: Die Zeitzeile
/// stammt aus dem letzten Event, nie aus der Wanduhr — gleiche Events ⇒
/// gleicher Seal ⇒ idempotente Ablage per Content Addressing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seal {
    /// Chain-Root über Events und Gap-Records ([`chain`]).
    pub root: ContentHash,

    /// Der Agent der Session (`claude-code`, …).
    pub agent: String,

    /// Die Beobachtungsgrenze, innerhalb derer die Coverage-Aussage gilt
    /// (Invariante: Coverage ist immer gescoped). Heute
    /// [`SCOPE_AGENT_HOOKS_V1`].
    pub scope: String,

    /// Der tatsächlich gelesene Bereich — nie mehr (Crash-Ehrlichkeit).
    pub first_seq: u64,

    /// Ende des Bereichs.
    pub last_seq: u64,

    /// Events mit gestempeltem Hash.
    pub events: u64,

    /// Zahl der Gap-Glieder in der Kette.
    pub gaps: u64,

    /// Alt-Events ohne Stempel.
    pub pre_chain: u64,

    /// Gespeichert oder zurückgewiesen.
    pub outcome: SealOutcome,

    /// `seal_id` der vorherigen Epoche derselben Session, falls bekannt.
    /// `None` heißt: Epochenkette hier nicht belegt — ein ehrlicher Zustand,
    /// kein Fehler (frischer Clone, erste Epoche).
    pub previous: Option<ContentHash>,

    /// Zeitstempel des letzten Events (RFC 3339) — beobachtete Zeit, keine
    /// Wanduhr.
    pub last_event_at: String,
}

impl Seal {
    /// Die Textform — genau [`SEAL_LINES`] Zeilen, jede `schlüssel=wert`,
    /// abgeschlossen mit `\n`.
    ///
    /// Fail-closed gegen Zeilen-Fälschung wie die Attestation-Payloads (#12):
    /// Die einzigen Freitextfelder (`agent`, `last_event_at`) werden auf
    /// Einzeiligkeit und Versteckzeichen geprüft; alle übrigen Zeilen sind
    /// per Konstruktion einzeilig (Hashes, Zahlen).
    pub fn to_text(&self) -> Result<String, crate::PayloadError> {
        crate::attest::check_single_line("agent", &self.agent)?;
        crate::attest::check_single_line("scope", &self.scope)?;
        crate::attest::check_single_line("last_event_at", &self.last_event_at)?;
        let session = match &self.outcome {
            SealOutcome::Stored { session } => session.as_str(),
            SealOutcome::Rejected => "-",
        };
        let outcome = match &self.outcome {
            SealOutcome::Stored { .. } => "stored",
            SealOutcome::Rejected => "storage_policy_rejected_payload",
        };
        let previous = match &self.previous {
            Some(id) => id.as_str(),
            None => "-",
        };
        Ok(format!(
            "{SEAL_VERSION}\n\
             root={root}\n\
             agent={agent}\n\
             scope={scope}\n\
             first_seq={first_seq}\n\
             last_seq={last_seq}\n\
             events={events}\n\
             gaps={gaps}\n\
             pre_chain={pre_chain}\n\
             outcome={outcome}\n\
             session={session}\n\
             previous={previous}\n\
             last_event_at={last_event_at}\n",
            root = self.root,
            agent = self.agent,
            scope = self.scope,
            first_seq = self.first_seq,
            last_seq = self.last_seq,
            events = self.events,
            gaps = self.gaps,
            pre_chain = self.pre_chain,
            last_event_at = self.last_event_at,
        ))
    }

    /// Die Identität des Seals: `derive_key(CTX_SEAL, text)`.
    pub fn id_of_text(text: &str) -> ContentHash {
        ContentHash::from_bytes(blake3::derive_key(CTX_SEAL, text.as_bytes()))
    }

    /// Liest die Textform zurück — strikt: exakt [`SEAL_LINES`] Zeilen,
    /// bekannte Version, jede Zeile mit ihrem Schlüssel. Ein Seal ist unser
    /// eigenes kanonisches Artefakt; Toleranz wäre hier keine Freundlichkeit,
    /// sondern eine Angriffsfläche.
    pub fn parse(text: &str) -> Result<Self, SealParseError> {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() != SEAL_LINES {
            return Err(SealParseError::Lines(lines.len()));
        }
        if lines[0] != SEAL_VERSION {
            return Err(SealParseError::Version);
        }
        fn field<'a>(line: &'a str, key: &'static str) -> Result<&'a str, SealParseError> {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or(SealParseError::Field(key))
        }
        fn num(line: &str, key: &'static str) -> Result<u64, SealParseError> {
            field(line, key)?
                .parse()
                .map_err(|_| SealParseError::Field(key))
        }
        // Symmetrie zum Schreibpfad: `to_text` prüft die Freitextfelder
        // fail-closed (#12) — der Parser muss es AUCH tun, denn die seal_id
        // ist nur ein Hash über beliebige Bytes. Ein handgebauter, hash-
        // valider Seal mit Steuer-/Versteckzeichen im scope würde sonst von
        // `verify` roh ins Terminal (und ins CI-Log) gedruckt.
        fn clean(field_name: &'static str, value: &str) -> Result<String, SealParseError> {
            crate::attest::check_single_line(field_name, value)
                .map_err(|_| SealParseError::Field(field_name))?;
            Ok(value.to_string())
        }
        let root: ContentHash = field(lines[1], "root")?
            .parse()
            .map_err(|_| SealParseError::Field("root"))?;
        let agent = clean("agent", field(lines[2], "agent")?)?;
        let scope = clean("scope", field(lines[3], "scope")?)?;
        if scope.is_empty() {
            // Invariante: Coverage ist immer gescoped — ein Seal ohne Grenze
            // wäre eine Vollständigkeits-Behauptung ohne Bezugsrahmen.
            return Err(SealParseError::Field("scope"));
        }
        let first_seq = num(lines[4], "first_seq")?;
        let last_seq = num(lines[5], "last_seq")?;
        let events = num(lines[6], "events")?;
        let gaps = num(lines[7], "gaps")?;
        let pre_chain = num(lines[8], "pre_chain")?;
        let outcome_word = field(lines[9], "outcome")?;
        let session_word = field(lines[10], "session")?;
        let outcome = match (outcome_word, session_word) {
            // Die Form streng pruefen: Ein Seal traegt nichts Tilgbares — auch
            // nicht in der session-Zeile. Ein token-foermiger Wert wuerde sonst
            // abgelegt, gesynct und von audit/fsck weiterverbreitet.
            ("stored", id) if id.parse::<crate::SessionId>().is_ok() => SealOutcome::Stored {
                session: id.to_string(),
            },
            ("storage_policy_rejected_payload", "-") => SealOutcome::Rejected,
            _ => return Err(SealParseError::Field("outcome")),
        };
        let previous = match field(lines[11], "previous")? {
            "-" => None,
            id => Some(id.parse().map_err(|_| SealParseError::Field("previous"))?),
        };
        let last_event_at = clean("last_event_at", field(lines[12], "last_event_at")?)?;
        Ok(Seal {
            root,
            agent,
            scope,
            first_seq,
            last_seq,
            events,
            gaps,
            pre_chain,
            outcome,
            previous,
            last_event_at,
        })
    }
}

/// Warum ein Text kein Seal ist. Nennt Zeile bzw. Feld, zitiert nie den Wert.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SealParseError {
    /// Falsche Zeilenzahl.
    #[error("Seal braucht {SEAL_LINES} Zeilen, hat aber {0}")]
    Lines(usize),

    /// Unbekannte Versionszeile.
    #[error("unbekannte Seal-Version")]
    Version,

    /// Eine Zeile trägt nicht den erwarteten Schlüssel oder keinen gültigen
    /// Wert.
    #[error("Seal-Zeile {0} fehlt oder ist ungültig")]
    Field(&'static str),
}

// ---------------------------------------------------------------------------
// Kodierung
// ---------------------------------------------------------------------------

/// u64, little-endian, feste 8 Bytes.
fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Länge (u64 LE) plus Bytes.
fn put_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    put_u64(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

/// Tag-Byte 0x00 (fehlt) oder 0x01 plus Länge und Bytes.
fn put_opt(buf: &mut Vec<u8>, bytes: Option<&[u8]>) {
    match bytes {
        None => buf.push(0x00),
        Some(bytes) => {
            buf.push(0x01);
            put_bytes(buf, bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_facts(payload: &ContentHash) -> EventFacts<'_> {
        EventFacts {
            seq: 42,
            at: "2026-08-24T10:15:00.441Z",
            at_nanos: 1_787_912_100_441_000_000,
            raw_kind: "PostToolUse",
            cwd: Some("/work/repo"),
            transcript_path: None,
            payload_hash: payload,
        }
    }

    // --- Golden-Tests: eingefrorene Known-Answer-Vektoren --------------------
    //
    // Dieselbe Begründung wie bei `id::tests`: Relative Tests blieben grün,
    // wenn sich Kodierung oder Kontexte *konsistent* änderten — für extern
    // nachrechenbare Hashes wäre genau das der Bruch. Neu erzeugen mit:
    //   cargo test -p minds-core -- --ignored --nocapture evidence_reference

    const GOLDEN_PAYLOAD_HASH: &str =
        "b3-95f7a055d99f2723278fd9dc0176f2a3f4d880bfcece3030a056d4af208a4783";
    const GOLDEN_EVENT_HASH: &str =
        "b3-03d71e9b8ab41ec912ab50a3e3b5129befb8adfc52eb20bda508e4afdff885c2";
    const GOLDEN_GAP_HASH: &str =
        "b3-dc14f263e5efbef2e3f598bca2d4a919dd8534c35a4789c3cf39b566fd31319c";
    const GOLDEN_CHAIN_ROOT: &str =
        "b3-1676980fced8f11c73cc9ed58294c90c9c141ad6fb0c1a8004c86c7dc666a685";

    fn golden_items() -> Vec<ChainItem> {
        let payload = payload_hash(b"{\"tool_name\":\"Read\"}");
        vec![
            ChainItem::Event {
                seq: 0,
                hash: event_hash(&EventFacts {
                    seq: 0,
                    ..sample_facts(&payload)
                }),
            },
            ChainItem::PreChain { seq: 1 },
            ChainItem::Gap(GapRecord::Missing { from: 2, to: 3 }),
            ChainItem::Event {
                seq: 4,
                hash: event_hash(&EventFacts {
                    seq: 4,
                    ..sample_facts(&payload)
                }),
            },
        ]
    }

    #[test]
    fn golden_payload_hash_is_frozen() {
        assert_eq!(
            payload_hash(b"{\"tool_name\":\"Read\"}").to_string(),
            GOLDEN_PAYLOAD_HASH
        );
    }

    #[test]
    fn golden_event_hash_is_frozen() {
        let payload = payload_hash(b"{\"tool_name\":\"Read\"}");
        assert_eq!(
            event_hash(&sample_facts(&payload)).to_string(),
            GOLDEN_EVENT_HASH
        );
    }

    #[test]
    fn golden_gap_hash_is_frozen() {
        assert_eq!(
            gap_hash(&GapRecord::Missing { from: 2, to: 3 }).to_string(),
            GOLDEN_GAP_HASH
        );
    }

    #[test]
    fn golden_chain_root_is_frozen() {
        let result = chain(&golden_items());
        assert_eq!(result.root.to_string(), GOLDEN_CHAIN_ROOT);
        assert_eq!(result.coverage.first_seq, 0);
        assert_eq!(result.coverage.last_seq, 4);
        assert_eq!(result.coverage.events, 2);
        assert_eq!(result.coverage.pre_chain, 1);
        assert_eq!(
            result.coverage.gaps,
            vec![GapRecord::Missing { from: 2, to: 3 }]
        );
        assert!(!result.coverage.is_gap_free());
    }

    #[test]
    #[ignore = "Referenz-Vektoren neu erzeugen: --ignored --nocapture"]
    fn evidence_reference_vectors() {
        let payload = payload_hash(b"{\"tool_name\":\"Read\"}");
        println!("payload = {payload}");
        println!("event   = {}", event_hash(&sample_facts(&payload)));
        println!(
            "gap     = {}",
            gap_hash(&GapRecord::Missing { from: 2, to: 3 })
        );
        println!("chain   = {}", chain(&golden_items()).root);
    }

    // --- Relative Eigenschaften ----------------------------------------------

    #[test]
    fn a_single_flipped_payload_bit_changes_the_root() {
        let a = chain(&golden_items());
        let mut tampered = golden_items();
        let payload = payload_hash(b"{\"tool_name\":\"ReaD\"}"); // ein Bit anders
        if let ChainItem::Event { hash, .. } = &mut tampered[0] {
            *hash = event_hash(&EventFacts {
                seq: 0,
                ..sample_facts(&payload)
            });
        }
        assert_ne!(a.root, chain(&tampered).root);
    }

    #[test]
    fn dropping_or_reordering_a_link_changes_the_root() {
        let all = golden_items();
        let complete = chain(&all);

        // Ein Glied weglassen — auch die Luecke selbst.
        for skip in 0..all.len() {
            let mut partial = all.clone();
            partial.remove(skip);
            assert_ne!(complete.root, chain(&partial).root, "ohne Glied {skip}");
        }

        // Umsortieren.
        let mut swapped = all.clone();
        swapped.swap(0, 3);
        assert_ne!(complete.root, chain(&swapped).root);
    }

    #[test]
    fn the_domains_are_separated() {
        // Gleiches Material, verschiedene Kontexte ⇒ verschiedene Hashes. Ein
        // Payload-Hash kann nie als Seal-Identitaet durchgehen.
        let material = b"identisches material";
        let as_payload = blake3::derive_key(CTX_PAYLOAD, material);
        let as_seal = blake3::derive_key(CTX_SEAL, material);
        let as_chain = blake3::derive_key(CTX_CHAIN, material);
        assert_ne!(as_payload, as_seal);
        assert_ne!(as_payload, as_chain);
        assert_ne!(as_seal, as_chain);
    }

    #[test]
    fn the_encoding_cannot_be_shifted_between_fields() {
        // Laengenpraefixe: `at`-Suffix in `raw_kind` verschieben ergibt einen
        // anderen Hash — zwei Feldfolgen koennen nie dieselben Bytes bilden.
        let payload = payload_hash(b"x");
        let a = event_hash(&EventFacts {
            at: "2026-01-01T00:00:00Z",
            raw_kind: "Stop",
            ..sample_facts(&payload)
        });
        let b = event_hash(&EventFacts {
            at: "2026-01-01T00:00:00ZS",
            raw_kind: "top",
            ..sample_facts(&payload)
        });
        assert_ne!(a, b);

        // Und ein leerer Some ist etwas anderes als None.
        let with_empty = event_hash(&EventFacts {
            cwd: Some(""),
            ..sample_facts(&payload)
        });
        let with_none = event_hash(&EventFacts {
            cwd: None,
            ..sample_facts(&payload)
        });
        assert_ne!(with_empty, with_none);
    }

    #[test]
    fn an_empty_chain_has_the_zero_root_and_claims_nothing() {
        let result = chain(&[]);
        assert_eq!(result.root, ContentHash::from_bytes([0u8; 32]));
        assert_eq!(result.coverage.events, 0);
        assert!(result.coverage.is_gap_free());
    }

    fn sample_seal() -> Seal {
        Seal {
            root: payload_hash(b"root-material"),
            agent: "claude-code".into(),
            scope: SCOPE_AGENT_HOOKS_V1.into(),
            first_seq: 0,
            last_seq: 41,
            events: 40,
            gaps: 2,
            pre_chain: 0,
            outcome: SealOutcome::Stored {
                session: format!("b3-{}", "a".repeat(64)),
            },
            previous: None,
            last_event_at: "2026-08-24T10:15:00Z".into(),
        }
    }

    #[test]
    fn a_seal_roundtrips_through_its_text_form() {
        let seal = sample_seal();
        let text = seal.to_text().unwrap();
        assert_eq!(text.lines().count(), SEAL_LINES);
        assert!(text.ends_with('\n'));
        assert_eq!(Seal::parse(&text).unwrap(), seal);

        // Auch der Rejected-Fall (session=-).
        let rejected = Seal {
            outcome: SealOutcome::Rejected,
            previous: Some(Seal::id_of_text(&text)),
            ..sample_seal()
        };
        let text = rejected.to_text().unwrap();
        assert_eq!(Seal::parse(&text).unwrap(), rejected);
    }

    #[test]
    fn the_seal_id_is_stable_and_domain_separated() {
        let text = sample_seal().to_text().unwrap();
        assert_eq!(Seal::id_of_text(&text), Seal::id_of_text(&text));
        // Nicht derselbe Hash wie ein Payload ueber dieselben Bytes.
        assert_ne!(Seal::id_of_text(&text), payload_hash(text.as_bytes()));
    }

    #[test]
    fn a_forged_agent_line_is_rejected_not_serialized() {
        // #12-Regel: Ein Zeilenumbruch im Freitextfeld koennte eine zweite
        // `outcome=`-Zeile faelschen — fail-closed, Feld benannt, Wert nie
        // zitiert.
        let evil = Seal {
            agent: "claude-code\noutcome=stored".into(),
            ..sample_seal()
        };
        let err = evil.to_text().unwrap_err();
        assert_eq!(err.field, "agent");
        assert!(!format!("{err}").contains("outcome=stored"));
    }

    #[test]
    fn parsing_is_strict_about_shape() {
        let text = sample_seal().to_text().unwrap();

        // Eine Zeile zu wenig.
        let truncated: String = text
            .lines()
            .take(SEAL_LINES - 1)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            Seal::parse(&truncated),
            Err(SealParseError::Lines(_))
        ));

        // Falsche Version.
        let wrong = text.replacen(SEAL_VERSION, "minds-seal-v9", 1);
        assert_eq!(Seal::parse(&wrong), Err(SealParseError::Version));

        // `stored` ohne Session-Id ist widerspruechlich.
        let broken = text.replacen(&format!("b3-{}", "a".repeat(64)), "-", 1);
        assert!(matches!(
            Seal::parse(&broken),
            Err(SealParseError::Field("outcome"))
        ));
    }

    #[test]
    fn a_salted_chain_differs_and_is_deterministic() {
        // Der Salt ist das Anti-Orakel: Ohne ihn ist der Root aus geratenen
        // Payloads nachrechenbar. Mit ihm nicht — und derselbe Salt liefert
        // weiterhin denselben Root (Idempotenz der Seals).
        let items = golden_items();
        let plain = chain(&items);
        let salted = chain_salted(&[7u8; 32], &items);
        assert_ne!(plain.root, salted.root);
        assert_eq!(salted.root, chain_salted(&[7u8; 32], &items).root);
        assert_ne!(salted.root, chain_salted(&[8u8; 32], &items).root);
        // Die Coverage haengt nicht am Salt.
        assert_eq!(plain.coverage, salted.coverage);
    }

    #[test]
    fn parse_rejects_hidden_and_control_characters_like_to_text_does() {
        // Symmetrie Schreib-/Lesepfad: Die seal_id ist ein Hash über
        // beliebige Bytes — was to_text nie erzeugen würde, darf parse nicht
        // durchreichen (Terminal-Injection über verify, #116-Doktrin).
        let base = sample_seal().to_text().unwrap();
        let esc = base.replacen("scope=agent-hooks/v1", "scope=agent\u{1b}[2Khooks", 1);
        assert!(matches!(
            Seal::parse(&esc),
            Err(SealParseError::Field("scope"))
        ));
        let bidi = base.replacen("agent=claude-code", "agent=claude\u{202e}edoc", 1);
        assert!(matches!(
            Seal::parse(&bidi),
            Err(SealParseError::Field("agent"))
        ));
        let zw = base.replacen(
            "last_event_at=2026-08-24T10:15:00Z",
            "last_event_at=2026-08-24T10:15:00Z\u{200b}",
            1,
        );
        assert!(matches!(
            Seal::parse(&zw),
            Err(SealParseError::Field("last_event_at"))
        ));
    }

    #[test]
    fn a_seal_with_a_token_shaped_session_line_is_rejected() {
        // `session=` muss eine formgueltige SessionId sein — ein Seal traegt
        // nichts Tilgbares, auch nicht ueber diese Zeile.
        let text = sample_seal().to_text().unwrap().replacen(
            &format!("b3-{}", "a".repeat(64)),
            "glpat-abc123",
            1,
        );
        assert!(matches!(
            Seal::parse(&text),
            Err(SealParseError::Field("outcome"))
        ));
    }

    // --- Die Invarianten aus ADR-0011, als benannte Verträge ---------------
    //
    // Vieles davon prüfen auch die relativen Tests oben; diese hier binden
    // die WORTLAUTE der Invarianten an Code, damit eine stille Verschiebung
    // der Semantik nicht als „Refactor" durchgeht.

    #[test]
    fn invariant_each_chained_link_is_bound_to_exactly_one_predecessor() {
        // Invariante 1+2: Die Verkettung lebt im FOLD — h_i deckt h_{i-1}.
        // Ein getauschter Vorgänger ändert jeden nachfolgenden Zustand.
        let items = golden_items();
        let complete = chain(&items);
        let mut other_predecessor = items.clone();
        other_predecessor[0] = ChainItem::PreChain { seq: 0 };
        assert_ne!(
            complete.root,
            chain(&other_predecessor).root,
            "der Root muss den Vorgänger jedes Glieds binden"
        );
    }

    #[test]
    fn invariant_the_event_hash_covers_the_observed_facts_and_only_those() {
        // Invariante 3: seq, Zeit, raw_kind, cwd, transcript_path,
        // payload_hash — jede Änderung ändert den Hash. `kind` ist bewusst
        // NICHT dabei (Interpretation, rekonstruierbar).
        let payload = payload_hash(b"x");
        let base = sample_facts(&payload);
        let base_hash = event_hash(&base);
        assert_ne!(
            base_hash,
            event_hash(&EventFacts {
                seq: 43,
                ..base.clone()
            })
        );
        assert_ne!(
            base_hash,
            event_hash(&EventFacts {
                at_nanos: base.at_nanos + 1,
                ..base.clone()
            })
        );
        assert_ne!(
            base_hash,
            event_hash(&EventFacts {
                raw_kind: "Stop",
                ..base.clone()
            })
        );
    }

    #[test]
    fn invariant_a_gap_is_itself_verifiable_evidence() {
        // Invariante 4: Wer die Lücke weglässt, bekommt einen anderen Root —
        // „da war halt nichts" ist keine mögliche Behauptung.
        let with_gap = golden_items();
        let without: Vec<ChainItem> = with_gap
            .iter()
            .filter(|i| !matches!(i, ChainItem::Gap(_)))
            .cloned()
            .collect();
        assert_ne!(chain(&with_gap).root, chain(&without).root);
    }

    #[test]
    fn invariant_coverage_is_always_scoped() {
        // Invariante 5: Ein Seal ohne Beobachtungsgrenze parst nicht —
        // „vollständig" ohne Bezugsrahmen wäre eine leere Behauptung.
        let text = sample_seal().to_text().unwrap().replacen(
            &format!("scope={SCOPE_AGENT_HOOKS_V1}"),
            "scope=",
            1,
        );
        assert!(matches!(
            Seal::parse(&text),
            Err(SealParseError::Field("scope"))
        ));
    }

    #[test]
    fn invariant_the_hash_domains_are_versioned_namespaces() {
        // Domain-Separation ist Teil des Protokolls, samt Version im String:
        // chain-v2 könnte neben v1 existieren, ohne Altes umzudeuten.
        for ctx in [CTX_PAYLOAD, CTX_EVENT, CTX_GAP, CTX_CHAIN, CTX_SEAL] {
            assert!(ctx.starts_with("minds/evidence/v1/"), "{ctx}");
        }
    }

    #[test]
    fn damaged_records_hash_distinctly() {
        let bare = GapRecord::Damaged {
            seq: None,
            bytes: None,
        };
        let with_seq = GapRecord::Damaged {
            seq: Some(7),
            bytes: None,
        };
        let with_bytes = GapRecord::Damaged {
            seq: Some(7),
            bytes: Some(payload_hash(b"truemmer")),
        };
        assert_ne!(gap_hash(&bare), gap_hash(&with_seq));
        assert_ne!(gap_hash(&with_seq), gap_hash(&with_bytes));
    }
}
