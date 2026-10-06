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
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Persistierbarer Zwischenstand eines [`ChainFolder`] für lokale Recovery.
///
/// Enthält nur den aktuellen Hash und die Coverage, keine vergangenen Events.
/// Der Zustand muss wie der Salt lokal geschützt bleiben: Vor dem ersten Glied
/// enthält er den abgeleiteten Salt. Serde prüft die Form, nicht die Echtheit;
/// Wiederaufnahme setzt einen vertrauenswürdigen, unveränderten Stand voraus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderState {
    state: [u8; 32],
    first_seq: Option<u64>,
    last_seq: u64,
    events: u64,
    pre_chain: u64,
    gaps: Vec<GapRecord>,
}

/// Inkrementeller Evidence-Fold: ein Kettenglied pro [`Self::push`].
///
/// Root und Coverage entsprechen jederzeit [`chain_salted`] bzw. [`chain`]
/// über denselben Präfix. Events werden nicht aufgehoben; nur Gap-Records
/// bleiben für die Coverage erhalten. Die gegebene Reihenfolge wird gebunden,
/// weder sortiert noch auf fehlende Sequenzen geprüft.
///
/// ```
/// use minds_core::evidence::{ChainFolder, ChainItem, GapRecord, chain_salted};
///
/// let salt = [7; 32]; // In Produktion: zufälliger, lokal geschützter Session-Salt.
/// let items = [
///     ChainItem::PreChain { seq: 0 },
///     ChainItem::Gap(GapRecord::Missing { from: 1, to: 2 }),
/// ];
/// let mut folder = ChainFolder::new_salted(&salt);
/// folder.push(&items[0]);
/// let saved = serde_json::to_vec(&folder.to_state())?;
/// let mut resumed = ChainFolder::from_state(serde_json::from_slice(&saved)?);
/// resumed.push(&items[1]);
/// assert_eq!(resumed.snapshot(), chain_salted(&salt, &items));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct ChainFolder {
    state: [u8; 32],
    first_seq: Option<u64>,
    last_seq: u64,
    events: u64,
    pre_chain: u64,
    gaps: Vec<GapRecord>,
}

impl ChainFolder {
    /// Startet wie [`chain_salted`] auf `derive_key(CTX_CHAIN, salt)`.
    pub fn new_salted(salt: &[u8; 32]) -> Self {
        Self::new(blake3::derive_key(CTX_CHAIN, salt))
    }

    /// Startet wie [`chain`] auf 32 Nullbytes (lokale Nachrechnung, Tests).
    pub fn new_unsalted() -> Self {
        Self::new([0; 32])
    }

    fn new(state: [u8; 32]) -> Self {
        Self {
            state,
            first_seq: None,
            last_seq: 0,
            events: 0,
            pre_chain: 0,
            gaps: Vec::new(),
        }
    }

    /// Bindet genau ein Glied an den bisherigen Head und ergänzt die Coverage.
    /// Lücken zählen nicht als gelesene Events und erweitern deren Bereich nicht.
    pub fn push(&mut self, item: &ChainItem) {
        let mut buf = Vec::with_capacity(80);
        buf.extend_from_slice(&self.state);
        match item {
            ChainItem::Event { seq, hash } => {
                buf.push(TAG_EVENT);
                buf.extend_from_slice(&hash.to_bytes());
                self.events += 1;
                self.first_seq.get_or_insert(*seq);
                self.last_seq = self.last_seq.max(*seq);
            }
            ChainItem::PreChain { seq } => {
                buf.push(TAG_PRE_CHAIN);
                put_u64(&mut buf, *seq);
                self.pre_chain += 1;
                self.first_seq.get_or_insert(*seq);
                self.last_seq = self.last_seq.max(*seq);
            }
            ChainItem::Gap(gap) => {
                buf.push(TAG_GAP);
                buf.extend_from_slice(&gap_hash(gap).to_bytes());
                self.gaps.push(gap.clone());
            }
        }
        self.state = blake3::derive_key(CTX_CHAIN, &buf);
    }

    /// Der aktuelle Root, ohne die Coverage zu kopieren.
    pub fn head(&self) -> ContentHash {
        ContentHash::from_bytes(self.state)
    }

    /// Root und Coverage des bisherigen Präfixes; der Folder bleibt nutzbar.
    pub fn snapshot(&self) -> ChainResult {
        ChainResult {
            root: self.head(),
            coverage: Coverage {
                first_seq: self.first_seq.unwrap_or(0),
                last_seq: self.last_seq,
                events: self.events,
                gaps: self.gaps.clone(),
                pre_chain: self.pre_chain,
            },
        }
    }

    /// Kopiert den vollständigen Stand für lokale Persistierung per Serde.
    pub fn to_state(&self) -> FolderState {
        FolderState {
            state: self.state,
            first_seq: self.first_seq,
            last_seq: self.last_seq,
            events: self.events,
            pre_chain: self.pre_chain,
            gaps: self.gaps.clone(),
        }
    }

    /// Setzt einen vertrauenswürdigen lokalen Stand ohne erneutes Falten fort.
    /// Der ursprüngliche Salt ist zur Wiederaufnahme nicht erforderlich.
    pub fn from_state(s: FolderState) -> Self {
        Self {
            state: s.state,
            first_seq: s.first_seq,
            last_seq: s.last_seq,
            events: s.events,
            pre_chain: s.pre_chain,
            gaps: s.gaps,
        }
    }

    // Der Batch-Pfad kann die Gap-Liste verschieben, statt sie zu kopieren.
    fn into_result(self) -> ChainResult {
        ChainResult {
            root: self.head(),
            coverage: Coverage {
                first_seq: self.first_seq.unwrap_or(0),
                last_seq: self.last_seq,
                events: self.events,
                gaps: self.gaps,
                pre_chain: self.pre_chain,
            },
        }
    }
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
    let mut folder = ChainFolder::new(start);
    for item in items {
        folder.push(item);
    }
    folder.into_result()
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

/// Vom unabhängigen Witness beobachtete Hook-Evidence.
pub const SCOPE_WITNESS_V1: &str = "witness/v1";

/// Vom Witness selbst beobachtete Datei-Änderungen im Worktree (EA-08):
/// der eigene Stream des Witness mit `fs.observed`-Events.
pub const SCOPE_WITNESS_FS_V1: &str = "witness-fs/v1";

/// Das Isolationsprofil des Witness (ADR-0012, Entscheidung 2): wie Agent
/// und Witness voneinander getrennt sind. Steht in `witness.json` und im
/// Payload von `witness.start` — Konfiguration des Witness, keine Aussage
/// über eine einzelne Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WitnessProfile {
    /// Der Agent läuft in einem Container, der Witness auf dem Host.
    Container,
    /// Der Witness läuft unter einer eigenen Benutzerkennung.
    User,
    /// Verwaltete Einstellungen des Harness trennen Agent und Witness.
    Managed,
}

impl WitnessProfile {
    /// Der Name in Konfiguration und Ausgabe (`container`, `user`,
    /// `managed`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::User => "user",
            Self::Managed => "managed",
        }
    }

    /// Liest einen Namen strikt zurück — `None` für alles andere.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "container" => Some(Self::Container),
            "user" => Some(Self::User),
            "managed" => Some(Self::Managed),
            _ => None,
        }
    }
}

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

    /// Die Beobachtungen des Datei-Beobachters (Scope `witness-fs/v1`)
    /// wurden redigiert und als Observation-Objekt abgelegt (EA-08).
    ///
    /// Die Zeile `session=` trägt hier die **Id des Observation-Objekts**
    /// (`b3-…`, [`crate::observation::Observations::id`]) — dieselbe Zeile,
    /// anderer Gegenstand: Das Seal-Format bleibt bei 13 Zeilen, und ein
    /// älteres Binary lehnt das unbekannte Outcome-Wort ab, statt die Id als
    /// Session zu deuten (derselbe akzeptierte Tausch wie bei Schema 2).
    ObservationsStored {
        /// Die Id des Observation-Objekts in Textform (`b3-…`).
        observations: String,
    },
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
            SealOutcome::ObservationsStored { observations } => observations.as_str(),
            SealOutcome::Rejected => "-",
        };
        let outcome = match &self.outcome {
            SealOutcome::Stored { .. } => "stored",
            SealOutcome::ObservationsStored { .. } => "observations_stored",
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
            // Strenger als `stored`: nur die kanonische Kleinschreibung — die
            // Id adressiert einen Ref, und es gibt genau eine Schreibweise.
            ("observations_stored", id)
                if id
                    .parse::<ContentHash>()
                    .is_ok_and(|hash| hash.as_str() == id) =>
            {
                SealOutcome::ObservationsStored {
                    observations: id.to_string(),
                }
            }
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

/// Ein Seal samt Identität und Signatur-Anwesenheit — die kleinste Einheit,
/// aus der eine Oberfläche ihre „SESSION SEALED"-Zusammenfassung rendert.
///
/// Bewusst **ohne** Verdikt: Ein Verdikt braucht alle Epochen einer Session
/// und lebt im Read-Model (`minds-reader`). Hier steht nur, was der
/// Checkpoint über den gerade geschriebenen Seal selbst weiß — billig genug
/// für den Hook-Pfad, ehrlich genug für die Anzeige.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealSummary {
    /// `seal_id = derive_key(CTX_SEAL, text)` — die Identität.
    pub seal_id: ContentHash,
    /// Der Seal selbst, mit allen Feldern.
    pub seal: Seal,
    /// Ob eine Signatur **vorliegt** (Anwesenheit, keine Prüfung —
    /// Gültigkeit prüft `minds verify`).
    pub signed: bool,
}

impl SealSummary {
    /// Bündelt Identität, Seal und Signatur-Anwesenheit.
    pub fn new(seal_id: ContentHash, seal: Seal, signed: bool) -> Self {
        Self {
            seal_id,
            seal,
            signed,
        }
    }
}

/// Warum ein Text kein Seal ist. Nennt Zeile bzw. Feld, zitiert nie den Wert.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SealParseError {
    /// Falsche Zeilenzahl.
    #[error("a seal has {SEAL_LINES} lines, this text has {0}")]
    Lines(usize),

    /// Unbekannte Versionszeile.
    #[error("unknown seal version")]
    Version,

    /// Eine Zeile trägt nicht den erwarteten Schlüssel oder keinen gültigen
    /// Wert.
    #[error("seal line {0} is missing or invalid")]
    Field(&'static str),
}

// ---------------------------------------------------------------------------
// Das Proof-Vokabular
// ---------------------------------------------------------------------------

/// Die vier Wörter des Verdikts — dieselbe Matrix, die `minds verify` druckt
/// (ADR-0011, Entscheidung 7). **Eine** Wortquelle für CLI, TUI,
/// Checkpoint-Summary und Audit-Export: Wer hier ein Wort ändert, ändert es
/// überall — und keine Oberfläche kann ein anderes sprechen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Seals hash-valide, Coverage lückenlos innerhalb der Grenze.
    Verified,
    /// Seal-Material verändert — die Verifikation schlägt fehl.
    Tampered,
    /// Hash-valide, aber Lücken oder Pre-Chain-Events im Bereich.
    Incomplete,
    /// Keine Grundlage für ein Verdikt (kein Seal, Payload nicht lesbar).
    Unverifiable,
}

impl Verdict {
    /// Das Verdikt-Wort, wie jede Oberfläche es druckt.
    pub const fn word(self) -> &'static str {
        match self {
            Verdict::Verified => "VERIFIED",
            Verdict::Tampered => "TAMPERED",
            Verdict::Incomplete => "VERIFIED, INCOMPLETE",
            Verdict::Unverifiable => "NOT VERIFIABLE",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.word())
    }
}

impl SealOutcome {
    /// Das Anzeige-Wort — nie das Wire-Wort aus [`Seal::to_text`], das
    /// hash-tragend ist und sich nicht ändern darf.
    pub const fn human_word(&self) -> &'static str {
        match self {
            SealOutcome::Stored { .. } => "stored",
            SealOutcome::ObservationsStored { .. } => "observations stored",
            SealOutcome::Rejected => "rejected (payload)",
        }
    }
}

// ---------------------------------------------------------------------------
// Proof-Vokabular (EA-13)
// ---------------------------------------------------------------------------
//
// **Eine** Quelle für Audit-Bundle, TUI, `minds verify` und Doku: Wer hier
// einen Satz ändert, ändert die Zusage überall — und keine Oberfläche kann
// mehr behaupten als eine andere. Jeder Satz trägt den Stufenbereich, in dem
// er gilt; jede Oberfläche druckt genau die Sätze der Stufe, die das
// geprüfte Material trägt.
//
// Die Texte stehen einzeln als Konstanten, damit die stufenlosen Listen
// (`PROVES`, `DOES_NOT_PROVE`) und die Stufen-Tabellen
// (`PROVES_V2`, `DOES_NOT_PROVE_V2`) dieselben Bytes teilen. Dass die
// stufenlosen Listen genau die A1-Sätze sind, hält ein Test fest.

/// Die Assurance-Stufe als Ordinal — der `minds-core`-Spiegel von
/// `minds_reader::assurance::Assurance`, damit `core` frei von Reader-Typen
/// bleibt (EA-13). Kein `Serialize`: Eine Stufe wird berechnet, nie
/// gespeichert (W2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// A0 claimed.
    A0,
    /// A1 observed.
    A1,
    /// A2 witnessed.
    A2,
    /// A3 reproduced.
    A3,
}

impl Level {
    /// Alle Stufen, aufsteigend.
    pub const ALL: [Level; 4] = [Level::A0, Level::A1, Level::A2, Level::A3];
}

/// Ein Satz des Proof-Vokabulars mit dem Stufenbereich, in dem er gilt:
/// ab `holds_from`, bis **ausschließlich** `holds_until` (`None`: nie
/// zurückgezogen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProofSentence {
    /// Stabile Kennung — für Tests und Doku, nie umbenennen.
    pub id: &'static str,
    /// Die Kurzform für eine Zeile (`minds verify`: `Not proven`).
    pub short: &'static str,
    /// Der volle Satz.
    pub text: &'static str,
    /// Ab dieser Stufe gilt der Satz.
    pub holds_from: Level,
    /// Ab dieser Stufe gilt er nicht mehr.
    pub holds_until: Option<Level>,
}

impl ProofSentence {
    /// Ob der Satz auf Stufe `level` gilt.
    pub fn holds_at(&self, level: Level) -> bool {
        self.holds_from <= level && self.holds_until.is_none_or(|until| level < until)
    }
}

// --- Was belegt ist: die Texte -----------------------------------------------

const P_SESSION_CONTENT_ADDRESSED: &str = "Every session id is the blake3 hash of its canonical content — the content can be recomputed against it.";
const P_ATTESTATION_PAYLOAD: &str = "The attestation_payload is byte-for-byte the text `minds sign` signs; a shipped signature is verifiable against it.";
const P_REVIEW_PAYLOAD: &str = "The review_payload binds the hash of the verdict; a valid signature over it shows who reviewed.";
const P_VERDICTS_SURVIVE_REBASE: &str =
    "Verdicts attach to the change id and therefore survive rebase and force-push.";
const P_FORGOTTEN_VISIBLE: &str = "A forgotten session stays visible as a reference (payload: forgotten) — deletion is provable, not traceless.";
const P_SEAL_VERIFIABLE: &str = "Seal identity and signature are externally verifiable: seal_id = blake3::derive_key(\"minds/evidence/v1/seal\", text). The seal commits cryptographically to chain root and coverage; the underlying chain is reproducible only with the local journal and session salt (ADR-0011).";
const P_BLOCK_SEAL: &str = "A block seal (rejected_seals) proves that a session existed whose payload the storage policy rejected — without disclosing its content.";
const P_WITNESS_CHAINING: &str = "Events were chained live by the witness, outside the agent's trust domain, and sealed under its key (minds-witness): a write between append and seal breaks a witness-signed chain.";
const P_INTENT_APPROVED: &str = "The intent the session worked against is bound by version, and its approver signed it (minds-intent).";
const P_FS_OBSERVED: &str = "The witness observed the worktree's file-system changes during the session, gap-free and under its key (witness-fs/v1) — a second observer besides the agent's own report. Whether they explain a given commit's lines is a separate check (`minds verify`, coverage axis).";
const P_RESULTS_REPRODUCED: &str = "The decisive results (tests, builds) are real: CI re-ran the decisive commands and reproduced the outcomes the agent reported.";
const P_FIRST_SIGHT_BOUND: &str = "An upper time bound: CI countersigned every seal on first sight (minds-anchor), so the evidence existed no later than that.";

// --- Was nicht belegt ist: die Texte -----------------------------------------

const N_MODEL_IDENTITY: &str =
    "Not which model produced the answers: the model name is what the agent reported.";
const N_DECISION_CORRECT: &str = "Not that the decision was right: the evidence shows what happened, not whether it was the correct thing to do.";
const N_OUTSIDE_BOUNDARY: &str = "Not what happened outside the observation boundary: activity that neither the hooks nor the witness observe is not recorded.";
const N_ROOT_COMPROMISE: &str = "Not integrity against whoever controls the host (root, the witness account or its key): they can rewrite the evidence and the witness alike.";
const N_RECORD_COMPLETE: &str = "Not that the record is complete: the hot path is fail-open, and a lost event is silently absent here (`minds fsck` makes gaps visible).";
const N_LINES_ATTRIBUTED: &str = "Not that a session actually produced the lines attributed to it — the mapping comes from trailers (observed) and heuristics (inferred); the provenance is stated on every edge.";
const N_TRANSCRIPT_REPORTED: &str =
    "Not that a model did what the transcript says — what is recorded is what the agent reported.";
const N_REPORTED_RESULTS: &str = "Not that reported results (tests, builds) are real: they are what the agent reported until a CI replay reproduces them.";
const N_WHO_CONTROLS_KEYS: &str = "Not who controls the signing keys. Without an allowed_signers file from a trusted source, a signature is only a self-attestation.";
const N_WHO_CONTROLS_KEYS_WITNESSED: &str = "Not who controls the human signing keys: witness key control is shown; human key custody still depends on allowed_signers.";
const N_UNSIGNED_ENTRIES: &str = "Not that unsigned entries are genuine: they are content-addressed, but nobody vouches for them with a key.";
const N_BUNDLE_LEVEL_SELF_REPORTED: &str = "Not the assurance level as a portable fact: it is assessed when the evidence is read, from the repository and the trusted signers at hand; a level stated elsewhere (an exported bundle, a report) cannot be recomputed from that document alone — re-run `minds verify --signers` against the repository.";
const N_BUNDLE_CHAIN: &str = "Not that the bundle alone can recompute the chain: the chain root is reproducible only with the local journal and session salt — the bundle proves the sealed claim (identity, signature, coverage), not the chain itself.";
const N_OUTSIDE_SEALED_RANGES: &str = "Not that nothing happened outside sealed ranges — a seal claims only the sequence range its epoch actually read.";
const N_APPEND_TO_SEAL_WINDOW: &str = "Not the integrity between append and seal: until the checkpoint, only the file system protects the journal; a local write before sealing is undetectable (ADR-0011, decision 1).";
const N_ONLY_ACTOR: &str = "Not that the agent process was the only actor: subprocesses, network access and plugins outside the hook boundary (scope in the seal) are not captured — coverage means complete within the boundary, never system activity.";
const N_ONLY_ACTOR_WITNESSED: &str = "Not that the agent was the only actor: changes in the worktree are observed; processes, network and other machines are not.";
const N_UNINTERPRETED_EFFECTS: &str = "Not the effect of uninterpreted tool calls: capture=uninterpreted means observed, but the effects are not normalized — the interpretation axis is separate from integrity and coverage.";
const N_WALL_CLOCK_TIME: &str = "Not real wall-clock time: timestamps come from the hook's local clock, with no external time anchor.";
const N_WALL_CLOCK_TIME_ANCHORED: &str =
    "Not the exact time: the CI anchor gives an upper bound, there is no lower bound.";

// --- Die Tabellen ------------------------------------------------------------

/// Was das Proof-Modell belegt, je Stufe (EA-13). Bis A1 sind es die
/// Zusagen über Inhaltsadressierung, Payloads und Seals; ab A2 kommt dazu,
/// was der Witness einlöst (Live-Verkettung, Intent, Datei-Beobachtung), ab A3, was
/// CI einlöst (Replay, Erstsichtung).
///
/// Die Inhaltsadressierung gilt schon auf A0: Sie hängt an keinem Seal.
/// Die Seal-Zusagen gelten ab A1 — auf A0 trägt kein intakter Seal das
/// Material.
pub const PROVES_V2: &[ProofSentence] = &[
    ProofSentence {
        id: "session_content_addressed",
        short: "session content",
        text: P_SESSION_CONTENT_ADDRESSED,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "attestation_payload",
        short: "the attestation payload",
        text: P_ATTESTATION_PAYLOAD,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "review_payload",
        short: "who reviewed",
        text: P_REVIEW_PAYLOAD,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "verdicts_survive_rebase",
        short: "verdicts across rebase",
        text: P_VERDICTS_SURVIVE_REBASE,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "forgotten_visible",
        short: "forgotten sessions",
        text: P_FORGOTTEN_VISIBLE,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "seal_verifiable",
        short: "seal identity and coverage",
        text: P_SEAL_VERIFIABLE,
        holds_from: Level::A1,
        holds_until: None,
    },
    ProofSentence {
        id: "block_seal",
        short: "rejected sessions existed",
        text: P_BLOCK_SEAL,
        holds_from: Level::A1,
        holds_until: None,
    },
    ProofSentence {
        id: "witness_chaining",
        short: "witness live chaining",
        text: P_WITNESS_CHAINING,
        holds_from: Level::A2,
        holds_until: None,
    },
    ProofSentence {
        id: "intent_approved",
        short: "intent version and approver",
        text: P_INTENT_APPROVED,
        holds_from: Level::A2,
        holds_until: None,
    },
    ProofSentence {
        id: "fs_observed",
        short: "file-system observation",
        text: P_FS_OBSERVED,
        holds_from: Level::A2,
        holds_until: None,
    },
    ProofSentence {
        id: "results_reproduced",
        short: "decisive results reproduced",
        text: P_RESULTS_REPRODUCED,
        holds_from: Level::A3,
        holds_until: None,
    },
    ProofSentence {
        id: "first_sight_bound",
        short: "first-sight time bound",
        text: P_FIRST_SIGHT_BOUND,
        holds_from: Level::A3,
        holds_until: None,
    },
];

/// Was das Proof-Modell **nicht** belegt, je Stufe. Die Sätze, die auf
/// keiner Stufe fallen, stehen vorn: Die Kurzzeile von `minds verify`
/// (`Not proven`, EA-12) nennt höchstens drei, und die drei sollen die
/// sein, die keine Stufe je einlöst.
///
/// Ab A2 fallen die Sätze, die der Witness einlöst, weg
/// (`append_to_seal_window`) oder werden enger gefasst
/// (`who_controls_keys` → `who_controls_keys_witnessed`, `only_actor` →
/// `only_actor_witnessed`); ab A3 die, die CI einlöst (`reported_results`;
/// `wall_clock_time` → `wall_clock_time_anchored`).
pub const DOES_NOT_PROVE_V2: &[ProofSentence] = &[
    ProofSentence {
        id: "model_identity",
        short: "model identity",
        text: N_MODEL_IDENTITY,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "decision_correct",
        short: "correctness of the decision",
        text: N_DECISION_CORRECT,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "outside_boundary",
        short: "actions outside the boundary",
        text: N_OUTSIDE_BOUNDARY,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "root_compromise",
        short: "a compromised host",
        text: N_ROOT_COMPROMISE,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "record_complete",
        short: "completeness of the record",
        text: N_RECORD_COMPLETE,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "lines_attributed",
        short: "line attribution",
        text: N_LINES_ATTRIBUTED,
        holds_from: Level::A0,
        // Nicht ab A2 zurückgezogen (Abweichung von der EA-13-Tabelle): Die
        // Stufe sagt, wer beobachtet hat, nicht, welcher Commit aus der
        // Session stammt — die Reconciliation ist kein Eingang von
        // `assess`, und den Trailer kann der Agent schreiben. Erst wenn
        // die Stufe den Abgleich trägt, darf dieser Satz fallen.
        holds_until: None,
    },
    ProofSentence {
        id: "transcript_reported",
        short: "the transcript's account",
        text: N_TRANSCRIPT_REPORTED,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "reported_results",
        short: "reported results",
        text: N_REPORTED_RESULTS,
        holds_from: Level::A0,
        holds_until: Some(Level::A3),
    },
    ProofSentence {
        id: "who_controls_keys",
        short: "key custody",
        text: N_WHO_CONTROLS_KEYS,
        holds_from: Level::A0,
        holds_until: Some(Level::A2),
    },
    ProofSentence {
        id: "who_controls_keys_witnessed",
        short: "human key custody",
        text: N_WHO_CONTROLS_KEYS_WITNESSED,
        holds_from: Level::A2,
        holds_until: None,
    },
    ProofSentence {
        id: "unsigned_entries",
        short: "unsigned entries",
        text: N_UNSIGNED_ENTRIES,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "bundle_chain",
        short: "the chain from the bundle alone",
        text: N_BUNDLE_CHAIN,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "bundle_level_self_reported",
        short: "a stated level on its own",
        text: N_BUNDLE_LEVEL_SELF_REPORTED,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "outside_sealed_ranges",
        short: "unsealed ranges",
        text: N_OUTSIDE_SEALED_RANGES,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "append_to_seal_window",
        short: "integrity between append and seal",
        text: N_APPEND_TO_SEAL_WINDOW,
        holds_from: Level::A0,
        holds_until: Some(Level::A2),
    },
    ProofSentence {
        id: "only_actor",
        short: "the agent as the only actor",
        text: N_ONLY_ACTOR,
        holds_from: Level::A0,
        holds_until: Some(Level::A2),
    },
    ProofSentence {
        id: "only_actor_witnessed",
        short: "actors beyond the worktree",
        text: N_ONLY_ACTOR_WITNESSED,
        holds_from: Level::A2,
        holds_until: None,
    },
    ProofSentence {
        id: "uninterpreted_effects",
        short: "effects of uninterpreted calls",
        text: N_UNINTERPRETED_EFFECTS,
        holds_from: Level::A0,
        holds_until: None,
    },
    ProofSentence {
        id: "wall_clock_time",
        short: "wall-clock time",
        text: N_WALL_CLOCK_TIME,
        holds_from: Level::A0,
        holds_until: Some(Level::A3),
    },
    ProofSentence {
        id: "wall_clock_time_anchored",
        short: "a lower time bound",
        text: N_WALL_CLOCK_TIME_ANCHORED,
        holds_from: Level::A3,
        holds_until: None,
    },
];

/// Die Zusagen, die auf Stufe `level` gelten — in der Reihenfolge von
/// [`PROVES_V2`].
pub fn proves_at(level: Level) -> impl Iterator<Item = &'static ProofSentence> {
    PROVES_V2
        .iter()
        .filter(move |sentence| sentence.holds_at(level))
}

/// Die Grenzen, die auf Stufe `level` gelten — in der Reihenfolge von
/// [`DOES_NOT_PROVE_V2`].
pub fn limits_at(level: Level) -> impl Iterator<Item = &'static ProofSentence> {
    DOES_NOT_PROVE_V2
        .iter()
        .filter(move |sentence| sentence.holds_at(level))
}

/// Was das Proof-Modell auf **A1** belegt — die stufenlose Form von
/// [`proves_at`]`(Level::A1)`, Satz für Satz in derselben Reihenfolge (ein
/// Test hält das fest). Nur zur Kompatibilität für Aufrufer ohne Stufe —
/// keine Wahrheit für sich; jede
/// Oberfläche, die eine kennt, nimmt [`proves_at`].
pub const PROVES: &[&str] = &[
    P_SESSION_CONTENT_ADDRESSED,
    P_ATTESTATION_PAYLOAD,
    P_REVIEW_PAYLOAD,
    P_VERDICTS_SURVIVE_REBASE,
    P_FORGOTTEN_VISIBLE,
    P_SEAL_VERIFIABLE,
    P_BLOCK_SEAL,
];

/// Was das Proof-Modell auf **A1** nicht belegt — die stufenlose Form von
/// [`limits_at`]`(Level::A1)`, Satz für Satz in derselben Reihenfolge (ein
/// Test hält das fest). Nur zur Kompatibilität für Aufrufer ohne Stufe —
/// keine Wahrheit für sich; jede
/// Oberfläche, die eine kennt, nimmt [`limits_at`].
pub const DOES_NOT_PROVE: &[&str] = &[
    N_MODEL_IDENTITY,
    N_DECISION_CORRECT,
    N_OUTSIDE_BOUNDARY,
    N_ROOT_COMPROMISE,
    N_RECORD_COMPLETE,
    N_LINES_ATTRIBUTED,
    N_TRANSCRIPT_REPORTED,
    N_REPORTED_RESULTS,
    N_WHO_CONTROLS_KEYS,
    N_UNSIGNED_ENTRIES,
    N_BUNDLE_CHAIN,
    N_BUNDLE_LEVEL_SELF_REPORTED,
    N_OUTSIDE_SEALED_RANGES,
    N_APPEND_TO_SEAL_WINDOW,
    N_ONLY_ACTOR,
    N_UNINTERPRETED_EFFECTS,
    N_WALL_CLOCK_TIME,
];

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

    fn witness_fs_seal() -> Seal {
        Seal {
            agent: "witness".into(),
            scope: SCOPE_WITNESS_FS_V1.into(),
            outcome: SealOutcome::ObservationsStored {
                observations: format!("b3-{}", "c".repeat(64)),
            },
            ..sample_seal()
        }
    }

    #[test]
    fn witness_fs_seal_text_is_golden() {
        // Absolut: die eingefrorene Textform samt Id. Ein älteres Binary
        // lehnt `observations_stored` ab — der dokumentierte Tausch.
        let seal = witness_fs_seal();
        let text = seal.to_text().unwrap();
        assert_eq!(
            text,
            format!(
                "minds-seal-v1\nroot={}\nagent=witness\nscope=witness-fs/v1\nfirst_seq=0\n\
                 last_seq=41\nevents=40\ngaps=2\npre_chain=0\noutcome=observations_stored\n\
                 session=b3-{}\nprevious=-\nlast_event_at=2026-08-24T10:15:00Z\n",
                payload_hash(b"root-material"),
                "c".repeat(64)
            )
        );
        assert_eq!(
            Seal::id_of_text(&text).as_str(),
            "b3-460d5be3b51613317fd249d8534d28ebc656332bf07146b82152b42e6bdf6d07"
        );
        assert_eq!(Seal::parse(&text).unwrap(), seal);
        assert_eq!(seal.outcome.human_word(), "observations stored");
    }

    #[test]
    fn witness_fs_seal_parsing_is_strict() {
        let text = witness_fs_seal().to_text().unwrap();
        let hex = "c".repeat(64);
        for bad in [
            "-".to_owned(),
            format!("b3-{}", hex.to_uppercase()),
            format!("B3-{hex}"),
            hex.clone(),
            "glpat-abc123".to_owned(),
        ] {
            let forged = text.replacen(&format!("b3-{hex}"), &bad, 1);
            assert!(
                matches!(Seal::parse(&forged), Err(SealParseError::Field("outcome"))),
                "{bad}"
            );
        }
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

    /// Name, Parser und JSON-Form (`witness.json`) sind dieselben drei
    /// Wörter — ein Profil, eine Schreibweise.
    #[test]
    fn witness_profile_names_round_trip() {
        for (profile, name) in [
            (WitnessProfile::Container, "container"),
            (WitnessProfile::User, "user"),
            (WitnessProfile::Managed, "managed"),
        ] {
            assert_eq!(profile.name(), name);
            assert_eq!(WitnessProfile::parse(name), Some(profile));
            let json = serde_json::to_string(&profile).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(
                serde_json::from_str::<WitnessProfile>(&json).unwrap(),
                profile
            );
        }
        assert_eq!(WitnessProfile::parse("Container"), None);
        assert_eq!(WitnessProfile::parse(""), None);
    }

    #[test]
    fn proof_sentences_have_unique_ids() {
        // Eindeutig über **beide** Tabellen: Doku und Tests nennen einen
        // Satz nur über seine Id.
        let mut ids: Vec<&str> = PROVES_V2
            .iter()
            .chain(DOES_NOT_PROVE_V2)
            .map(|s| s.id)
            .collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before);
        for sentence in PROVES_V2.iter().chain(DOES_NOT_PROVE_V2) {
            assert!(
                !sentence.id.is_empty()
                    && sentence
                        .id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{}",
                sentence.id
            );
            assert!(!sentence.short.is_empty() && !sentence.short.contains('·'));
            assert!(!sentence.text.is_empty());
            // Ein Bereich, der nie gilt, wäre ein toter Satz.
            assert!(Level::ALL.iter().any(|level| sentence.holds_at(*level)));
        }
    }

    /// Die Ids sind Schnittstelle (Bundle, Doku, Tests): eingefroren je
    /// Stufe. Wer hier etwas ändert, ändert eine Zusage — bewusst.
    #[test]
    fn proof_sentence_ids_per_level_are_frozen() {
        let proves = |level| proves_at(level).map(|s| s.id).collect::<Vec<_>>();
        let limits = |level| limits_at(level).map(|s| s.id).collect::<Vec<_>>();
        let a0_proves = [
            "session_content_addressed",
            "attestation_payload",
            "review_payload",
            "verdicts_survive_rebase",
            "forgotten_visible",
        ];
        let a1_proves = [&a0_proves[..], &["seal_verifiable", "block_seal"]].concat();
        let a2_proves = [
            &a1_proves[..],
            &["witness_chaining", "intent_approved", "fs_observed"],
        ]
        .concat();
        let a3_proves = [&a2_proves[..], &["results_reproduced", "first_sight_bound"]].concat();
        assert_eq!(proves(Level::A0), a0_proves);
        assert_eq!(proves(Level::A1), a1_proves);
        assert_eq!(proves(Level::A2), a2_proves);
        assert_eq!(proves(Level::A3), a3_proves);

        let a1_limits = [
            "model_identity",
            "decision_correct",
            "outside_boundary",
            "root_compromise",
            "record_complete",
            "lines_attributed",
            "transcript_reported",
            "reported_results",
            "who_controls_keys",
            "unsigned_entries",
            "bundle_chain",
            "bundle_level_self_reported",
            "outside_sealed_ranges",
            "append_to_seal_window",
            "only_actor",
            "uninterpreted_effects",
            "wall_clock_time",
        ];
        assert_eq!(limits(Level::A0), a1_limits);
        assert_eq!(limits(Level::A1), a1_limits);
        assert_eq!(
            limits(Level::A2),
            [
                "model_identity",
                "decision_correct",
                "outside_boundary",
                "root_compromise",
                "record_complete",
                "lines_attributed",
                "transcript_reported",
                "reported_results",
                "who_controls_keys_witnessed",
                "unsigned_entries",
                "bundle_chain",
                "bundle_level_self_reported",
                "outside_sealed_ranges",
                "only_actor_witnessed",
                "uninterpreted_effects",
                "wall_clock_time",
            ]
        );
        assert_eq!(
            limits(Level::A3),
            [
                "model_identity",
                "decision_correct",
                "outside_boundary",
                "root_compromise",
                "record_complete",
                "lines_attributed",
                "transcript_reported",
                "who_controls_keys_witnessed",
                "unsigned_entries",
                "bundle_chain",
                "bundle_level_self_reported",
                "outside_sealed_ranges",
                "only_actor_witnessed",
                "uninterpreted_effects",
                "wall_clock_time_anchored",
            ]
        );
    }

    /// Die stufenlosen Listen sind die A1-Sätze — Satz für Satz, in
    /// derselben Reihenfolge. Wer eine Liste ändert, ohne die Tabelle zu
    /// ändern, fällt hier auf.
    #[test]
    fn the_flat_lists_are_derived_from_a1() {
        let proves: Vec<&str> = proves_at(Level::A1).map(|s| s.text).collect();
        assert_eq!(proves, PROVES);
        let limits: Vec<&str> = limits_at(Level::A1).map(|s| s.text).collect();
        assert_eq!(limits, DOES_NOT_PROVE);
    }

    #[test]
    fn a1_limits_include_append_window() {
        assert!(
            limits_at(Level::A1).any(|s| s.id == "append_to_seal_window"),
            "A1 keeps the append→seal limitation"
        );
        assert!(
            DOES_NOT_PROVE
                .iter()
                .any(|t| t.contains("between append and seal"))
        );
        // A0 nennt mindestens so viel Grenze wie A1.
        assert!(limits_at(Level::A0).any(|s| s.id == "append_to_seal_window"));
    }

    #[test]
    fn a2_limits_exclude_append_window() {
        for level in [Level::A2, Level::A3] {
            let ids: Vec<&str> = limits_at(level).map(|s| s.id).collect();
            assert!(!ids.contains(&"append_to_seal_window"), "{level:?}");
            assert!(
                limits_at(level).all(|s| !s.text.contains("between append and seal")),
                "{level:?}"
            );
        }
        // … und nennt dafür die A2-Grenzen.
        let a2: Vec<&str> = limits_at(Level::A2).map(|s| s.id).collect();
        for retired in ["only_actor", "who_controls_keys"] {
            assert!(!a2.contains(&retired), "{retired}");
        }
        for kept in [
            "only_actor_witnessed",
            "who_controls_keys_witnessed",
            "reported_results",
            "wall_clock_time",
        ] {
            assert!(a2.contains(&kept), "{kept}");
        }
        // Die Stufe trägt den Abgleich mit dem Commit nicht: Die
        // Zuordnung von Zeilen bleibt auf jeder Stufe eine Grenze, und das
        // Bündel sagt auf jeder Stufe, dass es seine Stufe nur behauptet.
        for level in Level::ALL {
            let ids: Vec<&str> = limits_at(level).map(|s| s.id).collect();
            assert!(ids.contains(&"lines_attributed"), "{level:?}");
            assert!(ids.contains(&"bundle_level_self_reported"), "{level:?}");
        }
        let a3: Vec<&str> = limits_at(Level::A3).map(|s| s.id).collect();
        assert!(!a3.contains(&"reported_results"));
        assert!(!a3.contains(&"wall_clock_time"));
        assert!(a3.contains(&"wall_clock_time_anchored"));
    }

    /// Eine höhere Stufe sagt nie weniger zu als eine niedrigere.
    #[test]
    fn proves_only_grow_with_the_level() {
        for pair in Level::ALL.windows(2) {
            let lower: Vec<&str> = proves_at(pair[0]).map(|s| s.id).collect();
            let upper: Vec<&str> = proves_at(pair[1]).map(|s| s.id).collect();
            for id in &lower {
                assert!(upper.contains(id), "{id} lost at {:?}", pair[1]);
            }
        }
    }

    /// Jede zurückgezogene Grenze ist ab genau ihrer Stufe durch eine
    /// Zusage gedeckt oder durch eine engere Grenze ersetzt — die Tabelle
    /// lässt nichts stillschweigend fallen.
    #[test]
    fn every_retired_limit_is_covered() {
        // (zurückgezogen, ersetzt durch Zusage oder engere Grenze)
        let covered = [
            ("append_to_seal_window", "witness_chaining"),
            ("who_controls_keys", "who_controls_keys_witnessed"),
            ("only_actor", "only_actor_witnessed"),
            ("reported_results", "results_reproduced"),
            ("wall_clock_time", "wall_clock_time_anchored"),
        ];
        for sentence in DOES_NOT_PROVE_V2 {
            let Some(until) = sentence.holds_until else {
                continue;
            };
            let (_, by) = covered
                .iter()
                .find(|(id, _)| *id == sentence.id)
                .unwrap_or_else(|| panic!("{} retires without cover", sentence.id));
            let cover = PROVES_V2
                .iter()
                .chain(DOES_NOT_PROVE_V2)
                .find(|s| s.id == *by)
                .unwrap();
            assert_eq!(cover.holds_from, until, "{}", sentence.id);
        }
    }

    /// Die Kurzzeile nennt höchstens drei — auf jeder Stufe dieselben drei,
    /// die keine Stufe einlöst.
    #[test]
    fn the_first_three_limits_hold_at_every_level() {
        for level in Level::ALL {
            let first: Vec<&str> = limits_at(level).take(3).map(|s| s.short).collect();
            assert_eq!(
                first,
                [
                    "model identity",
                    "correctness of the decision",
                    "actions outside the boundary"
                ]
            );
        }
    }
}
