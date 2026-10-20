//! Assurance-Stufen (ADR-0012, EA-11): **wer** das Material einer Session
//! beobachtet hat — je versiegelter Range und für die Session insgesamt,
//! samt der Gründe, warum die Stufe nicht höher liegt.
//!
//! # Berechnet, nie gespeichert (W2)
//!
//! [`assess`] ist eine reine, deterministische Funktion über Fakten, die der
//! Aufrufer zur Lesezeit zusammenträgt: Scope und Signaturprüfung je Seal,
//! die Abdeckung durch den Datei-Beobachter, Profil des Witness, Intent,
//! Replay und Anker. Kein Typ dieses Moduls ist serialisierbar (das
//! enthaltene [`WitnessProfile`] ist es — Konfigurations-Vokabular aus
//! `witness.json`, keine Stufe), und kein gespeichertes Objekt nennt eine
//! Stufe — ein Test (`assurance_is_never_stored`) hält beides fest. Wer eine
//! Stufe sehen will, rechnet sie aus dem Material neu; eine abgelegte Stufe
//! wäre eine zweite Wahrheit, die der Agent schreiben könnte. Außerhalb
//! dieses Crates lässt sich ein Report nur über [`assess`] **bauen**
//! (`#[non_exhaustive]`); seine Felder nachträglich zu ändern, verhindert das
//! nicht — Oberflächen und Gates lesen ihn nur.
//!
//! # Vertrauen kommt vom Aufrufer, nie aus dem Repository
//!
//! Ob eine Witness-Signatur gültig ist, entscheidet der Aufrufer gegen eine
//! **explizit** übergebene `allowed_signers`-Datei (`--signers`, nie
//! `gpg.ssh.allowedSignersFile` aus der Repo-Konfiguration, die der Agent
//! schreiben kann) und meldet es über [`SealSignature`] und
//! [`AssuranceInput::trusted_signers`]. Ohne vertrauenswürdige Signer gibt
//! es kein A2 — fail-closed.
//!
//! # Die Regeln je Range
//!
//! Die Stufen sind Tore, von unten geprüft. Das erste Tor, an dem eine
//! Bedingung scheitert, setzt die Stufe; seine gescheiterten Bedingungen sind
//! die Gründe (in fester Reihenfolge, die erste zuerst). Höhere Tore werden
//! dann nicht mehr befragt.
//!
//! - **A0 claimed** — kein Seal, Legacy, verändertes Seal-Material, eine
//!   offene Epochen-Kette, ein im Repository fehlender bezeugter Seal
//!   (Ledger), ein Witness-Seal ohne Signatur oder — mit vertrauenswürdigen
//!   Signern geprüft — unter fremdem Namespace (EA-09), nur vermutete
//!   Verknüpfung (`inferred`) oder ein Scope, der
//!   keine Session versiegelt.
//! - **A1 observed** — Scope `agent-hooks/v1`; oder ein `witness/v1`-Seal,
//!   der eine A2-Bedingung verfehlt.
//! - **A2 witnessed** — Scope `witness/v1`; Signatur gültig unter
//!   `minds-witness` gegen vertrauenswürdige Signer; ein lückenloses
//!   `witness-fs/v1`-Fenster über die Session; jeder Witness-Lauf im
//!   Fenster mit qualifiziertem Profil ([`profile_qualifies`]) **und**
//!   belegter Trennung ([`WitnessStart::isolated`]) — der Profilname allein
//!   gewährt nie A2; die **Session ruht beim Checkpoint** (der letzte
//!   Bereich ist ein solcher Witness-Bereich, und seine gespeicherte Session
//!   endet in Ruhe: letztes Event `Stop`, `SubagentStop` oder `SessionEnd`,
//!   kein `PreToolUse` ohne `PostToolUse` — [`RangeInput::quiescence`]);
//!   Intent gebunden — vom Witness
//!   **verkettet**, belegt ab dem Anfang der Session, ohne Wechsel,
//!   Snapshot passend zum Anker (EA-14) — **und** unter `minds-intent`
//!   gültig signiert.
//! - **A3 reproduced** — A2, dazu ein signierter Replay ohne `claim not
//!   reproduced`, mit mindestens einem entscheidenden Befehl und **ohne**
//!   übersprungenen (jeder entscheidende Befehl nachvollzogen), und eine
//!   gültige `minds-anchor`-Gegenzeichnung des Seals. Das A3 einer
//!   Range spricht nur für **ihren** Seal; die Session erreicht A3 erst,
//!   wenn jeder ihrer Seals gegengezeichnet ist (Gesamtstufe = Minimum).
//!
//! # Was die Stufe über den Commit sagt
//!
//! Nichts. Sie beschreibt die Session. Eine Trailer-Verknüpfung
//! (`observed`) kann auch der Agent schreiben und so eine bezeugte Session
//! an einen fremden Commit hängen — ob die Session die Zeilen des Commits
//! erklärt, beantwortet die Coverage-Achse (Reconciliation), nicht diese.
//! Ein Gate auf die Stufe allein ersetzt sie nicht.
//!
//! # Was die Stufe nicht misst
//!
//! Unbestätigte Claims und unerklärte Zeilen senken die Stufe **nicht** —
//! sie sind Coverage-Fakten (EA-12 zeigt sie). Die Stufe sagt, wer
//! beobachtet hat, nicht wie sauber die Session war. Deshalb ist die
//! Reconciliation kein Eingang. Und ebenso wenig ein unversiegelter Rest
//! nach dem letzten Seal oder Lücken innerhalb eines Seals: Die Stufe gilt
//! je **versiegelter** Range; das macht das Verdikt `INCOMPLETE`
//! (Coverage-Achse). Mit einer Ausnahme: Ein Bereich mit Lücke oder
//! beschädigtem Eintrag gilt nie als ruhend (`quiescence`, siehe „Die
//! bezeugte Ruhe") — endet die Session in einem solchen Bereich, bleibt
//! sie bei A1. Eine Range, die ganz fehlt, ist dagegen kein Coverage-Fakt
//! (siehe unten).
//!
//! # Integrität geht vor
//!
//! Die eine Ausnahme ist die Integrität selbst: Ist Seal-Material verändert
//! ([`EvidenceVerdict::Tampered`]), trägt keine Range mehr als A0 — eine
//! gültige Witness-Signatur über einzelne Seals ändert daran nichts, wenn
//! die Kette nicht nachrechnet. Ebenso bei einer **offenen Epochen-Kette**
//! (gelöschter Seal in der Mitte, Gabelung, zurückgewiesener Vorgänger):
//! Dann fehlt eine Range, deren Stufe niemand kennt, und die Gesamtstufe
//! als Minimum über die übrigen wäre geschönt. Ebenso, wenn ein Seal, den
//! der Witness laut seinem Ledger erzeugt hat, im Repository fehlt (EA-12),
//! und bei einem Witness-Seal ohne Signatur oder (geprüft, mit
//! vertrauenswürdigen Signern) unter fremdem Namespace (EA-09) — jeder
//! dieser Befunde trifft **jede** Range der Session. Verdikt,
//! Kettenschluss und Ledger-Abgleich sind deshalb
//! Pflichteingänge, damit kein Aufrufer die Kappung vergessen kann. Die
//! Exit-Codes bleiben unberührt (W6).
//!
//! Das gilt für die **Witness-Kette**: Der Witness verkettet seine Seals
//! über den eigenen Epochen-Zustand. Ein Rückfall-Bereich (`agent-hooks/v1`,
//! Witness unerreichbar) entsteht auf der Seite des Agenten mit eigener
//! Kette — kein signiertes `previous` des Witness verweist je auf ihn
//! (`checkpoint/delegate.rs`: „zusammengeführt wird nichts"). Ihn zu
//! löschen oder gar nicht erst zu versiegeln, öffnet keine Kette und fehlt
//! in keinem Ledger (siehe „Bekannte Grenzen").
//!
//! # Die bezeugte Ruhe
//!
//! Den **letzten** Witness-Bereich zu löschen, öffnet die Kette nicht: Auf
//! ihn verweist nichts Signiertes. Ohne Ledger (CI) fiele das nicht auf, und
//! mit Ledger nur für Witness-Seals. Deshalb verlangt A2, dass der Witness
//! die Session am Checkpoint **ruhend** gesehen hat: Der letzte Bereich ist
//! ein vertrauenswürdiger `witness/v1`-Bereich, dessen gespeicherte Session
//! in Ruhe endet — letztes Event `Stop`, `SubagentStop` oder `SessionEnd`,
//! kein `PreToolUse` ohne `PostToolUse` (`lineage.quiescence`, über die
//! Session-Id im Seal mitsigniert; ein älteres Objekt mit `lineage.closed`
//! zählt als ruhend). Ein Commit, den der Mensch macht, nachdem der Agent
//! geantwortet hat, erreicht so A2, ohne dass die Session enden muss; was
//! der Agent danach tut, läuft in die nächste Epoche, und der nächste
//! Checkpoint versiegelt sie. Lief der Agent beim Checkpoint noch (ein
//! offener Tool-Aufruf, ein letztes Event, das kein Stop ist), bleibt die
//! Session bei A1 — `session active at checkpoint`, mit der Event-Nummer.
//!
//! Ob der Witness auch das **Ende** der Session gesehen hat
//! (`lineage.closed`, [`RangeInput::closes_session`]), senkt die Stufe nicht
//! mehr; der Report nennt es ([`AssuranceReport::session_end`]),
//! `minds verify` trägt es unter `Not proven` ein: Die Session kann weiter-
//! gehen, und ein späterer Bereich gehört dann zu einer weiteren Epoche.
//!
//! # Bekannte Grenzen
//!
//! Was die Regeln nicht schließen können, steht hier statt in einer
//! stillen Annahme (Tests halten es fest):
//!
//! Der Witness bezeugt, was er empfangen hat — nicht, was der Agent ihm
//! vorenthält. Der Socket nimmt Frames vom Agenten an (ADR-0012,
//! Entscheidung 2), und am Witness vorbei kommt der Agent jederzeit: im
//! `user`-Profil über die eigene Hook-Konfiguration im Repository, im
//! `container`-Profil durch Entfernen des Sockets, in beiden über einen
//! Frame, den der Hook als zu groß verwirft. Daraus folgen Grenzen, die keine
//! Regel hier schließt. Was sie aufdeckt, ist die **Coverage-Achse**: Der
//! Datei-Beobachter läuft unabhängig von Hook-Frames. Was in einer
//! verschwiegenen Zeit geschrieben wurde, wird nie `explained`; erfundene
//! Schreib-Claims ohne passende Beobachtung bleiben `reported only` /
//! `uncorroborated` (Coverage-Aussagen, sie senken die Stufe nicht).
//!
//! - **Eine ganz erfundene Session.** Der Agent schickt `SessionStart` …
//!   `SessionEnd` mit erfundenen Tool-Events unter neuer `local_id`, löst
//!   einen Checkpoint aus und verweist im Trailer darauf: geschlossen,
//!   vollständig, bezeugt (A2 — sobald A2 erreichbar ist). Bezeugt heißt
//!   „der Witness hat diese Frames empfangen", nicht „sie beschreiben, was
//!   geschah" — das sagt die Coverage-Achse (siehe auch „Was die Stufe über
//!   den Commit sagt").
//! - **Eine verpflanzte Kette.** Seals nennen weder Repository noch
//!   `local_id`. Beobachtet derselbe Witness-Schlüssel mehrere Repositories,
//!   lässt sich eine Session samt Kette aus einem anderen kopieren; das
//!   Ledger prüft nur Fehlendes, nicht Überzähliges. Abhilfe ist ein
//!   Witness-Schlüssel je Repository (EA-08b bindet Schlüssel und
//!   Repository) — und ein Aufrufer, der prüft, dass jeder Principal zu
//!   **diesem** Repository gehört (siehe [`AssuranceInput::trusted_signers`]).
//! - **Ein Harness ohne `Stop`/`SessionEnd`** (etwa OpenCode, dessen Plugin
//!   `session.*`-Events liefert, die als `Other` ankommen) ruht nie:
//!   Seine Sessions bleiben dauerhaft bei A1 — fail-closed, aber eine
//!   Erreichbarkeitsgrenze für EA-12/EA-22.
//!
//! - **Ein verschwiegener Rückfall-Bereich.** Arbeitet der Agent zeitweise
//!   am Witness vorbei, entsteht ein `agent-hooks/v1`-Bereich mit eigener
//!   Kette (oder, wenn er auch den lokalen Checkpoint unterdrückt, gar
//!   keiner). Löscht er ihn, öffnet das keine Witness-Kette, und das Ledger
//!   kennt ihn nie: Die Session steht mit ihren Witness-Bereichen da (A2).
//!   ADR-0012s „Witness 40 s unerreichbar → gemischt, Minimum A1" gilt
//!   damit nur, solange der Rückfall-Bereich vorliegt; der Aufrufer hängt ihn
//!   an, wenn er ihn findet ([`Seals::Sealed`]).
//! - **Ein ruhender Zwischenstand.** Jeder Checkpoint, der die Session in
//!   Ruhe trifft (der Agent hat geantwortet, der Mensch committet),
//!   versiegelt einen ruhenden Bereich — auch ein vom Agenten vorgetäuschtes
//!   `Stop` oder `SessionEnd` tut das. Dann
//!   - erreicht dieser Commit A2 ohne jede Löschung, obwohl die Session
//!     weitergeht — gewollt: Das ist der Commit nach der fertigen Antwort;
//!   - arbeitet der Agent danach am Witness vorbei, steht der ruhende
//!     Bereich als letzter Witness-Bereich da — auch mit vollständigem
//!     Ledger;
//!   - arbeitet er danach bezeugt weiter und löscht diese späteren
//!     Witness-Seals, fällt das nur mit Ledger auf (A0); ohne Ledger (CI)
//!     nicht — jeder ruhende Zwischenstand ist ohne Ledger ein mögliches
//!     „Ende".
//! - **Ein Commit, während der Agent arbeitet, bleibt bei A1**
//!   (`session active at checkpoint`), bis ein späterer Checkpoint einen
//!   ruhenden Bereich versiegelt und dieser Seal ins geprüfte Repository
//!   gelangt. Versiegelt wird heute nur per Checkpoint (am Commit).
//! - **`--resume`.** Setzt ein Mensch eine Session fort und committet,
//!   während der Agent arbeitet, ist der ruhende Bereich nicht mehr der
//!   letzte (A1) — es sei denn, der fortgesetzte Teil wird gelöscht (wie
//!   beim ruhenden Zwischenstand).
//! - **Ohne Ledger** fällt ein gelöschter Witness-Seal mitten in der Kette
//!   über den Kettenschluss auf (A0) und ein gelöschter letzter über die
//!   fehlende Ruhe (höchstens A1) — außer, der Bereich davor ruhte schon.
//!
//! Der Test `known_limits_are_pinned` hält jede dieser Grenzen fest.
//!
//! # Woher das Profil kommt
//!
//! ADR-0012 nennt das Profil „a fact the verifier checks (via the signer
//! principal)". Hier stammt es aus `witness.start` im Stream des Witness,
//! dessen Seals unter genau diesem Principal signiert sind — der Principal
//! bürgt für den Datensatz, der Datensatz nennt das Profil.

use std::collections::BTreeSet;
use std::fmt;

use minds_core::evidence::{
    SCOPE_AGENT_HOOKS_V1, SCOPE_WITNESS_V1, SEPARATED_WITNESS_PRINCIPAL, WitnessProfile,
};
use minds_core::{ContentHash, EvidenceSource, Quiescence};

use crate::model::EvidenceVerdict;
use crate::text::sanitize;

/// Ob das Profil `container` A2 tragen kann. Bleibt `false`, bis die
/// Qualifikation aus EA-S2 (inotify unter nativem Linux: Latenz, Verlust
/// unter Last, Socket, Isolation, Hooks) aufgezeichnet ist — ADR-0012,
/// Entscheidung 2: Die Annahme des ADR macht fehlende Messungen nicht zur
/// Qualifikation. Das Follow-up kippt die Konstante im Code, nie per
/// Konfiguration, die der Agent schreiben könnte.
pub const CONTAINER_PROFILE_QUALIFIES: bool = false;

/// Ob das Profil `managed` A2 tragen kann. Bleibt `false`, bis der Spike
/// EA-S1 mit vollständigen nativen Ergebnissen belegt, dass verwaltete
/// Einstellungen den Agenten vom Witness trennen (Addendum EA-S1).
pub const MANAGED_PROFILE_QUALIFIES: bool = false;

/// Ob ein Isolationsprofil A2 tragen **kann** — die Qualifikation des
/// Profils als Bauart. `user` hat kein offenes Qualifikations-Tor (der
/// Witness läuft unter eigener Kennung, die Trennung ist Dateibesitz).
///
/// Hinreichend ist das nie: Ein Profilname allein gewährt kein A2
/// (ADR-0012, Entscheidung 2). Ein Lauf zählt erst, wenn er zusätzlich
/// seine Trennung belegt ([`WitnessStart::isolated`]).
pub const fn profile_qualifies(profile: WitnessProfile) -> bool {
    match profile {
        WitnessProfile::User => true,
        WitnessProfile::Container => CONTAINER_PROFILE_QUALIFIES,
        WitnessProfile::Managed => MANAGED_PROFILE_QUALIFIES,
        // Dieselbe Kennung wie der Agent: keine Trennung, nie A2.
        WitnessProfile::Local => false,
    }
}

/// Höchstlänge (Zeichen) eines Strings aus dem Material in einem Grund oder
/// den Witness-Fakten — der Agent kann Scope und Lückengründe beliebig lang
/// machen; ein Job-Log soll davon nicht überflutet werden.
pub const MAX_SHOWN_CHARS: usize = 120;

/// Die Lückengründe des Beobachters — eine geschlossene Menge (EA-08c).
/// Alles andere erscheint als `unknown` (tolerant lesen): Ein Grund aus dem
/// Material ist so nie freier Text, der sich in einem Job-Log wie eine
/// eigene Aussage lesen ließe, und die Liste bleibt kurz (höchstens sechs
/// Wörter), gleich wie viele Lücken das Material nennt.
pub const GAP_REASONS: [&str; 5] = ["ignore_rules", "overflow", "panic", "policy", "unavailable"];

/// Höchstzahl der einzeln genannten Einträge einer Liste aus dem Material
/// (Principals, Schlüssel, fehlende Seals); der Rest wird gezählt — ein
/// Neustart-Karussell soll kein Job-Log füllen.
pub const MAX_LISTED: usize = 8;

/// Kürzt einen String aus dem Material auf [`MAX_SHOWN_CHARS`] Zeichen und
/// entschärft ihn danach — so wird nie mehr als der Ausschnitt escaped, und
/// der Schnitt trennt keine Escape-Sequenz.
fn shown(text: &str) -> String {
    match text.char_indices().nth(MAX_SHOWN_CHARS) {
        Some((cut, _)) => format!("{}…", sanitize(&text[..cut])),
        None => sanitize(text),
    }
}

/// Ein String aus dem Material in einem [`Reason`] — gekürzt und entschärft.
/// Nur dieses Modul baut ihn: Wer außerhalb einen Grund zusammensetzt, kann
/// keinen rohen Agent-Text hineinlegen, den `Reason::text` ungeprüft
/// ausgäbe. Die Klammern „…“ setzt erst `Reason::text`; wer einen `Shown`
/// selbst ausgibt, klammert ihn ebenso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown(String);

impl Shown {
    fn of(text: &str) -> Self {
        Self(shown(text))
    }

    /// Der entschärfte Text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Die Assurance-Stufe — geordnet: `A0Claimed < … < A3Reproduced`, damit
/// die Gesamtstufe das Minimum über die Ranges ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssuranceLevel {
    /// Behauptet: Import, Heuristik, Legacy — niemand Unabhängiges sah zu.
    A0Claimed,
    /// Beobachtet von den Hooks des Harness, unter derselben Kennung wie der
    /// Agent.
    A1Observed,
    /// Bezeugt von einem Witness in eigener Vertrauensdomäne.
    A2Witnessed,
    /// Bezeugt, in CI nachvollzogen und gegengezeichnet.
    A3Reproduced,
}

impl AssuranceLevel {
    /// Alle Stufen, aufsteigend.
    pub const ALL: [AssuranceLevel; 4] = [
        AssuranceLevel::A0Claimed,
        AssuranceLevel::A1Observed,
        AssuranceLevel::A2Witnessed,
        AssuranceLevel::A3Reproduced,
    ];

    /// Liest ein Kürzel (`A0` … `A3`) zurück — strikt, für Gates wie
    /// `--require-assurance` (EA-12); eine Wortliste, nicht zwei.
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.code() == code)
    }

    /// Das Kürzel (`A0` … `A3`).
    pub const fn code(self) -> &'static str {
        match self {
            Self::A0Claimed => "A0",
            Self::A1Observed => "A1",
            Self::A2Witnessed => "A2",
            Self::A3Reproduced => "A3",
        }
    }

    /// Der Ordinal-Spiegel in `minds-core` — der Schlüssel zum
    /// stufenabhängigen Proof-Vokabular ([`minds_core::evidence::limits_at`]).
    pub const fn level(self) -> minds_core::evidence::Level {
        match self {
            Self::A0Claimed => minds_core::evidence::Level::A0,
            Self::A1Observed => minds_core::evidence::Level::A1,
            Self::A2Witnessed => minds_core::evidence::Level::A2,
            Self::A3Reproduced => minds_core::evidence::Level::A3,
        }
    }

    /// Das Anzeige-Wort (`00-conventions.md`): `A0 claimed` … `A3
    /// reproduced`.
    pub const fn word(self) -> &'static str {
        match self {
            Self::A0Claimed => "A0 claimed",
            Self::A1Observed => "A1 observed",
            Self::A2Witnessed => "A2 witnessed",
            Self::A3Reproduced => "A3 reproduced",
        }
    }
}

impl fmt::Display for AssuranceLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// Was der Aufrufer über die Signatur eines Seals weiß.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealSignature {
    /// Keine `seal.sig`.
    Missing,
    /// Eine Signatur liegt vor, wurde aber nicht geprüft — **nur**: keine
    /// vertrauenswürdigen Signer oder kein `ssh-keygen`. Mit Signern und
    /// `ssh-keygen` ist jede misslungene Prüfung (kaputte Armor, Parse- oder
    /// Ausführungsfehler) [`SealSignature::NotWitness`] — sonst entginge ein
    /// absichtlich verdorbener Seal der Integritäts-Kappung.
    NotChecked,
    /// Gültig unter `minds-witness`, von einem Principal, dessen sämtliche
    /// `allowed_signers`-Zeilen auf genau diesen Namespace beschränkt sind.
    Witness {
        /// Der Principal, wie `ssh-keygen -Y find-principals` ihn nennt.
        principal: String,
    },
    /// Geprüft und **nicht** gültig unter `minds-witness` (falscher
    /// Namespace, fremder Schlüssel, manipulierte Bytes). Für einen
    /// `witness/v1`-Seal ist das ein Integritätsbefund (EA-09): mit
    /// vertrauenswürdigen Signern A0, auch wenn der Aufrufer es nicht
    /// zusätzlich als [`EvidenceVerdict::Tampered`] meldet; ohne Signer ist
    /// die Prüfung keine — A1.
    NotWitness,
}

impl SealSignature {
    /// Ob der Seal unter einem Principal gilt, den ein Mensch als
    /// **getrennten** Witness eingetragen hat
    /// ([`SEPARATED_WITNESS_PRINCIPAL`]) — die eine Stelle für diese Regel.
    pub fn is_separated_witness(&self) -> bool {
        matches!(self, Self::Witness { principal }
            if principal.starts_with(SEPARATED_WITNESS_PRINCIPAL))
    }
}

/// Ein versiegelter Bereich der Session, wie der Aufrufer ihn gelesen hat.
///
/// Dass der Seal zu **dieser** Session gehört (seine `session=`-Zeile, die
/// Epochen-Kette), prüft der Aufrufer — ein aus einer anderen Session
/// verpflanzter, gültig signierter Seal ist ein Integritätsbefund
/// ([`AssuranceInput::integrity`]), keine Range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeInput {
    /// Die `seal_id`.
    pub seal: ContentHash,
    /// Der Scope aus dem Seal-Text (roh; die Gründe entschärfen ihn).
    pub scope: String,
    /// Die Signaturlage.
    pub signature: SealSignature,
    /// Ob die Session, die dieser Seal nennt, am Ende des Bereichs ruhte
    /// (`lineage.quiescence`): letztes Event `Stop`, `SubagentStop` oder
    /// `SessionEnd`, kein `PreToolUse` ohne `PostToolUse`. Das Objekt ist
    /// inhaltsadressiert und seine Id steht im Seal-Text — unter einem
    /// vertrauenswürdigen Witness-Seal ist die Ruhe also bezeugt. `None`:
    /// nicht festgestellt (älteres Objekt, unvollständig gelesen) — zählt
    /// als nicht ruhend, es sei denn, [`Self::closes_session`] steht.
    ///
    /// Derselbe Vertrag wie bei [`Self::closes_session`]: aus genau dem
    /// Objekt dieses Seals, mit Hash-Prüfung, zur geprüften Agent-Session
    /// gehörend; sonst `None` — fail-closed.
    pub quiescence: Option<Quiescence>,
    /// Ob die Session, die dieser Seal nennt, mit dem Ende der Session
    /// schließt (`lineage.closed`). Das Objekt ist inhaltsadressiert und
    /// seine Id steht im Seal-Text — unter einem vertrauenswürdigen
    /// Witness-Seal ist das Ende also bezeugt. Für die Stufe zählt die
    /// Ruhe ([`Self::quiescence`]); das Ende nennt der Report nur noch
    /// ([`AssuranceReport::session_end`]).
    ///
    /// **Vertrag des Aufrufers:** gelesen aus genau dem Objekt, dessen Id
    /// die `session=`-Zeile **dieses** Seals nennt, mit Hash-Prüfung über
    /// die gespeicherten Bytes (wie `ContextStore::get`) — nie aus einem
    /// Index, Cache oder einer anderen Epoche. Und dieses Objekt gehört zur
    /// geprüften Agent-Session: gleiches `agent`, gleiche `lineage.local_id`
    /// — der Seal-Text nennt die `local_id` nicht, und ein erster
    /// Witness-Seal (`previous = -`) einer fremden Session schlösse sonst
    /// diese. Verglichen wird der gespeicherte (redigierte) Wert; eine
    /// redigierte `local_id` passt nie. Sonst setzte der Agent das Feld
    /// selbst.
    ///
    /// Fehlt das Objekt, ist es vergessen (`forget`), lässt es sich nicht
    /// lesen oder nennt der Seal keines (Block-Seal): `false` — fail-closed.
    pub closes_session: bool,
}

/// Das Seal-Material einer Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seals<'a> {
    /// Vor der Evidence-Chain erfasst: kein Seal-Material
    /// ([`crate::model::Provenance::Legacy`]).
    Legacy,
    /// Die versiegelten Bereiche der Agent-Session in **zeitlicher
    /// Reihenfolge**; der letzte ist das Ende. Leer heißt: im
    /// Chain-Zeitalter, aber kein Seal lesbar.
    ///
    /// **Vertrag des Aufrufers:** die Witness-Bereiche über die signierten
    /// `previous`-Verweise der Witness-Kette (nicht über die veränderbare
    /// Rückverweis-Liste im Repository), dazu jeder Rückfall-Bereich
    /// (`agent-hooks/v1`) derselben Agent-Session (`agent`, `local_id`), den
    /// er findet — eingeordnet nach dem `last_event_at` seines Seals. Der
    /// Rückfall hat eine eigene Kette; fehlt er, fällt das hier nicht auf
    /// (Modul-Doku, „Bekannte Grenzen").
    Sealed(&'a [RangeInput]),
}

/// Wie das Fenster der Session vom Datei-Beobachter (`witness-fs/v1`)
/// abgedeckt ist (EA-08, EA-08c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsCoverage {
    /// Eine lückenlose, vertrauenswürdige Epochen-Kette deckt das Fenster.
    Complete,
    /// Die Kette deckt das Fenster, aber der Beobachter war zeitweise blind.
    Gapped {
        /// Die Gründe der Lücken (`overflow`, `ignore_rules`, …), roh.
        reasons: Vec<String>,
    },
    /// Kein verwertbares Fenster (nicht bezeugt, Kette gebrochen, Glied
    /// nicht vertrauenswürdig).
    Unavailable {
        /// Die Ursache, roh (EA-08c: `epoch b3-… missing`, …); `None`, wenn
        /// der Aufrufer sie nicht kennt. Nennt nur Seal-Ids und Gründe, nie
        /// Pfade oder Inhalte.
        cause: Option<String>,
    },
}

/// Was ein Witness-Lauf bei seinem Start über sich festhielt
/// (`witness.start`).
///
/// **Herkunft (Vertrag des Aufrufers):** nur aus den `witness-fs/v1`-Epochen,
/// die das Beobachtungsfenster der Session tragen und deren Seals unter
/// `minds-witness` gegen die vertrauenswürdigen Signer gültig sind — sonst
/// gar nicht. Ein `witness.start` aus einem untergeschobenen Stream könnte
/// ein qualifiziertes Profil behaupten. Spannt die Session mehrere Läufe
/// (Neustart), steht **jeder** Start in der Liste. Ein Start wird nie
/// verworfen, weil sich etwas an ihm nicht lesen ließ: Ein unbekanntes
/// Profil wird `profile: None`, eine fehlende Trennung `isolated: false`
/// — sonst trüge der Rest der Liste ein A2, das ein Teil des Fensters nicht
/// hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessStart {
    /// Das Isolationsprofil; `None`, wenn der Start keines (oder ein diesem
    /// Binary unbekanntes) nennt — dann trägt die Session kein A2.
    pub profile: Option<WitnessProfile>,
    /// Ob der Lauf seine Trennung vom Agenten **belegt** hat (etwa: eigene
    /// Kennung des Witness ≠ Eigentümer des Worktrees, bestandene
    /// Isolationsprobe) — im signierten Material, nicht als Behauptung des
    /// Profilnamens. Heute hält `witness.start` keinen solchen Beleg fest;
    /// bis der Witness ihn schreibt, ist das `false`, und A2 bleibt aus.
    pub isolated: bool,
    /// Der Fingerabdruck des Witness-Schlüssels, falls bekannt.
    pub key: Option<String>,
}

/// Womit ein Intent signiert wurde (EA-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerKind {
    /// FIDO-Schlüssel (`sk-…`): Die Signatur verlangte eine Berührung.
    SecurityKey,
    /// Software-Schlüssel.
    SoftwareKey,
}

impl SignerKind {
    /// Das Anzeige-Wort (`sk key`, `software key`).
    pub const fn word(self) -> &'static str {
        match self {
            Self::SecurityKey => "sk key",
            Self::SoftwareKey => "software key",
        }
    }
}

/// Die Signaturlage eines Intent-Ankers unter `minds-intent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSignature {
    /// Keine Signatur.
    Unsigned,
    /// Eine Signatur liegt vor, wurde aber nicht geprüft (keine
    /// vertrauenswürdigen Signer).
    NotChecked,
    /// Signiert, aber nicht gültig unter `minds-intent` (falscher
    /// Namespace, fremder Schlüssel) — ein Assurance-Fakt, keine
    /// Manipulation der Evidence (EA-15).
    Invalid,
    /// Gültig unter `minds-intent` gegen vertrauenswürdige Signer.
    Valid(SignerKind),
}

/// Ob die Session an eine Anforderung gebunden ist (EA-14/EA-15) — zur
/// Lesezeit berechnet ([`crate::intent::intent_of`]), nie gespeichert (W2).
/// `minds verify` rechnet sie über `intent_of` (EA-15).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IntentState {
    /// Kein Intent-Anker — nur der Prompt (`intent: unbound (prompt
    /// only)`).
    #[default]
    Unbound,
    /// Gebunden.
    Bound {
        /// Der Anker, der am Ende des geprüften Materials galt.
        anchor_id: ContentHash,
        /// Vom Witness als `minds.intent`-Event verkettet. `false`: nur die
        /// lokale Datei (A1) — der Agent kann sie bearbeiten.
        chained: bool,
        /// Die Signaturlage.
        signature: IntentSignature,
        /// Der abgelegte Snapshot hasht auf `content=` des Ankers. `false`
        /// auch, wenn Anker oder Snapshot nicht im Store liegen.
        snapshot_matches: bool,
        /// Positiv belegt: Das erste Intent-Event der Kette hat der Witness
        /// vor dem ersten Hook-Event der Session geschrieben. `false` heißt
        /// „nicht belegt" — die Session lief erst ungebunden, oder ihr
        /// Anfang ist nicht lesbar.
        from_session_start: bool,
        /// Ein späteres Intent-Event nennt einen anderen Anker.
        changed_mid_session: bool,
    },
}

impl IntentState {
    /// Die Intent-Lage in festen Worten (EA-15): `intent not bound`,
    /// `intent unsigned`, `intent signed, sk key`, `intent signed, software
    /// key`, `intent signature not checked`, `intent signature invalid for
    /// minds-intent`. Dieselben Sätze wie die Gründe — eine Wortquelle für
    /// `minds verify` und die TUI.
    pub fn phrase(&self) -> String {
        match self {
            Self::Unbound => Reason::IntentNotBound.text(),
            Self::Bound { signature, .. } => match signature {
                IntentSignature::Valid(kind) => format!("intent signed, {}", kind.word()),
                IntentSignature::Unsigned => Reason::IntentUnsigned.text(),
                IntentSignature::NotChecked => Reason::IntentSignatureNotChecked.text(),
                IntentSignature::Invalid => Reason::IntentSignatureInvalid.text(),
            },
        }
    }
}
/// Das Ergebnis des Replays der entscheidenden Befehle in CI (EA-18b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaySummary {
    /// Der Replay-Record ist unter `minds-anchor` gültig signiert.
    pub signed: bool,
    /// Die entscheidenden Befehle der **Session** (EA-18a, vom Aufrufer
    /// aus der Session selbst bestimmt, nicht aus dem Record) — damit ein
    /// Record, der einen Befehl still auslässt, nicht als vollständig gilt.
    pub decisive: usize,
    /// Entscheidende Befehle, deren Ergebnis sich wiederholte.
    pub reproduced: usize,
    /// Entscheidende Befehle mit `claim not reproduced`.
    pub not_reproduced: usize,
    /// Übersprungene (nicht freigegebene) entscheidende Befehle.
    pub skipped: usize,
}

/// Der Abgleich des Witness-Ledgers mit dem Repository (EA-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerCheck {
    /// Kein Ledger zur Hand.
    NotChecked,
    /// Abgeglichen: Jeder bezeugte Seal liegt im Repository.
    Complete,
    /// Abgeglichen: Diese bezeugten Seals fehlen im Repository — **alle**
    /// fehlenden des Ledgers, ungefiltert. Das Ledger nennt keine Session;
    /// ein fehlender bezeugter Seal heißt, das Repository wurde verändert,
    /// und dann ist keine Session darin intakt (wie `TAMPERED` in EA-12).
    /// Die Reihenfolge ist gleichgültig: Der Grund nennt sie sortiert.
    Missing(Vec<ContentHash>),
}

/// Die Seals mit gültiger `minds-anchor`-Gegenzeichnung (EA-19).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorSummary {
    /// Die gegengezeichneten `seal_id`s.
    pub anchored: BTreeSet<ContentHash>,
}

/// Die Fakten, aus denen [`assess`] rechnet — gelesen, nie gespeichert.
///
/// Intent, Replay und Gegenzeichnungen sind **geprüfte** Fakten des
/// Aufrufers (Signaturen gegen die vertrauenswürdigen Signer); der Report
/// reicht sie unverändert durch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssuranceInput<'a> {
    /// Das Seal-Material.
    pub seals: Seals<'a>,
    /// Das Integritäts-Verdikt über dieses Material (dieselbe Rechnung wie
    /// `minds verify`). `Tampered` macht jede Range zu A0.
    pub integrity: EvidenceVerdict,
    /// Ob sich die Epochen-Kette der Session schließt
    /// ([`crate::model::EvidenceState::chain_closed`] — dieselbe Rechnung
    /// wie `minds verify`). Offen heißt: Ein Glied fehlt (gelöschter Seal),
    /// die Kette gabelt sich oder ein Vorgänger wurde zurückgewiesen — es
    /// gibt eine Range, die hier nicht steht und deren Stufe niemand kennt.
    /// Dann trägt keine Range mehr als A0: Wer einen schwachen Seal löscht,
    /// darf die Gesamtstufe nie heben.
    pub chain_closed: bool,
    /// Der Abgleich mit dem Ledger des Witness (`--witness-home`). Fehlt
    /// dort ein bezeugter Seal im Repository, ist das Material nicht intakt:
    /// jede Range A0. Ohne Ledger (`NotChecked`, etwa in CI) gibt es keine
    /// Kappung — ein gelöschter letzter Bereich fällt dann über das
    /// bezeugte Ende auf ([`RangeInput::closes_session`]).
    pub ledger: &'a LedgerCheck,
    /// Woher die Verknüpfung der Session mit dem geprüften Commit stammt;
    /// `None` außerhalb eines Commit-Kontexts. Eine nur vermutete
    /// ([`EvidenceSource::Heuristic`]) macht jede Range zu A0. Eine
    /// beobachtete (Trailer) belegt nicht, dass die Session den Commit
    /// erklärt — siehe Modul-Doku.
    pub link: Option<EvidenceSource>,
    /// Ob eine vertrauenswürdige `allowed_signers`-Datei übergeben wurde.
    /// Ohne sie zählen weder Witness-Signaturen noch `witness_starts`.
    ///
    /// **Vertrag des Aufrufers:** Die Datei liegt nicht im geprüften
    /// Worktree und nicht an einer Stelle, die der Agent schreiben kann
    /// (eine eingecheckte `allowed_signers` ist so agent-gesteuert wie
    /// `gpg.ssh.allowedSignersFile`); und jeder gemeldete Witness-Principal
    /// gehört zu **diesem** Repository — sonst trüge eine verpflanzte Kette
    /// (Modul-Doku, „Bekannte Grenzen").
    pub trusted_signers: bool,
    /// Die Abdeckung durch den Datei-Beobachter.
    pub observations: &'a FsCoverage,
    /// Die Starts der Witness-Läufe im Fenster der Session (Herkunft: siehe
    /// [`WitnessStart`]); leer, wenn unbekannt.
    pub witness_starts: &'a [WitnessStart],
    /// Die Intent-Lage.
    pub intent: &'a IntentState,
    /// Das Replay-Ergebnis, falls vorhanden.
    pub replay: Option<&'a ReplaySummary>,
    /// Die Gegenzeichnungen, falls vorhanden.
    pub anchors: Option<&'a AnchorSummary>,
}

/// Warum eine Range (oder die Session) nicht höher steht.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
    /// Seal-Material im Chain-Zeitalter, aber kein Seal.
    NoSeal,
    /// Vor der Evidence-Chain erfasst.
    Legacy,
    /// Seal-Material verändert — die Kette rechnet nicht nach.
    IntegrityBroken,
    /// Die Epochen-Kette schließt sich nicht — eine Range fehlt.
    ChainOpen,
    /// Bezeugte Seals aus dem Ledger fehlen im Repository.
    LedgerSealsMissing {
        /// Die ersten [`MAX_LISTED`], sortiert.
        seals: Vec<ContentHash>,
        /// Wie viele insgesamt fehlen.
        total: usize,
    },
    /// Die Verknüpfung mit dem Commit ist nur vermutet.
    Inferred,
    /// Der Scope versiegelt keine Session (entschärft).
    ForeignScope(Shown),
    /// Nur von den Hooks des Agenten beobachtet.
    AgentHooks,
    /// Keine vertrauenswürdigen Signer übergeben.
    SignersUntrusted,
    /// Signatur liegt vor, ungeprüft.
    SignatureNotChecked,
    /// Der Witness-Seal trägt keine Signatur.
    SignatureMissing,
    /// Nicht unter `minds-witness` von einem Witness signiert.
    SignatureNotWitness,
    /// **Andere** Witness-Seals derselben Session haben einen
    /// Integritätsbefund (keine Signatur oder fremder Namespace).
    OtherSealsInvalid {
        /// Die ersten [`MAX_LISTED`], sortiert.
        seals: Vec<ContentHash>,
        /// Wie viele insgesamt.
        total: usize,
    },
    /// Kein verwertbares Beobachtungsfenster; die Ursache entschärft und
    /// gekürzt, falls bekannt.
    NoObservationWindow(Option<Shown>),
    /// Lücken im Beobachtungsfenster — die Gründe aus [`GAP_REASONS`]
    /// (sonst `unknown`), sortiert, ohne Dubletten.
    ObservationGap(Vec<&'static str>),
    /// Das Profil des Witness ist unbekannt.
    ProfileUnknown,
    /// Das Profil trägt (noch) kein A2 ([`profile_qualifies`]).
    ProfileNotQualified(WitnessProfile),
    /// Ein Lauf hat seine Trennung vom Agenten nicht belegt.
    IsolationNotProven,
    /// Der letzte Bereich ist kein vertrauenswürdiger Witness-Bereich — ein
    /// späterer bezeugter Bereich könnte fehlen.
    LastRangeNotWitnessed,
    /// Die Session ruhte beim Checkpoint nicht: Der letzte Witness-Bereich
    /// endet mitten in der Arbeit des Agenten.
    SessionActive(Activity),
    /// Kein Intent-Anker.
    IntentNotBound,
    /// Intent nur lokal gebunden, nicht vom Witness verkettet (EA-14).
    IntentUnchained,
    /// Kein Beleg, dass der Intent ab dem Anfang der Session galt (EA-14).
    IntentStartUnproven,
    /// Der Intent wechselte während der Session (EA-14).
    IntentChangedMidSession,
    /// Anker oder Snapshot fehlen im Store, oder der Snapshot passt nicht
    /// zu `content=` (EA-14).
    IntentSnapshotMismatch,
    /// Intent gebunden, aber nicht signiert.
    IntentUnsigned,
    /// Intent signiert, Signatur ungeprüft.
    IntentSignatureNotChecked,
    /// Intent signiert, aber nicht gültig unter `minds-intent`.
    IntentSignatureInvalid,
    /// Kein Replay.
    NoReplay,
    /// Der Replay-Record ist nicht gültig signiert.
    ReplayUnsigned,
    /// So viele entscheidende Befehle ließen sich nicht reproduzieren.
    ClaimNotReproduced(usize),
    /// Die Session hat keinen entscheidenden Befehl.
    NoDecisiveCommands,
    /// Der Replay-Record deckt nicht alle entscheidenden Befehle der
    /// Session ab.
    ReplayIncomplete {
        /// So viele nennt der Record.
        covered: usize,
        /// So viele hat die Session.
        decisive: usize,
    },
    /// So viele entscheidende Befehle wurden übersprungen (nicht
    /// freigegeben) — ihre Behauptungen sind nicht nachvollzogen.
    DecisiveSkipped(usize),
    /// Der Seal trägt keine `minds-anchor`-Gegenzeichnung.
    NotAnchored,
}

/// Woran die Ruhe scheiterte ([`Reason::SessionActive`]) — mit der
/// laufenden Nummer des Events, damit der Mensch es im Record findet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Ein `PreToolUse` (diese `seq`) ohne sein `PostToolUse`.
    ToolCallOpen(u64),
    /// Das letzte Event (diese `seq`) ist kein `Stop`, `SubagentStop` oder
    /// `SessionEnd`.
    LastEvent(u64),
    /// Das Objekt sagt nichts über die Ruhe (älteres Binary, unvollständig
    /// gelesen, Block-Seal oder fremde Session) und schließt nicht mit
    /// `SessionEnd` — fail-closed.
    Unrecorded,
}

/// Was der Witness über das **Ende** der Session gesehen hat
/// ([`AssuranceReport::session_end`]) — Information neben der Stufe, keine
/// Bedingung (Modul-Doku, „Die bezeugte Ruhe").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// Nichts: Der letzte Bereich ist nicht bezeugt ruhend (die Stufe nennt
    /// den Grund), oder das Material ist nicht geprüft.
    Unknown,
    /// Bezeugt ruhend, ohne `SessionEnd`: Die Session kann weitergehen; ein
    /// späterer Bereich liegt in der nächsten Epoche.
    Open,
    /// Bezeugt mit `SessionEnd` als letztem Event.
    Witnessed,
}

impl Activity {
    /// Die Ruhe aus dem gespeicherten Objekt: `None` oder ein nicht
    /// ruhender Zustand ergibt die Aktivität, Ruhe `None`. Ein älteres
    /// Objekt ohne das Feld, das mit `SessionEnd` schließt, ruht.
    fn of(quiescence: Option<Quiescence>, closes_session: bool) -> Option<Self> {
        match quiescence {
            Some(Quiescence::Quiescent) => None,
            Some(Quiescence::ToolCallOpen { seq }) => Some(Self::ToolCallOpen(seq)),
            Some(Quiescence::Active { seq }) => Some(Self::LastEvent(seq)),
            None if closes_session => None,
            None => Some(Self::Unrecorded),
        }
    }
}

impl Reason {
    /// Ob der Grund die ganze Session trifft (verändertes Material, offene
    /// Kette, Ledger, fremde Signaturbefunde) — dann nennt keine Ausgabe
    /// eine Range dazu: Die Nummer zählte nur die lesbaren Seals und zeigte
    /// auf eine Zeile, die gar nicht der Grund ist.
    pub fn is_session_wide(&self) -> bool {
        matches!(
            self,
            Self::IntegrityBroken
                | Self::ChainOpen
                | Self::LedgerSealsMissing { .. }
                | Self::OtherSealsInvalid { .. }
        )
    }
    /// Der Anzeigetext (englisch, eine Wortquelle für CLI und TUI).
    pub fn text(&self) -> String {
        match self {
            Self::NoSeal => "no seal".into(),
            Self::Legacy => "captured before the evidence chain".into(),
            Self::IntegrityBroken => "integrity broken — seal material was altered".into(),
            Self::ChainOpen => "epoch chain open — a sealed range is missing".into(),
            // Wortlaut wie EA-12; Seal-Ids sind Hashes, nie Agent-Text.
            Self::LedgerSealsMissing { seals, total } => {
                let shown: Vec<String> = seals
                    .iter()
                    .take(MAX_LISTED)
                    .map(ToString::to_string)
                    .collect();
                let more = total.saturating_sub(shown.len());
                let more = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                format!(
                    "witnessed seal(s) missing from the repository: {}{more}",
                    shown.join(", ")
                )
            }
            Self::Inferred => "provenance inferred".into(),
            // In „…“ geklammert: Der Scope stammt aus dem Seal, und
            // ungeklammert läse sich `x; A3 reproduced` wie eine Aussage.
            // `sanitize` entschärft die Klammerzeichen selbst.
            Self::ForeignScope(scope) => format!("seal scope „{scope}“ is not a session scope"),
            Self::AgentHooks => "observed by the agent's hooks only (scope agent-hooks/v1)".into(),
            Self::SignersUntrusted => {
                "witness signature not checked — no trusted allowed_signers".into()
            }
            Self::SignatureNotChecked => "witness signature not checked".into(),
            Self::SignatureMissing => "witness seal missing signature".into(),
            Self::SignatureNotWitness => "witness seal not signed under minds-witness".into(),
            Self::OtherSealsInvalid { seals, total } => {
                let shown: Vec<String> = seals.iter().map(ToString::to_string).collect();
                let more = total.saturating_sub(shown.len());
                let more = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                format!(
                    "witness seal(s) of this session failed the signature check: {}{more}",
                    shown.join(", ")
                )
            }
            Self::NoObservationWindow(None) => {
                "no file-system observation window covering the session".into()
            }
            Self::NoObservationWindow(Some(cause)) => {
                // Geklammert wie der Scope: freier Text aus dem Material.
                format!("no file-system observation window covering the session („{cause}“)")
            }
            Self::ObservationGap(reasons) => {
                if reasons.is_empty() {
                    "observation gap".into()
                } else {
                    format!("observation gap ({})", reasons.join(", "))
                }
            }
            Self::ProfileUnknown => "witness profile unknown".into(),
            Self::ProfileNotQualified(profile) => {
                format!("profile {} not qualified for A2", profile.name())
            }
            Self::IsolationNotProven => "witness isolation not proven — not listed as separated (minds-witness-user@), or the witness shares its user ID, home or binary with the agent".into(),
            Self::LastRangeNotWitnessed => {
                "last sealed range not witnessed — a later range could be missing".into()
            }
            Self::SessionActive(Activity::ToolCallOpen(seq)) => {
                format!("session active at checkpoint (PreToolUse #{seq} open)")
            }
            Self::SessionActive(Activity::LastEvent(seq)) => {
                format!("session active at checkpoint (last event #{seq} is not a stop)")
            }
            Self::SessionActive(Activity::Unrecorded) => {
                "session active at checkpoint (quiescence not recorded)".into()
            }
            Self::IntentNotBound => "intent not bound".into(),
            Self::IntentUnchained => "intent bound (unchained)".into(),
            Self::IntentStartUnproven => "intent not proven from session start".into(),
            Self::IntentChangedMidSession => "intent changed mid-session".into(),
            Self::IntentSnapshotMismatch => {
                "intent snapshot missing or does not match its anchor".into()
            }
            Self::IntentUnsigned => "intent unsigned".into(),
            Self::IntentSignatureNotChecked => "intent signature not checked".into(),
            Self::IntentSignatureInvalid => "intent signature invalid for minds-intent".into(),
            Self::NoReplay => "no replay record".into(),
            Self::ReplayUnsigned => "replay record unsigned".into(),
            Self::ClaimNotReproduced(count) => {
                format!("claim not reproduced ({count} decisive command(s))")
            }
            Self::NoDecisiveCommands => "no decisive commands to reproduce".into(),
            Self::ReplayIncomplete { covered, decisive } if covered > decisive => format!(
                "replay record counts {covered} decisive command(s), the session has {decisive}"
            ),
            Self::ReplayIncomplete { covered, decisive } => {
                format!("replay covers {covered} of {decisive} decisive command(s)")
            }
            Self::DecisiveSkipped(skipped) => {
                format!("{skipped} decisive command(s) skipped, not replayed")
            }
            Self::NotAnchored => "seal not anchored — no minds-anchor countersignature".into(),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// Die Stufe einer versiegelten Range.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RangeAssurance {
    /// Die `seal_id` der Range.
    pub seal: ContentHash,
    /// Die erreichte Stufe.
    pub level: AssuranceLevel,
    /// Warum nicht höher — leer bei A3.
    pub reasons: Vec<Reason>,
}

/// Was über den Witness geprüft bekannt ist (alle Strings entschärft und
/// gekürzt). Nur mit vertrauenswürdigen Signern — sonst wären es Werte, die
/// der Agent selbst geschrieben haben kann.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WitnessFacts {
    /// Die Principals gültiger Witness-Signaturen, sortiert, ohne Dubletten,
    /// höchstens [`MAX_LISTED`].
    pub principals: Vec<String>,
    /// So viele weitere Principals, nicht einzeln genannt.
    pub principals_omitted: usize,
    /// Die Profile der Läufe aus `witness.start`, ohne Dubletten, in der
    /// Reihenfolge `container`, `user`, `managed` — wie aufgezeichnet,
    /// keine Qualifikation (die steht in den Gründen).
    pub profiles: Vec<WitnessProfile>,
    /// Die Schlüssel-Fingerabdrücke aus `witness.start`, sortiert, ohne
    /// Dubletten, höchstens [`MAX_LISTED`].
    pub keys: Vec<String>,
    /// So viele weitere Schlüssel, nicht einzeln genannt.
    pub keys_omitted: usize,
}

/// Die Assurance einer Session — je Range und insgesamt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssuranceReport {
    /// Die schwächste Range; A0 ohne Ranges.
    pub overall: AssuranceLevel,
    /// Der Index der schwächsten Range in [`Self::ranges`] (bei Gleichstand
    /// die erste); `None` ohne Ranges.
    pub weakest: Option<usize>,
    /// Warum `overall` nicht höher liegt: die Gründe der schwächsten Range,
    /// ohne Ranges die Session-Gründe ([`Reason::Legacy`] oder
    /// [`Reason::NoSeal`], dazu [`Reason::Inferred`]).
    pub reasons: Vec<Reason>,
    /// Die Ranges in Ketten-Reihenfolge, wie übergeben.
    pub ranges: Vec<RangeAssurance>,
    /// Was über den Witness geprüft bekannt ist; `None`, wenn nichts — und
    /// immer bei verändertem Material. Oberflächen zeigen die Fakten neben
    /// der Stufe, nie als eigene Aussage über die Session: Auch neben A0
    /// aus `inferred` oder einem fremden Scope stehen sie — sie sagen, wer
    /// die Seals signierte, nicht, dass die Session den Commit erklärt.
    pub witness: Option<WitnessFacts>,
    /// Die Intent-Lage; ohne vertrauenswürdige Signer oder bei verändertem
    /// Material ist eine gemeldete gültige Signatur hier `NotChecked`.
    pub intent: IntentState,
    /// Das Replay-Ergebnis — nur ein signiertes, gegen vertrauenswürdige
    /// Signer geprüftes über intaktem Material; sonst `None` (die Zählung
    /// eines ungeprüften Records ist eine Behauptung).
    pub replay: Option<ReplaySummary>,
    /// Die Gegenzeichnungen — unter denselben Bedingungen, sonst `None`.
    pub anchored: Option<AnchorSummary>,
    /// Die Stufe, wie jede Oberfläche sie zeigt — einmal hier gerechnet,
    /// aus `overall`, dem ersten Grund und den Witness-Fakten
    /// ([`Assurance`]). `minds verify`, Overview, Evidence und die Bindung
    /// lesen dieses Feld; keine leitet die Stufe selbst ab.
    pub rating: Assurance,
    /// Was der Witness über das **Ende** der Session gesehen hat, wenn der
    /// letzte Bereich bezeugt ruht: mit `SessionEnd` ([`SessionEnd::Witnessed`])
    /// oder ohne ([`SessionEnd::Open`]). Keine Bedingung der Stufe (die
    /// verlangt Ruhe, Modul-Doku „Die bezeugte Ruhe") — eine Information,
    /// die `minds verify` bei `Open` unter `Not proven` nennt: Die Session
    /// kann weitergehen, ein späterer Bereich liegt in der nächsten Epoche.
    /// Wie die übrigen Fakten nur über geprüftem, intaktem Material, sonst
    /// [`SessionEnd::Unknown`].
    pub session_end: SessionEnd,
}

/// Rechnet die Assurance einer Session aus — rein und deterministisch:
/// gleiche Fakten, gleicher Report. Liest nichts, schreibt nichts.
pub fn assess(input: &AssuranceInput<'_>) -> AssuranceReport {
    let ranges: &[RangeInput] = match input.seals {
        Seals::Legacy => &[],
        Seals::Sealed(ranges) => ranges,
    };
    let session = SessionFacts::of(input, ranges);
    let assessed: Vec<RangeAssurance> = ranges
        .iter()
        .map(|range| assess_range(input, &session, range))
        .collect();

    let weakest = assessed
        .iter()
        .enumerate()
        .min_by_key(|(index, range)| (range.level, *index))
        .map(|(index, _)| index);
    let (overall, reasons) = match weakest {
        Some(index) => (assessed[index].level, assessed[index].reasons.clone()),
        None => {
            // Dieselbe Reihenfolge wie je Range: Integrität zuerst — EA-12
            // druckt nur den ersten Grund. Eine offene Kette ist ohne Ranges
            // selbstverständlich (der Index schließt sie nie) und wird nicht
            // eigens genannt.
            let mut reasons = Vec::new();
            if altered(input.integrity) {
                reasons.push(Reason::IntegrityBroken);
            }
            reasons.extend(ledger_reason(input));
            reasons.push(match input.seals {
                Seals::Legacy => Reason::Legacy,
                Seals::Sealed(_) => Reason::NoSeal,
            });
            if inferred(input.link) {
                reasons.push(Reason::Inferred);
            }
            (AssuranceLevel::A0Claimed, reasons)
        }
    };

    // Was der Report weiterreicht, ist nur so geprüft wie die Signer, gegen
    // die geprüft wurde — und nur über intaktem Material: Ohne
    // vertrauenswürdige Signer oder neben „integrity broken" / „chain open" /
    // „seal missing" ist nichts davon „signiert", „gegengezeichnet" oder
    // „bezeugt", gleich, was der Aufrufer meldet; ein Principal oder „intent
    // signed" läse sich dort wie eine Bürgschaft.
    let checked = input.trusted_signers && intact(input, ranges);
    // Erschöpfend: Ergänzt EA-14 die Variante, muss hier entschieden werden,
    // was davon ungeprüft erscheinen darf.
    let intent = match input.intent {
        IntentState::Unbound => IntentState::Unbound,
        IntentState::Bound {
            anchor_id,
            chained,
            signature,
            snapshot_matches,
            from_session_start,
            changed_mid_session,
        } => IntentState::Bound {
            anchor_id: anchor_id.clone(),
            // Wie bei der Signatur: Ohne geprüftes, intaktes Material ist
            // nichts davon bezeugt.
            chained: *chained && checked,
            snapshot_matches: *snapshot_matches,
            from_session_start: *from_session_start && checked,
            changed_mid_session: *changed_mid_session,
            signature: match signature {
                IntentSignature::Valid(_) | IntentSignature::Invalid if !checked => {
                    IntentSignature::NotChecked
                }
                IntentSignature::Unsigned
                | IntentSignature::NotChecked
                | IntentSignature::Invalid
                | IntentSignature::Valid(_) => *signature,
            },
        },
    };
    let witness = witness_facts(input, ranges).filter(|_| checked && !ranges.is_empty());
    let rating = rating_of(overall, &reasons, weakest, witness.as_ref(), &intent);
    AssuranceReport {
        overall,
        weakest,
        reasons,
        ranges: assessed,
        // Ohne versiegelten Bereich gibt es nichts, wofür ein Witness
        // bürgte — keine Fakten neben „no seal".
        witness,
        intent,
        // Die Zählung eines unsignierten Records ist eine Behauptung, kein
        // Ergebnis — sie erscheint nicht.
        replay: input.replay.filter(|r| checked && r.signed).copied(),
        anchored: input.anchors.filter(|_| checked).cloned(),
        rating,
        // Wie die übrigen Fakten: nur über geprüftem, intaktem Material.
        session_end: if checked {
            session.session_end
        } else {
            SessionEnd::Unknown
        },
    }
}

/// Die Stufe, wie **jede** Oberfläche sie zeigt — `minds verify`, die
/// Overview, der Evidence-Report und die Bindung im Requirements-Tab —,
/// einmal in [`assess`] gerechnet ([`AssuranceReport::rating`], für ein
/// Seal-Material und eine Signer-Lage: [`assurance`]), in denselben
/// Worten. Keine Oberfläche leitet die Stufe selbst ab: nicht
/// aus den Epochen, nicht aus der Zahl beobachteter Zeilen, nicht aus dem
/// Material des Repositorys allein.
///
/// Drei Varianten: [`Assurance::Unchecked`] — die Witness-Signatur hat
/// hier niemand geprüft; [`Assurance::A1`] — nicht bezeugt, mit der
/// Stufe (`A0 claimed` oder `A1 observed`), dem **ersten** Grund, warum
/// sie nicht höher liegt, und der Nummer der Range, aus der er stammt
/// (1-basiert; `None` für einen Grund, der die ganze Session trifft);
/// [`Assurance::A2`] — bezeugt, mit der Stufe (`A2 witnessed` oder `A3
/// reproduced`) und den geprüften Witness-Fakten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assurance {
    /// Das Material trägt eine Witness-Signatur, die **hier** niemand
    /// geprüft hat — keine vertrauenswürdigen Signer oder kein
    /// `ssh-keygen`. Die Stufe steht bei A1 (fail-closed), und `minds
    /// verify --signers` kann höher urteilen. Nie „no witness ran": Ein
    /// Witness hat signiert.
    Unchecked {
        /// [`Reason::SignersUntrusted`] oder [`Reason::SignatureNotChecked`].
        reason: Reason,
        /// Die Range, aus der der Grund stammt.
        range: Option<usize>,
    },
    /// Nicht bezeugt. [`Reason::AgentHooks`] heißt: Kein Witness lief;
    /// jeder andere Grund auf A1 heißt: Ein Witness hat signiert, trägt
    /// aber kein A2. Auf A0 sah nichts Unabhängiges zu (verändertes
    /// Material, offene Kette, Legacy, kein Seal, …).
    A1 {
        /// [`AssuranceLevel::A0Claimed`] oder [`AssuranceLevel::A1Observed`]
        /// — nie höher ([`assess`] hält das ein).
        level: AssuranceLevel,
        /// Warum nicht höher.
        reason: Reason,
        /// Die Range, aus der der Grund stammt.
        range: Option<usize>,
    },
    /// Bezeugt — von einem getrennten Witness, auf A3 zusätzlich in CI
    /// nachvollzogen.
    A2 {
        /// [`AssuranceLevel::A2Witnessed`] oder
        /// [`AssuranceLevel::A3Reproduced`] — nie niedriger ([`assess`]
        /// hält das ein).
        level: AssuranceLevel,
        /// Die Principals der gültigen Witness-Signaturen (entschärft,
        /// gekürzt, `+n more`), durch `, ` getrennt.
        principal: String,
        /// Die Profile der Witness-Läufe, ohne Dubletten.
        profile: Vec<WitnessProfile>,
        /// Die Intent-Lage in festen Worten ([`IntentState::phrase`]).
        intent: String,
    },
}

impl Assurance {
    /// Die Stufe — für [`Assurance::Unchecked`] A1, der Boden, den das
    /// Material ohne geprüfte Signatur trägt.
    pub const fn level(&self) -> AssuranceLevel {
        match self {
            Self::Unchecked { .. } => AssuranceLevel::A1Observed,
            Self::A1 { level, .. } | Self::A2 { level, .. } => *level,
        }
    }

    /// Das Anzeige-Wort der Stufe (`A2 witnessed`) — dasselbe in jeder
    /// Oberfläche.
    pub const fn word(&self) -> &'static str {
        self.level().word()
    }

    /// Das Kürzel der Stufe (`A2`).
    pub const fn code(&self) -> &'static str {
        self.level().code()
    }

    /// Was in der Klammer hinter dem Wort steht: unter A2 der erste Grund
    /// mit seiner Range (`range 2: witness signature not checked — no
    /// trusted allowed_signers`), ab A2 die Witness-Fakten
    /// (`minds-witness-user@build-07, profile user; intent signed, sk key`).
    pub fn detail(&self) -> String {
        match self {
            Self::Unchecked { reason, range } | Self::A1 { reason, range, .. } => match range {
                Some(n) => format!("range {n}: {}", reason.text()),
                None => reason.text(),
            },
            Self::A2 {
                principal,
                profile,
                intent,
                ..
            } => {
                let mut parts: Vec<String> = Vec::new();
                if !principal.is_empty() {
                    parts.push(principal.clone());
                }
                if !profile.is_empty() {
                    let names: Vec<&str> = profile.iter().map(|p| p.name()).collect();
                    parts.push(format!("profile {}", names.join("/")));
                }
                if parts.is_empty() {
                    intent.clone()
                } else {
                    format!("{}; {intent}", parts.join(", "))
                }
            }
        }
    }

    /// Die Grenze der Stufe als Satz (LIMITS): wer auf dieser Stufe zusah
    /// — und was deshalb nur Aussage des Agenten bleibt. „no witness ran"
    /// steht nur dort, wo das Material keine Witness-Signatur trägt.
    pub fn limit(&self) -> String {
        match self {
            Self::Unchecked { .. } => {
                "witness signature not checked here — minds verify --signers rates this seal.".into()
            }
            Self::A1 {
                level: AssuranceLevel::A0Claimed,
                ..
            } => {
                "claimed at A0 — nothing independent observed this session; every fact here is what the agent recorded.".into()
            }
            Self::A1 {
                reason: Reason::AgentHooks,
                ..
            } => {
                "observed at A1 — no witness ran; the agent's own reports are the only source for file contents.".into()
            }
            Self::A1 { reason, .. } => format!(
                "observed at A1 — a witness signed, but the record does not reach A2 ({}); the agent's own reports are the only source for file contents.",
                reason.text()
            ),
            Self::A2 {
                level: AssuranceLevel::A3Reproduced,
                ..
            } => {
                "reproduced at A3 — the result was reproduced independently; what was not reproduced remains observed, not proven.".into()
            }
            Self::A2 { .. } => {
                "witnessed at A2 — a separated witness observed and signed the record; file contents it did not read are still the agent's reports.".into()
            }
        }
    }

    /// Was das Proof-Modell auf dieser Stufe **nicht** belegt — das
    /// kanonische Vokabular ([`minds_core::evidence::limits_at`]).
    pub fn limitations(
        &self,
    ) -> impl Iterator<Item = &'static minds_core::evidence::ProofSentence> {
        minds_core::evidence::limits_at(self.level().level())
    }
}

impl fmt::Display for Assurance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// Die Stufe aus den Teilen eines Reports — einmal gerechnet, in
/// [`assess`], und als [`AssuranceReport::rating`] abgelegt. Rein und
/// deterministisch: gleiche Teile, gleiche [`Assurance`].
fn rating_of(
    overall: AssuranceLevel,
    reasons: &[Reason],
    weakest: Option<usize>,
    witness: Option<&WitnessFacts>,
    intent: &IntentState,
) -> Assurance {
    let first = || {
        let reason = reasons.first().cloned();
        let range = weakest
            .filter(|_| !reason.as_ref().is_some_and(Reason::is_session_wide))
            .map(|index| index + 1);
        (reason, range)
    };
    match overall {
        AssuranceLevel::A0Claimed => {
            let (reason, range) = first();
            debug_assert!(reason.is_some(), "assess names a reason below A2");
            Assurance::A1 {
                level: overall,
                // `assess` nennt unterhalb von A2 immer einen Grund; ohne
                // Range ist es „no seal".
                reason: reason.unwrap_or(Reason::NoSeal),
                range,
            }
        }
        AssuranceLevel::A1Observed => {
            let (reason, range) = first();
            debug_assert!(reason.is_some(), "assess names a reason below A2");
            match reason {
                Some(reason @ (Reason::SignersUntrusted | Reason::SignatureNotChecked)) => {
                    Assurance::Unchecked { reason, range }
                }
                Some(reason) => Assurance::A1 {
                    level: overall,
                    reason,
                    range,
                },
                // Unerreichbar: Eine A1-Range trägt `AgentHooks` oder die
                // Gründe des A2-Tors.
                None => Assurance::A1 {
                    level: overall,
                    reason: Reason::AgentHooks,
                    range,
                },
            }
        }
        AssuranceLevel::A2Witnessed | AssuranceLevel::A3Reproduced => {
            let (principal, profile) = match witness {
                Some(facts) => {
                    let mut parts: Vec<String> = facts.principals.clone();
                    if facts.principals_omitted > 0 {
                        parts.push(format!("+{} more", facts.principals_omitted));
                    }
                    (parts.join(", "), facts.profiles.clone())
                }
                None => (String::new(), Vec::new()),
            };
            Assurance::A2 {
                level: overall,
                principal,
                profile,
                intent: intent.phrase(),
            }
        }
    }
}

/// Das Seal-Material einer Session, wie der Aufrufer es gelesen hat —
/// unabhängig davon, ob er Signern traut: die Seals mit ihrer
/// Signaturlage, das Integritäts-Verdikt, die Kette, der Ledger, die
/// Verknüpfung mit dem Commit und die Intent-Lage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealMaterial<'a> {
    /// Siehe [`AssuranceInput::seals`].
    pub seals: Seals<'a>,
    /// Siehe [`AssuranceInput::integrity`].
    pub integrity: EvidenceVerdict,
    /// Siehe [`AssuranceInput::chain_closed`].
    pub chain_closed: bool,
    /// Siehe [`AssuranceInput::ledger`].
    pub ledger: &'a LedgerCheck,
    /// Siehe [`AssuranceInput::link`].
    pub link: Option<EvidenceSource>,
    /// Siehe [`AssuranceInput::intent`].
    pub intent: &'a IntentState,
}

/// Was nur mit **vertrauenswürdigen Signern** zählt — die Fakten, die der
/// Aufrufer gegen seine `allowed_signers`-Datei (`--signers`) geprüft hat.
/// `None` an [`assurance`] heißt: keine Signer — kein Witness-Start, kein
/// Beobachtungsfenster, kein Replay, keine Gegenzeichnung zählt, und kein
/// Seal trägt mehr als A1 ([`Assurance::Unchecked`]).
///
/// **Vertrag des Aufrufers:** wie [`AssuranceInput::trusted_signers`] — die
/// Datei liegt nicht im geprüften Worktree und nicht an einer Stelle, die
/// der Agent schreiben kann.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signers<'a> {
    /// Siehe [`AssuranceInput::witness_starts`].
    pub witness_starts: &'a [WitnessStart],
    /// Siehe [`AssuranceInput::observations`].
    pub observations: &'a FsCoverage,
    /// Siehe [`AssuranceInput::replay`].
    pub replay: Option<&'a ReplaySummary>,
    /// Siehe [`AssuranceInput::anchors`].
    pub anchors: Option<&'a AnchorSummary>,
}

/// Ohne Signer: kein Beobachtungsfenster.
static NO_OBSERVATIONS: FsCoverage = FsCoverage::Unavailable { cause: None };

impl<'a> AssuranceInput<'a> {
    /// Die Fakten aus Seal-Material und Signer-Lage — die eine Tür, durch
    /// die `minds verify`, der Index und [`assurance`] gehen: ohne Signer
    /// (`None`) `trusted_signers: false` und nichts, was nur mit Signern
    /// zählte.
    pub fn of(seal: &SealMaterial<'a>, signers: Option<&Signers<'a>>) -> Self {
        Self {
            seals: seal.seals,
            integrity: seal.integrity,
            chain_closed: seal.chain_closed,
            ledger: seal.ledger,
            link: seal.link,
            trusted_signers: signers.is_some(),
            observations: signers.map_or(&NO_OBSERVATIONS, |s| s.observations),
            witness_starts: signers.map_or(&[], |s| s.witness_starts),
            intent: seal.intent,
            replay: signers.and_then(|s| s.replay),
            anchors: signers.and_then(|s| s.anchors),
        }
    }
}

/// Die Stufe eines Seal-Materials, wie **jede** Oberfläche sie zeigt —
/// `minds verify`, die Overview, der Evidence-Report, die Bindung im
/// Requirements-Tab —, aus demselben Material und derselben Signer-Lage:
/// rein und deterministisch, gleiche Eingabe, gleiche [`Assurance`]. Ohne
/// Signer (`None`) trägt ein signierter Witness-Seal [`Assurance::Unchecked`];
/// „no witness ran" sagt nur Material ohne Witness-Signatur.
///
/// Dasselbe wie `assess(&AssuranceInput::of(seal, signers)).rating` — wer
/// den ganzen Report braucht (`minds verify`), nimmt den Weg über
/// [`assess`] und liest dort dieselbe Stufe.
pub fn assurance(seal: &SealMaterial<'_>, signers: Option<&Signers<'_>>) -> Assurance {
    assess(&AssuranceInput::of(seal, signers)).rating
}

/// Die Bedingungen, die für jede Range der Session gleich ausfallen —
/// einmal ausgewertet, in der Reihenfolge der Regeln.
struct SessionFacts {
    /// Die Witness-Seals der Session mit Integritätsbefund (EA-09) — jeder
    /// macht das Material nicht intakt, also **jede** Range A0.
    invalid_seals: BTreeSet<ContentHash>,
    /// Was ein A2 auf Session-Ebene verhindert (Beobachter, Profil, Intent).
    witnessed: Vec<Reason>,
    /// Was ein A3 auf Session-Ebene verhindert (Replay).
    reproduced: Vec<Reason>,
    /// Was über das Ende der Session bezeugt ist.
    session_end: SessionEnd,
}

impl SessionFacts {
    fn of(input: &AssuranceInput<'_>, ranges: &[RangeInput]) -> Self {
        let mut witnessed = Vec::new();
        match input.observations {
            FsCoverage::Complete => {}
            FsCoverage::Gapped { reasons } => {
                let known: BTreeSet<&'static str> = reasons
                    .iter()
                    .map(|reason| {
                        GAP_REASONS
                            .into_iter()
                            .find(|known| known == reason)
                            .unwrap_or("unknown")
                    })
                    .collect();
                witnessed.push(Reason::ObservationGap(known.into_iter().collect()));
            }
            FsCoverage::Unavailable { cause } => {
                witnessed.push(Reason::NoObservationWindow(cause.as_deref().map(Shown::of)));
            }
        }
        // Ein Lauf ohne lesbares Profil zählt wie gar keins: Ein Teil des
        // Fensters lag dann unter unbekannter Isolation. Ohne
        // vertrauenswürdige Signer zählt kein Start — das sagt schon der
        // Signer-Grund; „profile unknown" wäre dort eine falsche Aussage.
        let starts = witness_starts(input);
        if input.trusted_signers
            && (starts.is_empty() || starts.iter().any(|start| start.profile.is_none()))
        {
            witnessed.push(Reason::ProfileUnknown);
        }
        witnessed.extend(
            witness_profiles(input)
                .into_iter()
                .filter(|profile| !profile_qualifies(*profile))
                .map(Reason::ProfileNotQualified),
        );
        // Der Profilname allein gewährt nie A2: Jeder Lauf muss seine
        // Trennung vom Agenten belegt haben — und jeder Witness-Bereich der
        // Session muss unter einem Principal gelten, den ein Mensch als
        // getrennten Witness eingetragen hat. Sonst signierte ein Schlüssel,
        // den der Agent lesen kann (`local`), die Hook-Evidence, während ein
        // getrennter Witness nur die Beobachtungs-Epochen beisteuert.
        let unvouched = ranges.iter().any(|range| {
            matches!(range.signature, SealSignature::Witness { .. })
                && !range.signature.is_separated_witness()
        });
        if unvouched || starts.iter().any(|start| !start.isolated) {
            witnessed.push(Reason::IsolationNotProven);
        }
        // Die Ruhe muss bezeugt sein: Der letzte Bereich (Ketten-
        // Reihenfolge) ist ein vertrauenswürdiger Witness-Bereich, dessen
        // gespeicherte Session in Ruhe endet. Sonst könnte ein späterer,
        // schwächerer Bereich gelöscht sein — die Kette bliebe geschlossen,
        // weil nichts Signiertes auf ihn verweist —, oder der Agent war
        // mitten in der Arbeit, als der Checkpoint lief. Ohne
        // vertrauenswürdige Signer ist nichts bezeugt — das sagt schon der
        // Signer-Grund.
        let last_witnessed = |last: &RangeInput| {
            last.scope == SCOPE_WITNESS_V1
                && input.trusted_signers
                && matches!(last.signature, SealSignature::Witness { .. })
        };
        if input.trusted_signers {
            match ranges.last() {
                Some(last) if last_witnessed(last) => {
                    if let Some(activity) = Activity::of(last.quiescence, last.closes_session) {
                        witnessed.push(Reason::SessionActive(activity));
                    }
                }
                _ => witnessed.push(Reason::LastRangeNotWitnessed),
            }
        }
        // Das Ende der Session ist nur noch Information: Ruht der letzte
        // Bereich bezeugt, sagt `closed`, ob die Session dabei zu Ende war.
        let session_end = match ranges.last() {
            Some(last)
                if last_witnessed(last)
                    && Activity::of(last.quiescence, last.closes_session).is_none() =>
            {
                if last.closes_session {
                    SessionEnd::Witnessed
                } else {
                    SessionEnd::Open
                }
            }
            _ => SessionEnd::Unknown,
        };
        match input.intent {
            IntentState::Unbound => witnessed.push(Reason::IntentNotBound),
            IntentState::Bound {
                anchor_id: _,
                chained,
                signature,
                snapshot_matches,
                from_session_start,
                changed_mid_session,
            } => {
                // Gebunden heißt für A2: bezeugt (verkettet), belegt ab dem
                // Anfang, ohne Wechsel, Snapshot nachrechenbar (EA-14).
                if !chained {
                    witnessed.push(Reason::IntentUnchained);
                } else if !from_session_start {
                    witnessed.push(Reason::IntentStartUnproven);
                }
                if *changed_mid_session {
                    witnessed.push(Reason::IntentChangedMidSession);
                }
                if !snapshot_matches {
                    witnessed.push(Reason::IntentSnapshotMismatch);
                }
                match (signature, input.trusted_signers) {
                    (IntentSignature::Unsigned, _) => witnessed.push(Reason::IntentUnsigned),
                    (IntentSignature::Invalid, true) => {
                        witnessed.push(Reason::IntentSignatureInvalid);
                    }
                    // Wie bei den Seals: ohne vertrauenswürdige Signer ist eine
                    // gemeldete Prüfung — gültig wie ungültig — keine.
                    (IntentSignature::NotChecked, _)
                    | (IntentSignature::Valid(_) | IntentSignature::Invalid, false) => {
                        witnessed.push(Reason::IntentSignatureNotChecked);
                    }
                    (IntentSignature::Valid(_), true) => {}
                }
            }
        }

        let mut reproduced = Vec::new();
        match input.replay {
            None => reproduced.push(Reason::NoReplay),
            Some(replay) => {
                if !replay.signed {
                    reproduced.push(Reason::ReplayUnsigned);
                }
                if replay.not_reproduced > 0 {
                    reproduced.push(Reason::ClaimNotReproduced(replay.not_reproduced));
                }
                // Alle entscheidenden Befehle müssen nachvollzogen sein
                // (ADR-0012: „CI re-execution of decisive commands"): Ein
                // übersprungener ließe seine Behauptung — etwa ein
                // Benchmark-Ergebnis — unbelegt, während A3 sie als
                // reproduziert ausweist (EA-13 zieht `reported_results` bei
                // A3 zurück).
                // Gemessen an den entscheidenden Befehlen der Session, nicht
                // an dem, was der Record aufzählt.
                let covered = replay
                    .reproduced
                    .saturating_add(replay.not_reproduced)
                    .saturating_add(replay.skipped);
                if replay.decisive == 0 {
                    reproduced.push(Reason::NoDecisiveCommands);
                } else if covered != replay.decisive {
                    reproduced.push(Reason::ReplayIncomplete {
                        covered,
                        decisive: replay.decisive,
                    });
                }
                if replay.skipped > 0 {
                    reproduced.push(Reason::DecisiveSkipped(replay.skipped));
                }
            }
        }
        Self {
            invalid_seals: ranges
                .iter()
                .filter(|range| signature_finding(input, range).is_some())
                .map(|range| range.seal.clone())
                .collect(),
            witnessed,
            reproduced,
            session_end,
        }
    }
}

/// Die Tore für eine Range, von unten.
fn assess_range(
    input: &AssuranceInput<'_>,
    session: &SessionFacts,
    range: &RangeInput,
) -> RangeAssurance {
    let verdict = |level, reasons| RangeAssurance {
        seal: range.seal.clone(),
        level,
        reasons,
    };

    // Tor zu A1: Ist das Material intakt, und wer hat überhaupt beobachtet?
    let mut claimed = Vec::new();
    if altered(input.integrity) {
        claimed.push(Reason::IntegrityBroken);
    }
    if !input.chain_closed {
        claimed.push(Reason::ChainOpen);
    }
    claimed.extend(ledger_reason(input));
    if inferred(input.link) {
        claimed.push(Reason::Inferred);
    }
    let scope = range.scope.as_str();
    if scope != SCOPE_AGENT_HOOKS_V1 && scope != SCOPE_WITNESS_V1 {
        claimed.push(Reason::ForeignScope(Shown::of(scope)));
    }
    claimed.extend(signature_finding(input, range));
    // Ein Witness-Seal derselben Session mit Integritätsbefund trifft auch
    // diese Range: Das Material ist nicht intakt (wie `Tampered`).
    // Ein Grund je Range, gekappt — nicht einer je fremdem Seal: Bei vielen
    // unsignierten Seals wüchse der Report sonst quadratisch.
    let others: Vec<&ContentHash> = session
        .invalid_seals
        .iter()
        .filter(|seal| **seal != range.seal)
        .collect();
    if !others.is_empty() {
        claimed.push(Reason::OtherSealsInvalid {
            total: others.len(),
            seals: others.into_iter().take(MAX_LISTED).cloned().collect(),
        });
    }
    if !claimed.is_empty() {
        return verdict(AssuranceLevel::A0Claimed, claimed);
    }
    if scope == SCOPE_AGENT_HOOKS_V1 {
        return verdict(AssuranceLevel::A1Observed, vec![Reason::AgentHooks]);
    }

    // Tor zu A2: ein Witness, dem der Prüfer traut, mit zweitem Auge.
    let mut observed = Vec::new();
    match (&range.signature, input.trusted_signers) {
        (_, false) => observed.push(Reason::SignersUntrusted),
        (SealSignature::NotChecked, true) => observed.push(Reason::SignatureNotChecked),
        // Oben schon A0; hier nur der Vollständigkeit halber erschöpfend.
        (SealSignature::Missing | SealSignature::NotWitness, true) => {}
        (SealSignature::Witness { .. }, true) => {}
    }
    observed.extend(session.witnessed.iter().cloned());
    if !observed.is_empty() {
        return verdict(AssuranceLevel::A1Observed, observed);
    }

    // Tor zu A3: in CI nachvollzogen und gegengezeichnet.
    let mut witnessed = session.reproduced.clone();
    if !input
        .anchors
        .is_some_and(|anchors| anchors.anchored.contains(&range.seal))
    {
        witnessed.push(Reason::NotAnchored);
    }
    if !witnessed.is_empty() {
        return verdict(AssuranceLevel::A2Witnessed, witnessed);
    }
    verdict(AssuranceLevel::A3Reproduced, Vec::new())
}

/// Der Ledger-Grund, falls bezeugte Seals fehlen — sortiert und ohne
/// Dubletten, damit derselbe Befund (gleich, in welcher Reihenfolge der
/// Aufrufer ihn sammelte) denselben Text ergibt.
fn ledger_reason(input: &AssuranceInput<'_>) -> Option<Reason> {
    match input.ledger {
        LedgerCheck::Missing(seals) if !seals.is_empty() => {
            // Gespeichert wird höchstens, was der Text nennt — die Liste
            // stammt aus dem Repository und steht in jeder Range.
            let sorted: BTreeSet<&ContentHash> = seals.iter().collect();
            Some(Reason::LedgerSealsMissing {
                total: sorted.len(),
                seals: sorted.into_iter().take(MAX_LISTED).cloned().collect(),
            })
        }
        LedgerCheck::Missing(_) | LedgerCheck::NotChecked | LedgerCheck::Complete => None,
    }
}

/// Der Integritätsbefund eines Witness-Seals selbst (EA-09): ohne Signatur
/// oder — geprüft — unter fremdem Namespace. Gleich, ob der Aufrufer ihn
/// zusätzlich als `Tampered` meldet.
fn signature_finding(input: &AssuranceInput<'_>, range: &RangeInput) -> Option<Reason> {
    if range.scope != SCOPE_WITNESS_V1 {
        return None;
    }
    match (&range.signature, input.trusted_signers) {
        (SealSignature::Missing, _) => Some(Reason::SignatureMissing),
        (SealSignature::NotWitness, true) => Some(Reason::SignatureNotWitness),
        (SealSignature::NotWitness, false)
        | (SealSignature::NotChecked | SealSignature::Witness { .. }, _) => None,
    }
}

/// Ob das Material intakt ist: unverändert, Kette geschlossen, kein
/// bezeugter Seal fehlt, kein Witness-Seal mit Integritätsbefund.
fn intact(input: &AssuranceInput<'_>, ranges: &[RangeInput]) -> bool {
    !altered(input.integrity)
        && input.chain_closed
        && !matches!(input.ledger, LedgerCheck::Missing(seals) if !seals.is_empty())
        && ranges
            .iter()
            .all(|range| signature_finding(input, range).is_none())
}

/// Ob das Seal-Material verändert ist. Erschöpfend, damit ein neues,
/// scheiterndes Verdikt hier eine Entscheidung erzwingt, statt still als
/// intakt zu gelten.
fn altered(verdict: EvidenceVerdict) -> bool {
    match verdict {
        EvidenceVerdict::Tampered => true,
        EvidenceVerdict::Verified | EvidenceVerdict::Incomplete => false,
    }
}

/// Ob die Verknüpfung nur vermutet ist. Erschöpfend, damit eine neue
/// [`EvidenceSource`] hier eine Entscheidung erzwingt.
fn inferred(link: Option<EvidenceSource>) -> bool {
    match link {
        Some(EvidenceSource::Heuristic) => true,
        Some(
            EvidenceSource::HumanDeclared
            | EvidenceSource::ContentDerived
            | EvidenceSource::Observed,
        )
        | None => false,
    }
}

/// Die Starts, die zählen: ohne vertrauenswürdige Signer keine.
fn witness_starts<'a>(input: &AssuranceInput<'a>) -> &'a [WitnessStart] {
    if input.trusted_signers {
        input.witness_starts
    } else {
        &[]
    }
}

/// Die Profile der zählenden Starts, ohne Dubletten, in fester Reihenfolge.
fn witness_profiles(input: &AssuranceInput<'_>) -> Vec<WitnessProfile> {
    let starts = witness_starts(input);
    [
        WitnessProfile::Container,
        WitnessProfile::User,
        WitnessProfile::Managed,
        WitnessProfile::Local,
    ]
    .into_iter()
    .filter(|profile| starts.iter().any(|start| start.profile == Some(*profile)))
    .collect()
}

/// Die Witness-Fakten: Principals nur aus geprüften Signaturen, Profile und
/// Schlüssel aus den zählenden Starts.
fn witness_facts(input: &AssuranceInput<'_>, ranges: &[RangeInput]) -> Option<WitnessFacts> {
    // Roh entdoppeln, dann kürzen: Zwei Principals mit gleichem Anfang
    // bleiben zwei.
    let principals: BTreeSet<&str> = ranges
        .iter()
        .filter(|range| input.trusted_signers && range.scope == SCOPE_WITNESS_V1)
        .filter_map(|range| match &range.signature {
            SealSignature::Witness { principal } => Some(principal.as_str()),
            _ => None,
        })
        .collect();
    let profiles = witness_profiles(input);
    let keys: BTreeSet<&str> = witness_starts(input)
        .iter()
        .filter_map(|start| start.key.as_deref())
        .collect();
    // Nur wenn ein vertrauenswürdiger Witness-Seal die Session trägt: Neben
    // einer reinen Hooks-Session hieße „profile user" sonst, ein Witness
    // hätte sie gesehen.
    if principals.is_empty() {
        return None;
    }
    Some(WitnessFacts {
        principals_omitted: principals.len().saturating_sub(MAX_LISTED),
        keys_omitted: keys.len().saturating_sub(MAX_LISTED),
        principals: principals.into_iter().take(MAX_LISTED).map(shown).collect(),
        profiles,
        keys: keys.into_iter().take(MAX_LISTED).map(shown).collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use minds_core::evidence::SCOPE_WITNESS_FS_V1;

    use super::*;

    const PRINCIPAL: &str = "minds-witness-user@build-07";

    /// Ein vom Witness verketteter, von Anfang an gültiger Anker mit
    /// passendem Snapshot — nur die Signaturlage variiert.
    fn bound(signature: IntentSignature) -> IntentState {
        IntentState::Bound {
            anchor_id: ContentHash::from_bytes([0xa1; 32]),
            chained: true,
            signature,
            snapshot_matches: true,
            from_session_start: true,
            changed_mid_session: false,
        }
    }

    /// Was ein Bericht ohne geprüftes, intaktes Material von [`bound`]
    /// übrig lässt: nichts davon gilt als bezeugt.
    fn unchecked_intent() -> IntentState {
        IntentState::Bound {
            anchor_id: ContentHash::from_bytes([0xa1; 32]),
            chained: false,
            signature: IntentSignature::NotChecked,
            snapshot_matches: true,
            from_session_start: false,
            changed_mid_session: false,
        }
    }

    /// Wie [`bound`] (gültig signiert), mit abweichenden Merkmalen.
    #[derive(Clone, Copy)]
    struct Traits {
        chained: bool,
        matches: bool,
        from_start: bool,
        changed: bool,
    }

    fn bound_with(change: impl FnOnce(&mut Traits)) -> IntentState {
        let mut t = Traits {
            chained: true,
            matches: true,
            from_start: true,
            changed: false,
        };
        change(&mut t);
        IntentState::Bound {
            anchor_id: ContentHash::from_bytes([0xa1; 32]),
            chained: t.chained,
            signature: IntentSignature::Valid(SignerKind::SecurityKey),
            snapshot_matches: t.matches,
            from_session_start: t.from_start,
            changed_mid_session: t.changed,
        }
    }

    fn seal(n: u8) -> ContentHash {
        ContentHash::from_bytes([n; 32])
    }

    fn witness_range(n: u8) -> RangeInput {
        RangeInput {
            seal: seal(n),
            scope: SCOPE_WITNESS_V1.into(),
            signature: SealSignature::Witness {
                principal: PRINCIPAL.into(),
            },
            // Jeder Witness-Bereich der Fixtures ruht und schließt die
            // Session; es zählt nur der letzte.
            quiescence: Some(Quiescence::Quiescent),
            closes_session: true,
        }
    }

    /// Ein Witness-Bereich, der die Session mitten in der Arbeit des Agenten
    /// versiegelt hat (letztes Event #9 kein Stop) — und sie nicht schließt.
    fn active_range(n: u8) -> RangeInput {
        RangeInput {
            quiescence: Some(Quiescence::Active { seq: 9 }),
            closes_session: false,
            ..witness_range(n)
        }
    }

    fn hooks_range(n: u8) -> RangeInput {
        RangeInput {
            seal: seal(n),
            scope: SCOPE_AGENT_HOOKS_V1.into(),
            signature: SealSignature::Missing,
            quiescence: None,
            closes_session: false,
        }
    }

    /// Die Fakten als eigene Daten — [`AssuranceInput`] leiht nur.
    #[derive(Debug, Clone, PartialEq)]
    struct Fixture {
        legacy: bool,
        ranges: Vec<RangeInput>,
        integrity: EvidenceVerdict,
        chain_closed: bool,
        ledger: LedgerCheck,
        link: Option<EvidenceSource>,
        trusted_signers: bool,
        observations: FsCoverage,
        witness_starts: Vec<WitnessStart>,
        intent: IntentState,
        replay: Option<ReplaySummary>,
        anchors: Option<AnchorSummary>,
    }

    fn start(profile: WitnessProfile) -> WitnessStart {
        WitnessStart {
            profile: Some(profile),
            isolated: true,
            key: None,
        }
    }

    fn unavailable() -> FsCoverage {
        FsCoverage::Unavailable { cause: None }
    }

    impl Fixture {
        /// Jede Bedingung erfüllt: eine `witness/v1`-Range auf A3 (Profil
        /// `user` — das einzige ohne offenes Qualifikations-Tor).
        fn reproduced() -> Self {
            Self {
                legacy: false,
                ranges: vec![witness_range(1)],
                integrity: EvidenceVerdict::Verified,
                chain_closed: true,
                // Wie in CI: ohne Witness-Home kein Ledger.
                ledger: LedgerCheck::NotChecked,
                link: Some(EvidenceSource::Observed),
                trusted_signers: true,
                observations: FsCoverage::Complete,
                witness_starts: vec![WitnessStart {
                    profile: Some(WitnessProfile::User),
                    isolated: true,
                    key: Some("SHA256:abc".into()),
                }],
                intent: bound(IntentSignature::Valid(SignerKind::SecurityKey)),
                replay: Some(ReplaySummary {
                    signed: true,
                    decisive: 3,
                    reproduced: 3,
                    not_reproduced: 0,
                    skipped: 0,
                }),
                anchors: Some(AnchorSummary {
                    anchored: [seal(1)].into_iter().collect(),
                }),
            }
        }

        /// Jede A2-Bedingung erfüllt, kein Replay, keine Gegenzeichnung.
        fn witnessed() -> Self {
            Self {
                replay: None,
                anchors: None,
                ..Self::reproduced()
            }
        }

        fn input(&self) -> AssuranceInput<'_> {
            AssuranceInput {
                seals: if self.legacy {
                    Seals::Legacy
                } else {
                    Seals::Sealed(&self.ranges)
                },
                integrity: self.integrity,
                chain_closed: self.chain_closed,
                ledger: &self.ledger,
                link: self.link,
                trusted_signers: self.trusted_signers,
                observations: &self.observations,
                witness_starts: &self.witness_starts,
                intent: &self.intent,
                replay: self.replay.as_ref(),
                anchors: self.anchors.as_ref(),
            }
        }

        fn assess(&self) -> AssuranceReport {
            assess(&self.input())
        }
    }

    fn texts(reasons: &[Reason]) -> Vec<String> {
        reasons.iter().map(Reason::text).collect()
    }

    #[test]
    fn assurance_rules_table() {
        type Change = fn(&mut Fixture);
        let table: &[(&str, Change, AssuranceLevel, &[&str])] = &[
            (
                "all conditions met",
                |_| {},
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            // A0
            (
                "legacy",
                |f| f.legacy = true,
                AssuranceLevel::A0Claimed,
                &["captured before the evidence chain"],
            ),
            (
                "no seal",
                |f| f.ranges.clear(),
                AssuranceLevel::A0Claimed,
                &["no seal"],
            ),
            (
                "legacy and inferred",
                |f| {
                    f.legacy = true;
                    f.link = Some(EvidenceSource::Heuristic);
                },
                AssuranceLevel::A0Claimed,
                &["captured before the evidence chain", "provenance inferred"],
            ),
            (
                "seal material altered",
                |f| f.integrity = EvidenceVerdict::Tampered,
                AssuranceLevel::A0Claimed,
                &["integrity broken — seal material was altered"],
            ),
            (
                "altered and no seal readable",
                |f| {
                    f.integrity = EvidenceVerdict::Tampered;
                    f.ranges.clear();
                },
                AssuranceLevel::A0Claimed,
                &["integrity broken — seal material was altered", "no seal"],
            ),
            (
                "incomplete coverage does not lower the level",
                |f| f.integrity = EvidenceVerdict::Incomplete,
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "ledger checked, nothing missing",
                |f| f.ledger = LedgerCheck::Complete,
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "ledger checked, empty list of missing seals",
                |f| f.ledger = LedgerCheck::Missing(Vec::new()),
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "a witnessed seal from the ledger is missing",
                |f| f.ledger = LedgerCheck::Missing(vec![seal(7)]),
                AssuranceLevel::A0Claimed,
                &["witnessed seal(s) missing from the repository: \
                     b3-0707070707070707070707070707070707070707070707070707070707070707"],
            ),
            (
                // Das Ledger nennt keine Session: Fehlt ein fremder Seal,
                // ist das Repository verändert — auch für diese Session.
                // Unsortiert und doppelt gemeldet, sortiert genannt.
                "a foreign witnessed seal is missing, reported out of order",
                |f| f.ledger = LedgerCheck::Missing(vec![seal(0xb), seal(0xa), seal(0xb)]),
                AssuranceLevel::A0Claimed,
                &["witnessed seal(s) missing from the repository: \
                     b3-0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a, \
                     b3-0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"],
            ),
            (
                "an open epoch chain hides a range",
                |f| {
                    f.integrity = EvidenceVerdict::Incomplete;
                    f.chain_closed = false;
                },
                AssuranceLevel::A0Claimed,
                &["epoch chain open — a sealed range is missing"],
            ),
            (
                "altered and open",
                |f| {
                    f.integrity = EvidenceVerdict::Tampered;
                    f.chain_closed = false;
                },
                AssuranceLevel::A0Claimed,
                &[
                    "integrity broken — seal material was altered",
                    "epoch chain open — a sealed range is missing",
                ],
            ),
            (
                "provenance inferred",
                |f| f.link = Some(EvidenceSource::Heuristic),
                AssuranceLevel::A0Claimed,
                &["provenance inferred"],
            ),
            (
                "no commit context",
                |f| f.link = None,
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "declared link",
                |f| f.link = Some(EvidenceSource::HumanDeclared),
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "observation scope seals no session",
                |f| f.ranges[0].scope = SCOPE_WITNESS_FS_V1.into(),
                AssuranceLevel::A0Claimed,
                &["seal scope „witness-fs/v1“ is not a session scope"],
            ),
            (
                "unknown scope",
                |f| f.ranges[0].scope = "agent-hooks/v2".into(),
                AssuranceLevel::A0Claimed,
                &["seal scope „agent-hooks/v2“ is not a session scope"],
            ),
            (
                "a scope is compared exactly",
                |f| f.ranges[0].scope = "witness/v1 ".into(),
                AssuranceLevel::A0Claimed,
                &["seal scope „witness/v1 “ is not a session scope"],
            ),
            (
                "every A0 condition is named",
                |f| {
                    f.link = Some(EvidenceSource::Heuristic);
                    f.ranges[0].scope = "x/v1".into();
                },
                AssuranceLevel::A0Claimed,
                &[
                    "provenance inferred",
                    "seal scope „x/v1“ is not a session scope",
                ],
            ),
            // A1
            (
                "agent hooks",
                |f| f.ranges[0] = hooks_range(1),
                AssuranceLevel::A1Observed,
                &["observed by the agent's hooks only (scope agent-hooks/v1)"],
            ),
            (
                "agent hooks, signed by a human",
                |f| {
                    f.ranges[0].scope = SCOPE_AGENT_HOOKS_V1.into();
                    f.ranges[0].signature = SealSignature::NotWitness;
                },
                AssuranceLevel::A1Observed,
                &["observed by the agent's hooks only (scope agent-hooks/v1)"],
            ),
            (
                "no trusted allowed_signers",
                |f| f.trusted_signers = false,
                AssuranceLevel::A1Observed,
                // Ohne Signer zählt weder ein `witness.start` (der
                // Signer-Grund sagt das schon) noch eine als gültig
                // gemeldete Intent-Signatur.
                &[
                    "witness signature not checked — no trusted allowed_signers",
                    "intent signature not checked",
                ],
            ),
            (
                "signature not checked",
                |f| f.ranges[0].signature = SealSignature::NotChecked,
                AssuranceLevel::A1Observed,
                // Der letzte Bereich ist ungeprüft — damit auch die Ruhe.
                &[
                    "witness signature not checked",
                    "last sealed range not witnessed — a later range could be missing",
                ],
            ),
            (
                // EA-09: Ein Witness-Seal ohne Signatur ist ein
                // Integritätsbefund — A0, nicht A1.
                "signature missing",
                |f| f.ranges[0].signature = SealSignature::Missing,
                AssuranceLevel::A0Claimed,
                &["witness seal missing signature"],
            ),
            (
                "a missing signature needs no signers to be seen",
                |f| {
                    f.ranges[0].signature = SealSignature::Missing;
                    f.trusted_signers = false;
                },
                AssuranceLevel::A0Claimed,
                &["witness seal missing signature"],
            ),
            (
                "not signed under minds-witness",
                |f| f.ranges[0].signature = SealSignature::NotWitness,
                AssuranceLevel::A0Claimed,
                &["witness seal not signed under minds-witness"],
            ),
            (
                "a foreign signature is unknown without signers",
                |f| {
                    f.ranges[0].signature = SealSignature::NotWitness;
                    f.trusted_signers = false;
                },
                AssuranceLevel::A1Observed,
                &[
                    "witness signature not checked — no trusted allowed_signers",
                    "intent signature not checked",
                ],
            ),
            (
                "no observation window",
                |f| f.observations = unavailable(),
                AssuranceLevel::A1Observed,
                &["no file-system observation window covering the session"],
            ),
            (
                "no observation window, with its cause",
                |f| {
                    f.observations = FsCoverage::Unavailable {
                        cause: Some("epoch b3-0123 missing".into()),
                    }
                },
                AssuranceLevel::A1Observed,
                &[
                    "no file-system observation window covering the session („epoch b3-0123 missing“)",
                ],
            ),
            (
                "observation gaps",
                |f| {
                    f.observations = FsCoverage::Gapped {
                        reasons: vec!["panic".into(), "overflow".into(), "overflow".into()],
                    }
                },
                AssuranceLevel::A1Observed,
                &["observation gap (overflow, panic)"],
            ),
            (
                "a gap reason outside the closed set is unknown",
                |f| {
                    f.observations = FsCoverage::Gapped {
                        reasons: vec![
                            "overflow), witness isolation proven (x".into(),
                            "vm_paused".into(),
                            "overflow".into(),
                        ],
                    }
                },
                AssuranceLevel::A1Observed,
                &["observation gap (overflow, unknown)"],
            ),
            (
                "observation gap without a reason",
                |f| f.observations = FsCoverage::Gapped { reasons: vec![] },
                AssuranceLevel::A1Observed,
                &["observation gap"],
            ),
            (
                "profile unknown",
                |f| f.witness_starts.clear(),
                AssuranceLevel::A1Observed,
                &["witness profile unknown"],
            ),
            (
                "profile managed (until EA-S1)",
                |f| f.witness_starts = vec![start(WitnessProfile::Managed)],
                AssuranceLevel::A1Observed,
                &["profile managed not qualified for A2"],
            ),
            // Die Hook-Evidence unter einem gewöhnlichen Witness-Principal
            // (etwa dem Schlüssel eines `local`-Witness, den der Agent lesen
            // kann) — auch wenn die Beobachtungs-Epochen getrennt sind.
            (
                "a witness range not under a separated principal",
                |f| {
                    f.ranges[0].signature = SealSignature::Witness {
                        principal: "minds-witness@build-07".into(),
                    }
                },
                AssuranceLevel::A1Observed,
                &[
                    "witness isolation not proven — not listed as separated (minds-witness-user@), or the witness shares its user ID, home or binary with the agent",
                ],
            ),
            // `local` (derselbe Nutzer wie der Agent) nie — auch nicht mit
            // einem Start, der Isolation behauptet.
            (
                "profile local (never)",
                |f| f.witness_starts = vec![start(WitnessProfile::Local)],
                AssuranceLevel::A1Observed,
                &["profile local not qualified for A2"],
            ),
            (
                "profile container (until EA-S2)",
                |f| f.witness_starts = vec![start(WitnessProfile::Container)],
                AssuranceLevel::A1Observed,
                &["profile container not qualified for A2"],
            ),
            (
                "every run's profile counts",
                |f| {
                    f.witness_starts = vec![
                        start(WitnessProfile::User),
                        start(WitnessProfile::Managed),
                        start(WitnessProfile::Container),
                        start(WitnessProfile::Managed),
                    ]
                },
                AssuranceLevel::A1Observed,
                &[
                    "profile container not qualified for A2",
                    "profile managed not qualified for A2",
                ],
            ),
            (
                "several user runs",
                |f| f.witness_starts.push(start(WitnessProfile::User)),
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "one run's profile unreadable",
                |f| {
                    f.witness_starts.push(WitnessStart {
                        profile: None,
                        isolated: true,
                        key: None,
                    })
                },
                AssuranceLevel::A1Observed,
                &["witness profile unknown"],
            ),
            (
                "a profile name without proven isolation",
                |f| f.witness_starts[0].isolated = false,
                AssuranceLevel::A1Observed,
                &[
                    "witness isolation not proven — not listed as separated (minds-witness-user@), or the witness shares its user ID, home or binary with the agent",
                ],
            ),
            (
                "one of several runs without proven isolation",
                |f| {
                    f.witness_starts.push(WitnessStart {
                        profile: Some(WitnessProfile::User),
                        isolated: false,
                        key: None,
                    })
                },
                AssuranceLevel::A1Observed,
                &[
                    "witness isolation not proven — not listed as separated (minds-witness-user@), or the witness shares its user ID, home or binary with the agent",
                ],
            ),
            (
                "intent signature not checked",
                |f| f.intent = bound(IntentSignature::NotChecked),
                AssuranceLevel::A1Observed,
                &["intent signature not checked"],
            ),
            (
                "intent not bound",
                |f| f.intent = IntentState::Unbound,
                AssuranceLevel::A1Observed,
                &["intent not bound"],
            ),
            (
                "intent unsigned",
                |f| f.intent = bound(IntentSignature::Unsigned),
                AssuranceLevel::A1Observed,
                &["intent unsigned"],
            ),
            (
                "the last range was sealed while the agent worked",
                |f| f.ranges[0] = active_range(1),
                AssuranceLevel::A1Observed,
                &["session active at checkpoint (last event #9 is not a stop)"],
            ),
            (
                "the last range was sealed inside a tool call",
                |f| f.ranges[0].quiescence = Some(Quiescence::ToolCallOpen { seq: 4 }),
                AssuranceLevel::A1Observed,
                &["session active at checkpoint (PreToolUse #4 open)"],
            ),
            (
                "an older object without the field that does not close",
                |f| {
                    f.ranges[0].quiescence = None;
                    f.ranges[0].closes_session = false;
                },
                AssuranceLevel::A1Observed,
                &["session active at checkpoint (quiescence not recorded)"],
            ),
            (
                "the session ends in an agent-hooks range",
                |f| {
                    let mut hooks = hooks_range(2);
                    hooks.quiescence = Some(Quiescence::Quiescent);
                    hooks.closes_session = true;
                    f.ranges.push(hooks);
                },
                AssuranceLevel::A1Observed,
                // Gleichstand bei A1: der erste Bereich, der Witness-Bereich
                // — sein Grund ist der unbezeugte letzte Bereich.
                &["last sealed range not witnessed — a later range could be missing"],
            ),
            (
                "an invalid intent signature is unknown without signers",
                |f| {
                    f.trusted_signers = false;
                    f.intent = bound(IntentSignature::Invalid);
                },
                AssuranceLevel::A1Observed,
                &[
                    "witness signature not checked — no trusted allowed_signers",
                    "intent signature not checked",
                ],
            ),
            (
                "intent signature invalid",
                |f| f.intent = bound(IntentSignature::Invalid),
                AssuranceLevel::A1Observed,
                &["intent signature invalid for minds-intent"],
            ),
            (
                "intent bound only through the local file",
                |f| {
                    f.intent = bound_with(|t| {
                        t.chained = false;
                        t.from_start = false;
                    })
                },
                AssuranceLevel::A1Observed,
                // Unverkettet: kein zweiter Grund für den fehlenden Anfang.
                &["intent bound (unchained)"],
            ),
            (
                "intent start not proven",
                |f| f.intent = bound_with(|t| t.from_start = false),
                AssuranceLevel::A1Observed,
                &["intent not proven from session start"],
            ),
            (
                "intent changed mid-session",
                |f| f.intent = bound_with(|t| t.changed = true),
                AssuranceLevel::A1Observed,
                &["intent changed mid-session"],
            ),
            (
                "intent snapshot missing or altered",
                |f| f.intent = bound_with(|t| t.matches = false),
                AssuranceLevel::A1Observed,
                &["intent snapshot missing or does not match its anchor"],
            ),
            (
                "every intent defect is named",
                |f| {
                    f.intent = bound_with(|t| {
                        (t.from_start, t.matches, t.changed) = (false, false, true);
                    })
                },
                AssuranceLevel::A1Observed,
                &[
                    "intent not proven from session start",
                    "intent changed mid-session",
                    "intent snapshot missing or does not match its anchor",
                ],
            ),
            (
                "intent signed with a software key",
                |f| f.intent = bound(IntentSignature::Valid(SignerKind::SoftwareKey)),
                AssuranceLevel::A3Reproduced,
                &[],
            ),
            (
                "every A2 condition is named, in rule order",
                |f| {
                    f.intent = IntentState::Unbound;
                    f.witness_starts.clear();
                    f.observations = unavailable();
                    f.ranges[0].signature = SealSignature::NotChecked;
                },
                AssuranceLevel::A1Observed,
                &[
                    "witness signature not checked",
                    "no file-system observation window covering the session",
                    "witness profile unknown",
                    "last sealed range not witnessed — a later range could be missing",
                    "intent not bound",
                ],
            ),
            (
                "a failed A2 gate hides the A3 conditions",
                |f| {
                    f.intent = IntentState::Unbound;
                    f.replay = None;
                    f.anchors = None;
                },
                AssuranceLevel::A1Observed,
                &["intent not bound"],
            ),
            // A2
            (
                "no replay",
                |f| f.replay = None,
                AssuranceLevel::A2Witnessed,
                &["no replay record"],
            ),
            (
                "replay unsigned",
                |f| f.replay.as_mut().unwrap().signed = false,
                AssuranceLevel::A2Witnessed,
                &["replay record unsigned"],
            ),
            (
                "claim not reproduced",
                |f| {
                    let replay = f.replay.as_mut().unwrap();
                    replay.reproduced = 2;
                    replay.not_reproduced = 1;
                },
                AssuranceLevel::A2Witnessed,
                &["claim not reproduced (1 decisive command(s))"],
            ),
            (
                "no decisive commands",
                |f| {
                    let replay = f.replay.as_mut().unwrap();
                    replay.decisive = 0;
                    replay.reproduced = 0;
                },
                AssuranceLevel::A2Witnessed,
                &["no decisive commands to reproduce"],
            ),
            (
                "every decisive command skipped",
                |f| {
                    let replay = f.replay.as_mut().unwrap();
                    replay.reproduced = 0;
                    replay.skipped = 3;
                },
                AssuranceLevel::A2Witnessed,
                &["3 decisive command(s) skipped, not replayed"],
            ),
            (
                "one skipped beside reproduced ones",
                |f| {
                    let replay = f.replay.as_mut().unwrap();
                    replay.reproduced = 2;
                    replay.skipped = 1;
                },
                AssuranceLevel::A2Witnessed,
                &["1 decisive command(s) skipped, not replayed"],
            ),
            (
                "the record silently omits a decisive command",
                |f| f.replay.as_mut().unwrap().decisive = 4,
                AssuranceLevel::A2Witnessed,
                &["replay covers 3 of 4 decisive command(s)"],
            ),
            (
                "the record lists more than the session ran",
                |f| f.replay.as_mut().unwrap().decisive = 2,
                AssuranceLevel::A2Witnessed,
                &["replay record counts 3 decisive command(s), the session has 2"],
            ),
            (
                "a session without decisive commands, record with some",
                |f| f.replay.as_mut().unwrap().decisive = 0,
                AssuranceLevel::A2Witnessed,
                &["no decisive commands to reproduce"],
            ),
            (
                "no countersignatures",
                |f| f.anchors = None,
                AssuranceLevel::A2Witnessed,
                &["seal not anchored — no minds-anchor countersignature"],
            ),
            (
                "countersignature for another seal",
                |f| f.anchors.as_mut().unwrap().anchored = [seal(9)].into_iter().collect(),
                AssuranceLevel::A2Witnessed,
                &["seal not anchored — no minds-anchor countersignature"],
            ),
            (
                "every A3 condition is named",
                |f| {
                    f.replay = Some(ReplaySummary {
                        signed: false,
                        decisive: 3,
                        reproduced: 0,
                        not_reproduced: 2,
                        skipped: 0,
                    });
                    f.anchors = None;
                },
                AssuranceLevel::A2Witnessed,
                &[
                    "replay record unsigned",
                    "claim not reproduced (2 decisive command(s))",
                    "replay covers 2 of 3 decisive command(s)",
                    "seal not anchored — no minds-anchor countersignature",
                ],
            ),
        ];
        for (name, change, level, reasons) in table {
            let mut fixture = Fixture::reproduced();
            change(&mut fixture);
            let report = fixture.assess();
            assert_eq!(report.overall, *level, "{name}");
            assert_eq!(texts(&report.reasons), *reasons, "{name}");
            if let Some(weakest) = report.weakest {
                assert_eq!(report.ranges[weakest].level, *level, "{name}");
                assert_eq!(report.ranges[weakest].reasons, report.reasons, "{name}");
            }
        }
    }

    #[test]
    fn assurance_overall_is_weakest_range() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![witness_range(1), hooks_range(2), witness_range(3)];
        fixture.anchors = None;
        let report = fixture.assess();

        let levels: Vec<AssuranceLevel> = report.ranges.iter().map(|r| r.level).collect();
        assert_eq!(
            levels,
            [
                AssuranceLevel::A2Witnessed,
                AssuranceLevel::A1Observed,
                AssuranceLevel::A2Witnessed
            ]
        );
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(report.weakest, Some(1));
        assert_eq!(report.ranges[1].seal, seal(2));
        assert_eq!(report.reasons, [Reason::AgentHooks]);

        // Gleichstand: die erste schwächste Range (das Ende bezeugt der
        // letzte Witness-Bereich).
        fixture.ranges = vec![
            witness_range(1),
            hooks_range(2),
            hooks_range(3),
            witness_range(4),
        ];
        assert_eq!(fixture.assess().weakest, Some(1));

        // Endet die Session mit einem Hooks-Bereich, ist ihr letzter Bereich
        // nicht bezeugt: auch die Witness-Bereiche bleiben bei A1.
        fixture.ranges = vec![witness_range(1), hooks_range(2)];
        let report = fixture.assess();
        assert_eq!(report.ranges[0].level, AssuranceLevel::A1Observed);
        assert_eq!(report.ranges[0].reasons, [Reason::LastRangeNotWitnessed]);

        // Nur ein Seal gegengezeichnet: Die andere Range hält die Session
        // auf A2.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![witness_range(1), witness_range(2)];
        let report = fixture.assess();
        assert_eq!(report.ranges[0].level, AssuranceLevel::A3Reproduced);
        assert_eq!(report.ranges[1].level, AssuranceLevel::A2Witnessed);
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.reasons, [Reason::NotAnchored]);
    }

    /// Den **letzten** Bereich zu löschen, öffnet die Kette nicht — auf ihn
    /// verweist nichts Signiertes. Ohne bezeugte Ruhe kommt die Session
    /// dann trotzdem nie über A1.
    #[test]
    fn deleting_the_last_seal_never_lifts_above_a1() {
        // Der Witness fiel aus (mitten in der Arbeit); die Session lief per
        // Hooks weiter.
        let mut fixture = Fixture::reproduced();
        let w1 = active_range(1);
        let w2 = active_range(2);
        fixture.anchors = Some(AnchorSummary {
            anchored: [seal(1), seal(2)].into_iter().collect(),
        });
        fixture.ranges = vec![w1.clone(), w2.clone(), hooks_range(3)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A1Observed);

        // Der Agent löscht den Hooks-Bereich am Ende: Kette geschlossen,
        // kein Ledger — und doch bleibt es bei A1.
        fixture.ranges = vec![w1, w2.clone()];
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::LastEvent(9))]
        );

        // Löscht er einen bezeugten Seal am Ende, nennt ihn das Ledger.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![active_range(1)];
        fixture.ledger = LedgerCheck::Missing(vec![seal(2)]);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A0Claimed);
    }

    /// A2 ohne Sessionende (EA-11): Ein Checkpoint, der die Session ruhend
    /// trifft — der Agent hat geantwortet (`Stop`), der Mensch committet —,
    /// erreicht A2; das Ende der Session ist nur noch Information. Lief der
    /// Agent noch (offenes `PreToolUse`), bleibt es bei A1, mit der
    /// Event-Nummer im Grund.
    #[test]
    fn a_quiescent_checkpoint_is_witnessed_without_a_session_end() {
        // Letztes Event `Stop`, kein `SessionEnd`: A2, Ende nicht bezeugt.
        let mut fixture = Fixture::witnessed();
        fixture.ranges = vec![witness_range(1)];
        fixture.ranges[0].closes_session = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.session_end, SessionEnd::Open);
        // Mit `SessionEnd`: ebenso A2, und das Ende bezeugt.
        fixture.ranges[0].closes_session = true;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.session_end, SessionEnd::Witnessed);
        // Das Ende ist nur so geprüft wie die Signer.
        fixture.trusted_signers = false;
        assert_eq!(fixture.assess().session_end, SessionEnd::Unknown);

        // Mitten im Tool-Aufruf versiegelt: A1, der Grund nennt das Event.
        let mut fixture = Fixture::witnessed();
        fixture.ranges = vec![witness_range(1)];
        fixture.ranges[0].quiescence = Some(Quiescence::ToolCallOpen { seq: 3 });
        fixture.ranges[0].closes_session = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::ToolCallOpen(3))]
        );
        assert_eq!(
            report.reasons[0].text(),
            "session active at checkpoint (PreToolUse #3 open)"
        );
        // Über das Ende ist dann nichts gesagt — die Stufe nennt den Grund.
        assert_eq!(report.session_end, SessionEnd::Unknown);

        // Ein älteres Objekt ohne das Feld: Ruhe nur, wenn es mit
        // `SessionEnd` schließt.
        let mut fixture = Fixture::witnessed();
        fixture.ranges = vec![witness_range(1)];
        fixture.ranges[0].quiescence = None;
        assert_eq!(fixture.assess().overall, AssuranceLevel::A2Witnessed);
        fixture.ranges[0].closes_session = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::Unrecorded)]
        );

        // Der zweite Checkpoint derselben Session: Der ruhende Bereich
        // davor bleibt, der neue ruht ebenso — Kette geschlossen, A2.
        let mut fixture = Fixture::witnessed();
        fixture.ranges = vec![witness_range(1), witness_range(2)];
        for range in &mut fixture.ranges {
            range.closes_session = false;
        }
        assert!(fixture.chain_closed);
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.ranges.len(), 2);
    }

    /// Die bekannten Grenzen (Modul-Doku, „Bekannte Grenzen") — jede als
    /// Behauptung, damit eine Regeländerung sie sichtbar verschiebt und
    /// niemand eine offene Grenze für geschlossen hält.
    #[test]
    fn known_limits_are_pinned() {
        // Ein Witness-Bereich, versiegelt, während der Agent arbeitete.
        let open = active_range;
        // Ein ruhender Zwischenstand: Der Agent hat geantwortet (`Stop`),
        // die Session lief danach weiter — kein `SessionEnd`.
        let resting = |n| RangeInput {
            closes_session: false,
            ..witness_range(n)
        };
        let anchored = |seals: &[u8]| {
            Some(AnchorSummary {
                anchored: seals.iter().map(|n| seal(*n)).collect(),
            })
        };

        // Ein Commit, während der Agent arbeitet: Sein Seal ruht nicht —
        // A1, genau aus diesem Grund, bis ein späterer Checkpoint einen
        // ruhenden Bereich versiegelt.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![open(1)];
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::LastEvent(9))]
        );
        fixture.ranges.push(resting(2));
        fixture.anchors = anchored(&[1, 2]);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // `--resume` (oder einfach: weiterarbeiten): Nach dem ruhenden
        // Bereich geht es weiter, und der Commit trifft den Agenten bei der
        // Arbeit — A1; wird der fortgesetzte Teil gelöscht, steht der
        // ruhende Bereich wieder als letzter.
        let mut fixture = Fixture::reproduced();
        fixture.anchors = anchored(&[1, 2]);
        fixture.ranges = vec![resting(1), open(2)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A1Observed);
        fixture.ranges.pop();
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // Ohne Ledger: ein gelöschter letzter Witness-Seal hinter einem
        // ruhenden Zwischenstand bleibt unbemerkt — jeder ruhende
        // Zwischenstand ist ohne Ledger ein mögliches „Ende".
        let mut fixture = Fixture::reproduced();
        fixture.anchors = anchored(&[1, 2]);
        fixture.ranges = vec![resting(1), resting(2)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);
        fixture.ranges.pop();
        assert_eq!(fixture.ledger, LedgerCheck::NotChecked);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);
        // … mit Ledger nicht.
        fixture.ledger = LedgerCheck::Missing(vec![seal(2)]);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A0Claimed);

        // Ein verschwiegener Rückfall-Bereich: h2 lag zeitlich zwischen w1
        // und w3, aber in eigener Kette. Liegt er vor, zieht er die Session
        // auf A1; fehlt er, bleibt die Witness-Kette geschlossen und das
        // Ledger vollständig — A3.
        let mut fixture = Fixture::reproduced();
        fixture.anchors = anchored(&[1, 3]);
        fixture.ledger = LedgerCheck::Complete;
        fixture.ranges = vec![open(1), hooks_range(2), witness_range(3)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A1Observed);
        fixture.ranges = vec![open(1), witness_range(3)];
        assert!(fixture.chain_closed);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // Ein vorgetäuschtes `Stop` oder `SessionEnd` direkt vor dem Commit
        // — oder eine ganz erfundene Session (neue `local_id`,
        // `SessionStart` … `Stop` über den Socket): ein einziger ruhender
        // Witness-Bereich, A2+ ohne Löschung, auch mit vollständigem Ledger.
        let mut fixture = Fixture::reproduced();
        fixture.ledger = LedgerCheck::Complete;
        fixture.ranges = vec![resting(1)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // Eine verpflanzte Kette: Session samt Kette aus einem anderen
        // Repository unter demselben Witness-Schlüssel kopiert — die Seals
        // nennen kein Repository, das Ledger prüft nur Fehlendes. Erkennt
        // nur ein Aufrufer, der Principal und Repository bindet.
        let mut fixture = Fixture::reproduced();
        fixture.ledger = LedgerCheck::Complete;
        fixture.ranges = vec![witness_range(7)];
        fixture.anchors = anchored(&[7]);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // Eine Lücke im letzten Bereich (etwa ein einmal fail-open
        // gelaufener Hook): Der Checkpoint setzt `quiescence` dann nicht —
        // der Bereich kommt ohne Ruhe an, A1.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![witness_range(1)];
        fixture.ranges[0].quiescence = None;
        fixture.ranges[0].closes_session = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::Unrecorded)]
        );

        // Ein Harness ohne `Stop`/`SessionEnd` (OpenCode): Kein Bereich ruht
        // je — dauerhaft A1, auch nach jedem späteren Checkpoint.
        let mut fixture = Fixture::reproduced();
        fixture.anchors = anchored(&[1, 2]);
        fixture.ranges = vec![open(1), open(2)];
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(
            report.reasons,
            [Reason::SessionActive(Activity::LastEvent(9))]
        );

        // Ein ruhender Zwischenstand, danach am Witness vorbei (h3) — auch
        // mit vollständigem Ledger, denn h3 kennt es nicht.
        let mut fixture = Fixture::reproduced();
        fixture.anchors = anchored(&[1, 2]);
        fixture.ledger = LedgerCheck::Complete;
        // w2 ruht, danach lief die Session per Hooks.
        fixture.ranges = vec![resting(1), resting(2), hooks_range(3)];
        assert_eq!(fixture.assess().overall, AssuranceLevel::A1Observed);
        // Ohne den Hooks-Bereich steht der ruhende Bereich als letzter da.
        fixture.ranges.pop();
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);
        fixture.ledger = LedgerCheck::NotChecked;

        // Dasselbe ohne Blenden: Nach dem ruhenden Zwischenstand versiegelt
        // der Witness weiter (w3, bei der Arbeit), der Agent löscht w3. Ohne
        // Ledger bleibt das unbemerkt …
        let w3 = open(3);
        fixture.ranges = vec![resting(1), resting(2), w3];
        fixture.anchors = Some(AnchorSummary {
            anchored: [seal(1), seal(2), seal(3)].into_iter().collect(),
        });
        assert_eq!(fixture.assess().overall, AssuranceLevel::A1Observed);
        fixture.ranges.pop();
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);
        // … mit Ledger nicht.
        fixture.ledger = LedgerCheck::Missing(vec![seal(3)]);
        assert_eq!(fixture.assess().overall, AssuranceLevel::A0Claimed);
    }

    /// Wer den schwächsten Witness-Seal aus der Mitte der Witness-Kette
    /// löscht, gewinnt nichts: `previous` von w3 zeigt ins Leere, die Kette
    /// ist offen, und die Session fällt auf A0 statt auf das Minimum der
    /// übrigen Ranges zu steigen. (Ein Rückfall-Bereich liegt nicht in dieser
    /// Kette — siehe `known_limits_are_pinned`.)
    #[test]
    fn deleting_a_middle_seal_never_raises_the_level() {
        let mut fixture = Fixture::reproduced();
        // w2 ist nicht gegengezeichnet: A2 zwischen zwei A3-Bereichen.
        fixture.anchors = Some(AnchorSummary {
            anchored: [seal(1), seal(3)].into_iter().collect(),
        });
        fixture.ranges = vec![witness_range(1), witness_range(2), witness_range(3)];
        let before = fixture.assess().overall;
        assert_eq!(before, AssuranceLevel::A2Witnessed);

        fixture.ranges = vec![witness_range(1), witness_range(3)];
        fixture.integrity = EvidenceVerdict::Incomplete;
        fixture.chain_closed = false;
        let after = fixture.assess();
        assert!(after.overall <= before, "{:?}", after.overall);
        assert_eq!(after.overall, AssuranceLevel::A0Claimed);
        assert_eq!(after.reasons, [Reason::ChainOpen]);
    }

    #[test]
    fn assurance_untrusted_signers_caps_at_a1() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![witness_range(1), witness_range(2)];
        fixture.anchors = Some(AnchorSummary {
            anchored: [seal(1), seal(2)].into_iter().collect(),
        });
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);

        // Eine als gültig gemeldete Witness-Signatur zählt nicht ohne
        // vertrauenswürdige Signer — fail-closed, auch bei widersprüchlicher
        // Eingabe.
        fixture.trusted_signers = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        for range in &report.ranges {
            assert_eq!(range.level, AssuranceLevel::A1Observed);
            assert_eq!(
                range.reasons,
                [Reason::SignersUntrusted, Reason::IntentSignatureNotChecked]
            );
        }
        // Weder Principal noch Profil noch Schlüssel: ungeprüft könnte all
        // das vom Agenten stammen.
        assert_eq!(report.witness, None);
        // Auch Intent, Replay und Gegenzeichnungen erscheinen nicht als
        // geprüft.
        assert_eq!(report.intent, unchecked_intent());
        assert_eq!(report.replay, None);
        assert_eq!(report.anchored, None);
    }

    #[test]
    fn altered_material_shows_nothing_as_verified() {
        let mut fixture = Fixture::reproduced();
        let intact = fixture.assess();
        assert!(intact.witness.is_some());
        assert!(intact.replay.is_some());
        assert!(intact.anchored.is_some());

        // Verändert, Kette offen, bezeugter Seal fehlt, ein Witness-Seal
        // ohne oder mit fremder Signatur (EA-09) neben einem gültigen: jedes
        // Mal ist das Material nicht intakt, und nichts erscheint als
        // geprüft — auch nicht der Principal des gültigen Seals.
        let changes: [fn(&mut Fixture); 5] = [
            |f| f.integrity = EvidenceVerdict::Tampered,
            |f| f.chain_closed = false,
            |f| f.ledger = LedgerCheck::Missing(vec![seal(9)]),
            |f| {
                f.ranges.insert(
                    0,
                    RangeInput {
                        signature: SealSignature::Missing,
                        ..witness_range(2)
                    },
                )
            },
            |f| {
                f.ranges.insert(
                    0,
                    RangeInput {
                        signature: SealSignature::NotWitness,
                        ..witness_range(2)
                    },
                )
            },
        ];
        for change in changes {
            let mut fixture = Fixture::reproduced();
            change(&mut fixture);
            let report = fixture.assess();
            assert_eq!(report.overall, AssuranceLevel::A0Claimed);
            // Jede Range, nicht nur die Gesamtstufe: Eine Oberfläche, die je
            // Range rendert, darf neben einem Befund kein „A3" zeigen.
            for range in &report.ranges {
                assert_eq!(range.level, AssuranceLevel::A0Claimed, "{range:?}");
            }
            assert_eq!(report.witness, None);
            assert_eq!(report.intent, unchecked_intent());
            assert_eq!(report.replay, None);
            assert_eq!(report.anchored, None);
        }
        fixture.integrity = EvidenceVerdict::Incomplete;
        assert!(fixture.assess().witness.is_some(), "coverage is no tamper");
    }

    #[test]
    fn listed_material_is_bounded() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges = (1..=20)
            .map(|n| RangeInput {
                signature: SealSignature::Witness {
                    principal: format!("witness-{n:02}@host"),
                },
                ..witness_range(n)
            })
            .collect();
        fixture.witness_starts = (0..20)
            .map(|n| WitnessStart {
                key: Some(format!("SHA256:{n:02}")),
                ..start(WitnessProfile::User)
            })
            .collect();
        let witness = fixture.assess().witness.unwrap();
        assert_eq!(witness.principals.len(), MAX_LISTED);
        assert_eq!(witness.principals_omitted, 20 - MAX_LISTED);
        assert_eq!(witness.keys.len(), MAX_LISTED);
        assert_eq!(witness.keys_omitted, 20 - MAX_LISTED);

        let mut fixture = Fixture::reproduced();
        fixture.ledger = LedgerCheck::Missing((10..30).map(seal).collect());
        let text = fixture.assess().reasons[0].text();
        assert_eq!(text.matches("b3-").count(), MAX_LISTED, "{text}");
        assert!(
            text.ends_with(&format!("(+{} more)", 20 - MAX_LISTED)),
            "{text}"
        );
    }

    #[test]
    fn an_unsigned_replay_shows_no_counts() {
        let mut fixture = Fixture::reproduced();
        fixture.replay.as_mut().unwrap().signed = false;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.reasons, [Reason::ReplayUnsigned]);
        assert_eq!(report.replay, None);
    }

    #[test]
    fn assurance_requires_fs_coverage_for_a2() {
        let mut fixture = Fixture::reproduced();
        fixture.replay = None;
        assert_eq!(fixture.assess().overall, AssuranceLevel::A2Witnessed);

        fixture.observations = unavailable();
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(report.reasons, [Reason::NoObservationWindow(None)]);

        fixture.observations = FsCoverage::Gapped {
            reasons: vec!["overflow".into()],
        };
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A1Observed);
        assert_eq!(texts(&report.reasons), ["observation gap (overflow)"]);
    }

    #[test]
    fn assurance_is_deterministic_and_read_only() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![witness_range(3), hooks_range(1), witness_range(2)];
        let before = fixture.clone();
        let first = fixture.assess();
        let second = fixture.assess();
        assert_eq!(first, second);
        // Garantiert schon die Signatur (`&`, keine innere Veränderlichkeit,
        // kein Store); der Vergleich hält die Zusage der Spec fest.
        assert_eq!(fixture, before, "assess changed its input");
        // Die Ranges bleiben in Ablage-Reihenfolge — keine Sortierung, die
        // „Range 2" je Lauf anders meinen ließe.
        let seals: Vec<ContentHash> = first.ranges.iter().map(|r| r.seal.clone()).collect();
        assert_eq!(seals, [seal(3), seal(1), seal(2)]);

        // Wo die Reihenfolge der Eingabe nichts bedeutet — Starts, fehlende
        // Ledger-Seals, Lückengründe —, ändert sie den Report nicht.
        let shuffled = |reverse: bool| {
            let mut fixture = Fixture::reproduced();
            let mut starts = vec![
                WitnessStart {
                    key: Some("SHA256:b".into()),
                    ..start(WitnessProfile::User)
                },
                WitnessStart {
                    key: Some("SHA256:a".into()),
                    ..start(WitnessProfile::Managed)
                },
            ];
            let mut missing = vec![seal(5), seal(4), seal(6)];
            let mut gaps = vec!["panic".to_string(), "overflow".to_string()];
            if reverse {
                starts.reverse();
                missing.reverse();
                gaps.reverse();
            }
            fixture.witness_starts = starts;
            fixture.ledger = LedgerCheck::Missing(missing);
            fixture.observations = FsCoverage::Gapped { reasons: gaps };
            let mut report = fixture.assess();
            // Witness-Fakten erscheinen hier nicht (Ledger-Befund); für sie
            // dieselbe Probe über intaktem Material.
            fixture.ledger = LedgerCheck::Complete;
            report.witness = fixture.assess().witness;
            report
        };
        assert_eq!(shuffled(false), shuffled(true));
    }

    #[test]
    fn the_report_carries_the_facts_it_was_given() {
        let fixture = Fixture::reproduced();
        let report = fixture.assess();
        assert_eq!(report.intent, fixture.intent);
        assert_eq!(report.replay, fixture.replay);
        assert_eq!(report.anchored, fixture.anchors);
        assert_eq!(
            report.witness,
            Some(WitnessFacts {
                principals: vec![PRINCIPAL.into()],
                principals_omitted: 0,
                profiles: vec![WitnessProfile::User],
                keys: vec!["SHA256:abc".into()],
                keys_omitted: 0,
            })
        );

        // Mehrere Läufe: jedes Profil und jeder Schlüssel einmal.
        let mut fixture = Fixture::reproduced();
        fixture.witness_starts = vec![
            WitnessStart {
                profile: Some(WitnessProfile::Managed),
                isolated: true,
                key: Some("SHA256:b".into()),
            },
            WitnessStart {
                profile: Some(WitnessProfile::User),
                isolated: true,
                key: Some("SHA256:a".into()),
            },
            WitnessStart {
                profile: Some(WitnessProfile::User),
                isolated: true,
                key: Some("SHA256:a".into()),
            },
        ];
        let witness = fixture.assess().witness.unwrap();
        assert_eq!(
            witness.profiles,
            [WitnessProfile::User, WitnessProfile::Managed]
        );
        assert_eq!(witness.keys, ["SHA256:a", "SHA256:b"]);

        // Ohne Witness-Material keine Witness-Fakten.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![hooks_range(1)];
        fixture.witness_starts.clear();
        assert_eq!(fixture.assess().witness, None);

        // Eine reine Hooks-Session nennt keinen Witness — auch wenn Starts
        // vorliegen: „profile user" hieße sonst, ein Witness hätte sie
        // gesehen.
        let mut fixture = Fixture::reproduced();
        fixture.ranges = vec![hooks_range(1)];
        assert_eq!(fixture.assess().witness, None);

        // Ohne versiegelten Bereich auch nicht — selbst wenn Starts
        // vorliegen: neben „no seal" bürgt niemand.
        let mut fixture = Fixture::reproduced();
        fixture.ranges.clear();
        assert_eq!(fixture.assess().witness, None);
        fixture.legacy = true;
        assert_eq!(fixture.assess().witness, None);
    }

    #[test]
    fn strings_from_the_material_are_sanitized() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].signature = SealSignature::Witness {
            principal: "w\u{1b}[2K@host".into(),
        };
        fixture.witness_starts[0].key = Some("k\u{202e}ey".into());
        fixture.observations = FsCoverage::Gapped {
            reasons: vec!["over\u{1b}[31mflow".into()],
        };
        let report = fixture.assess();
        let witness = report.witness.clone().unwrap();
        assert_eq!(witness.principals, [sanitize("w\u{1b}[2K@host")]);
        assert_eq!(witness.keys, [sanitize("k\u{202e}ey")]);
        assert_ne!(witness.principals[0], "w\u{1b}[2K@host");
        assert_ne!(witness.keys[0], "k\u{202e}ey");
        for text in texts(&report.reasons) {
            assert!(!text.contains('\u{1b}'), "{text:?}");
        }

        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].scope = "evil\u{1b}]0;title\u{7}".into();
        for text in texts(&fixture.assess().reasons) {
            assert!(!text.chars().any(char::is_control), "{text:?}");
        }

        // Die Klammer um den Scope lässt sich nicht von innen schließen.
        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].scope = "x“ is fine; A3 reproduced „y".into();
        let text = fixture.assess().reasons[0].text();
        assert_eq!(text.matches('“').count(), 1, "{text:?}");
    }

    #[test]
    fn strings_from_the_material_are_bounded() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].scope = "s".repeat(1_000_000);
        let text = fixture.assess().reasons[0].text();
        assert!(text.contains(&format!("„{}…“", "s".repeat(MAX_SHOWN_CHARS))));
        assert!(
            text.chars().count() < MAX_SHOWN_CHARS + 64,
            "{}",
            text.len()
        );

        // Mehrbyte-Zeichen: Der Schnitt liegt auf einer Zeichengrenze.
        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].scope = "ä🦀".repeat(1_000);
        let text = fixture.assess().reasons[0].text();
        assert!(text.contains(&format!("„{}…“", "ä🦀".repeat(MAX_SHOWN_CHARS / 2))));

        // Ein Klammerzeichen genau vor dem Schnitt wird ganz entschärft —
        // keine halbe Escape-Sequenz vor dem `…`.
        let mut fixture = Fixture::reproduced();
        let head = format!("{}“", "s".repeat(MAX_SHOWN_CHARS - 1));
        fixture.ranges[0].scope = format!("{head}tail");
        let text = fixture.assess().reasons[0].text();
        assert!(
            text.contains(&format!("„{}…“", sanitize(&head))),
            "{text:?}"
        );

        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].signature = SealSignature::Witness {
            principal: "p".repeat(10_000),
        };
        fixture.witness_starts[0].key = Some("k".repeat(10_000));
        let witness = fixture.assess().witness.unwrap();
        assert_eq!(witness.principals[0].chars().count(), MAX_SHOWN_CHARS + 1);
        assert_eq!(witness.keys[0].chars().count(), MAX_SHOWN_CHARS + 1);

        let mut fixture = Fixture::reproduced();
        fixture.observations = FsCoverage::Gapped {
            reasons: (0..1_000)
                .map(|n| format!("r{n:04}"))
                .chain(GAP_REASONS.map(String::from))
                .collect(),
        };
        let report = fixture.assess();
        assert_eq!(
            texts(&report.reasons),
            ["observation gap (ignore_rules, overflow, panic, policy, unavailable, unknown)"]
        );

        // Escapes blähen nicht unbegrenzt auf: 120 Zeichen bleiben 120
        // Escape-Sequenzen.
        let mut fixture = Fixture::reproduced();
        fixture.ranges[0].scope = "\u{202e}".repeat(10_000);
        let text = fixture.assess().reasons[0].text();
        assert!(text.len() < MAX_SHOWN_CHARS * 12, "{}", text.len());
        assert!(!text.contains('\u{202e}'));
    }

    /// Viele unsignierte Witness-Seals lassen den Report nicht quadratisch
    /// wachsen: je Range ein gekappter Grund für die übrigen.
    #[test]
    fn many_invalid_seals_stay_bounded() {
        let mut fixture = Fixture::reproduced();
        fixture.ranges = (0..1_000u16)
            .map(|n| {
                let mut bytes = [0u8; 32];
                bytes[..2].copy_from_slice(&n.to_be_bytes());
                RangeInput {
                    seal: ContentHash::from_bytes(bytes),
                    signature: SealSignature::Missing,
                    ..witness_range(0)
                }
            })
            .collect();
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A0Claimed);
        for range in &report.ranges {
            assert!(range.reasons.len() <= 3, "{:?}", range.reasons);
        }
        let Some(Reason::OtherSealsInvalid { seals, total }) = report.ranges[0].reasons.last()
        else {
            panic!("{:?}", report.ranges[0].reasons);
        };
        assert_eq!(seals.len(), MAX_LISTED);
        assert_eq!(*total, 999);
        assert!(
            report
                .reasons
                .last()
                .unwrap()
                .text()
                .ends_with("(+991 more)")
        );
    }

    #[test]
    fn huge_replay_counts_do_not_overflow() {
        let mut fixture = Fixture::reproduced();
        fixture.replay = Some(ReplaySummary {
            signed: true,
            decisive: usize::MAX,
            reproduced: usize::MAX,
            not_reproduced: usize::MAX,
            skipped: usize::MAX,
        });
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(
            report.reasons,
            [
                Reason::ClaimNotReproduced(usize::MAX),
                Reason::DecisiveSkipped(usize::MAX)
            ]
        );

        fixture.replay = Some(ReplaySummary {
            signed: true,
            decisive: usize::MAX,
            reproduced: usize::MAX,
            not_reproduced: 0,
            skipped: 0,
        });
        assert_eq!(fixture.assess().overall, AssuranceLevel::A3Reproduced);
    }

    /// Identischer Seal, identische Signer-Lage, drei Pfade — `minds verify`
    /// (der Report aus [`assess`], Pfad der `Assurance`-Zeile), die TUI
    /// (Overview, Evidence und Bindung lesen `rating` desselben Reports über
    /// `SessionAssurance`) und der Index ([`assurance`]) — ergeben dieselbe
    /// Stufe, dasselbe Wort, dieselbe Klammer und denselben Grenz-Satz.
    /// Ohne Signer: auf allen Pfaden `Unchecked`.
    #[test]
    fn the_same_seal_and_signers_rate_the_same_on_every_path() {
        let mut fixture = Fixture::reproduced();
        fixture.replay = None;
        let seal = SealMaterial {
            seals: Seals::Sealed(&fixture.ranges),
            integrity: fixture.integrity,
            chain_closed: fixture.chain_closed,
            ledger: &fixture.ledger,
            link: fixture.link,
            intent: &fixture.intent,
        };
        let signers = Signers {
            witness_starts: &fixture.witness_starts,
            observations: &fixture.observations,
            replay: None,
            anchors: fixture.anchors.as_ref(),
        };
        // Die eine Tür: dieselben Fakten, die `verify` von Hand zusammenträgt.
        assert_eq!(AssuranceInput::of(&seal, Some(&signers)), fixture.input());

        // Pfad 1 — `minds verify`: der Report, seine `Assurance`-Zeile.
        let verify = assess(&AssuranceInput::of(&seal, Some(&signers)));
        // Pfad 2 — die TUI: `SessionAssurance.rating` ist dieses Feld.
        let overview = verify.rating.clone();
        // Pfad 3 — der Index und jeder, der nur die Stufe braucht.
        let evidence = assurance(&seal, Some(&signers));
        assert_eq!(verify.rating, evidence);
        assert_eq!(overview, evidence);
        assert_eq!(
            evidence,
            Assurance::A2 {
                level: AssuranceLevel::A2Witnessed,
                principal: PRINCIPAL.into(),
                profile: vec![WitnessProfile::User],
                intent: "intent signed, sk key".into(),
            }
        );
        // Dieselben Worte auf jeder Oberfläche.
        assert_eq!(
            format!("{} ({})", verify.rating.word(), verify.rating.detail()),
            format!("A2 witnessed ({PRINCIPAL}, profile user; intent signed, sk key)")
        );
        assert_eq!(overview.word(), "A2 witnessed");
        assert_eq!(evidence.word(), "A2 witnessed");
        assert_eq!(
            evidence.limit(),
            "witnessed at A2 — a separated witness observed and signed the record; file contents it did not read are still the agent's reports."
        );
        // Dreimal gefragt, dreimal dasselbe — und nichts geschrieben.
        assert_eq!(assurance(&seal, Some(&signers)), evidence);
        assert_eq!(assess(&AssuranceInput::of(&seal, Some(&signers))), verify);

        // Ohne Signer: auf jedem Pfad ungeprüft, nichts zählt, was nur mit
        // Signern zählte.
        let input = AssuranceInput::of(&seal, None);
        assert!(!input.trusted_signers);
        assert_eq!(input.observations, &FsCoverage::Unavailable { cause: None });
        assert!(input.witness_starts.is_empty());
        assert_eq!(input.replay, None);
        assert_eq!(input.anchors, None);
        let unchecked = assurance(&seal, None);
        assert_eq!(assess(&input).rating, unchecked);
        assert_eq!(
            unchecked,
            Assurance::Unchecked {
                reason: Reason::SignersUntrusted,
                range: Some(1),
            }
        );
        assert_eq!(
            unchecked.limit(),
            "witness signature not checked here — minds verify --signers rates this seal."
        );
    }

    /// Die Varianten der Stufe, ihre Worte, Klammern und Grenz-Sätze — je
    /// Stufe einmal; „no witness ran" nur für Material ohne
    /// Witness-Signatur.
    #[test]
    fn the_rating_is_one_derivation_for_every_surface() {
        // A2: bezeugt, mit den geprüften Witness-Fakten in der Klammer.
        let mut fixture = Fixture::reproduced();
        fixture.replay = None;
        let report = fixture.assess();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        let rating = report.rating.clone();
        assert_eq!(
            rating,
            Assurance::A2 {
                level: AssuranceLevel::A2Witnessed,
                principal: PRINCIPAL.into(),
                profile: vec![WitnessProfile::User],
                intent: "intent signed, sk key".into(),
            }
        );
        assert_eq!(rating.word(), "A2 witnessed");
        assert_eq!(rating.code(), "A2");
        assert_eq!(
            rating.detail(),
            format!("{PRINCIPAL}, profile user; intent signed, sk key")
        );
        assert!(
            rating.limit().starts_with("witnessed at A2 — "),
            "{}",
            rating.limit()
        );
        assert!(!rating.limit().contains("no witness ran"));
        assert_eq!(
            rating.limitations().map(|s| s.id).collect::<Vec<_>>(),
            minds_core::evidence::limits_at(minds_core::evidence::Level::A2)
                .map(|s| s.id)
                .collect::<Vec<_>>()
        );
        // Dreimal gefragt, dreimal dasselbe — und der Report blieb, was er
        // war (nichts wird geschrieben).
        let again = fixture.assess();
        assert_eq!(again.rating, rating);
        assert_eq!(again, report);

        // A3: dieselbe Klammer, das Wort der Stufe.
        let rating = Fixture::reproduced().assess().rating;
        assert!(
            matches!(
                rating,
                Assurance::A2 {
                    level: AssuranceLevel::A3Reproduced,
                    ..
                }
            ),
            "{rating:?}"
        );
        assert_eq!(rating.word(), "A3 reproduced");
        assert!(rating.limit().starts_with("reproduced at A3 — "));

        // Dasselbe Material ohne Signer: ungeprüft — A1 als Boden, die
        // Klammer wie die `Assurance`-Zeile von `verify`, der Grenz-Satz
        // verweist auf `--signers`. Nie „no witness ran": Ein Witness hat
        // signiert.
        fixture.trusted_signers = false;
        let rating = fixture.assess().rating;
        assert_eq!(
            rating,
            Assurance::Unchecked {
                reason: Reason::SignersUntrusted,
                range: Some(1),
            }
        );
        assert_eq!(rating.level(), AssuranceLevel::A1Observed);
        assert_eq!(rating.word(), "A1 observed");
        assert_eq!(
            rating.detail(),
            "range 1: witness signature not checked — no trusted allowed_signers"
        );
        assert_eq!(
            rating.limit(),
            "witness signature not checked here — minds verify --signers rates this seal."
        );
        assert!(!rating.limit().contains("no witness ran"));

        // Mit Signern, aber ohne `ssh-keygen`: ebenso ungeprüft.
        fixture.trusted_signers = true;
        fixture.ranges[0].signature = SealSignature::NotChecked;
        let rating = fixture.assess().rating;
        assert!(
            matches!(
                rating,
                Assurance::Unchecked {
                    reason: Reason::SignatureNotChecked,
                    range: Some(1)
                }
            ),
            "{rating:?}"
        );

        // Nur Hooks: A1 — und **nur hier** „no witness ran".
        let mut hooks = Fixture::reproduced();
        hooks.ranges = vec![hooks_range(1)];
        let rating = hooks.assess().rating;
        assert_eq!(
            rating,
            Assurance::A1 {
                level: AssuranceLevel::A1Observed,
                reason: Reason::AgentHooks,
                range: Some(1),
            }
        );
        assert_eq!(
            rating.limit(),
            "observed at A1 — no witness ran; the agent's own reports are the only source for file contents."
        );
        assert_eq!(
            rating.detail(),
            "range 1: observed by the agent's hooks only (scope agent-hooks/v1)"
        );

        // Ein Witness, der signierte, aber kein A2 trägt (Profil `local`):
        // A1 mit dem Grund — nicht „no witness ran".
        let mut local = Fixture::reproduced();
        local.witness_starts = vec![start(WitnessProfile::Local)];
        let rating = local.assess().rating;
        assert!(
            matches!(
                &rating,
                Assurance::A1 {
                    level: AssuranceLevel::A1Observed,
                    reason: Reason::ProfileNotQualified(WitnessProfile::Local),
                    range: Some(1)
                }
            ),
            "{rating:?}"
        );
        assert_eq!(
            rating.limit(),
            "observed at A1 — a witness signed, but the record does not reach A2 (profile local not qualified for A2); the agent's own reports are the only source for file contents."
        );

        // A0: verändertes Material — ein Grund für die ganze Session, ohne
        // Range; Legacy ohne Range.
        let mut altered = Fixture::reproduced();
        altered.integrity = EvidenceVerdict::Tampered;
        let rating = altered.assess().rating;
        assert_eq!(
            rating,
            Assurance::A1 {
                level: AssuranceLevel::A0Claimed,
                reason: Reason::IntegrityBroken,
                range: None,
            }
        );
        assert_eq!(rating.detail(), Reason::IntegrityBroken.text());
        assert!(rating.limit().starts_with("claimed at A0 — "));
        let mut legacy = Fixture::reproduced();
        legacy.legacy = true;
        assert_eq!(
            legacy.assess().rating,
            Assurance::A1 {
                level: AssuranceLevel::A0Claimed,
                reason: Reason::Legacy,
                range: None,
            }
        );
        assert_eq!(legacy.assess().rating.to_string(), "A0 claimed");
    }

    #[test]
    fn levels_are_ordered_and_named() {
        let mut sorted = AssuranceLevel::ALL;
        sorted.sort();
        assert_eq!(sorted, AssuranceLevel::ALL);
        let words: Vec<String> = AssuranceLevel::ALL
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            words,
            ["A0 claimed", "A1 observed", "A2 witnessed", "A3 reproduced"]
        );
        let codes: Vec<&str> = AssuranceLevel::ALL.iter().map(|a| a.code()).collect();
        assert_eq!(codes, ["A0", "A1", "A2", "A3"]);
        for level in AssuranceLevel::ALL {
            assert_eq!(AssuranceLevel::parse(level.code()), Some(level));
        }
        for wrong in ["a2", "A4", "A2 witnessed", " A2", ""] {
            assert_eq!(AssuranceLevel::parse(wrong), None, "{wrong:?}");
        }
        assert_eq!(SignerKind::SecurityKey.word(), "sk key");
        assert_eq!(SignerKind::SoftwareKey.word(), "software key");
    }

    /// Alle `.rs`-Dateien unter `dir`, rekursiv.
    fn sources(dir: &Path) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    out.push((path, text));
                }
            }
        }
        assert!(!out.is_empty(), "no sources under {}", dir.display());
        out
    }

    /// Woran eine gespeicherte Stufe im Code eines Schreibpfads erkennbar
    /// wäre (kleingeschrieben, ohne Kommentarzeilen): ein JSON-Schlüssel
    /// oder ein serialisiertes Feld, das eine Stufe, eine
    /// Reconciliation-Klasse, eine Korroboration oder ein Replay-Urteil
    /// behauptet (W2), ein Stufen-Kürzel als String-Literal, oder ein
    /// Import dieses Moduls. Bewusst strukturell statt eines Wortverbots:
    /// Prosa über Stufen (Doku, EA-13s `Level` in `minds-core`) bleibt
    /// erlaubt. Für `minds-cli` ist das ein **Stolperdraht**, kein Beweis:
    /// Wer einen Stufen-Code unter neutralem Schlüssel ablegt — etwa aus
    /// einem Ausgabe-Pfad als `&str` an einen Schreibpfad gereicht —,
    /// umgeht ihn. Hart sind nur die Crate-Grenzen: Die Schreib-Crates
    /// hängen nicht am Reader, und der Reader hat kein serde.
    ///
    /// W2 nennt auch `reproduced`: Das CI-signierte Replay-Record aus EA-18b
    /// trägt je Befehl `"verdict":"reproduced"` (die Form legt EA-18b fest).
    /// Aufgelöst über [`REPLAY_VERDICTS`]: Nur die Datei des Record-Typs darf
    /// genau diese beiden Urteils-Wörter tragen — sie sind die Beobachtung
    /// des CI-Laufs je argv, keine Stufe; `A3 reproduced` und `claim not
    /// reproduced` rechnet der Reader (`replay::summarize`) gegen die
    /// entscheidenden Befehle der Session selbst, nur unter gültiger
    /// `minds-anchor`-Signatur. Jedes andere Muster bleibt dort verboten.
    const STORED_CLAIMS: &[&str] = &[
        "\"assurance",
        "\"level\"",
        "\"corroborat",
        "\"uncorroborat",
        "\"reproduced\"",
        "\"not_reproduced\"",
        "\"reconciliation\"",
        "\"recon_class\"",
        "\"explained",
        "\"unexplained\"",
        "\"a0\"",
        "\"a1\"",
        "\"a2\"",
        "\"a3\"",
        "assurance:",
        "assurance_level",
        "corroboration",
        "corroborated:",
        "uncorroborated:",
        "reproduced:",
        "explained:",
        "recon_class:",
        "reconclass",
        "assurance::",
        "assurance as ",
        "minds_reader as ",
    ];

    /// W2: Eine Stufe wird berechnet, nie gespeichert.
    ///
    /// - Kein Schreibpfad gespeicherter Objekte trägt ein
    ///   [`STORED_CLAIMS`]-Muster: die Crates, die gespeicherte Objekte
    ///   definieren, schreiben oder signieren, und `minds-cli` im Ganzen —
    ///   bis auf eine ausdrückliche Liste reiner Ausgabe-Pfade (`verify`,
    ///   `audit --export`, Rendering): Eine neue Datei ist gescannt, bis
    ///   jemand sie bewusst als Ausgabe einträgt; Ausgabe-Pfade dürfen das
    ///   Modul nicht weiterreichen.
    /// - Jedes Crate des Workspace ist eingeordnet (gescannt oder begründet
    ///   ausgenommen) — ein neues fällt auf, statt ungescannt zu bleiben.
    /// - Diese Crates hängen nicht am Reader.
    /// - Dieses Modul selbst fasst weder Store noch Git an.
    /// - Der Reader hat keine serde-Abhängigkeit: Ohne sie gibt es (Orphan-
    ///   Regel) keinen `Serialize`-Impl für seine Typen.
    ///
    /// Liest die Nachbar-Crates über `CARGO_MANIFEST_DIR/..` — gültig im
    /// Workspace; der Reader wird nicht einzeln paketiert.
    #[test]
    fn assurance_is_never_stored() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        // Jedes Crate des Workspace ist eingeordnet — ein neues fällt hier
        // auf, statt still ungescannt zu bleiben.
        const STORED: &[&str] = &[
            "minds-core",
            "minds-capture",
            "minds-store",
            "minds-redact",
            "minds-git",
            "minds-attest",
            "minds-gitlab",
            "minds-metrics",
        ];
        const NOT_STORED: &[(&str, &str)] = &[
            ("minds-cli", "Datei für Datei unten gescannt"),
            (
                "minds-reader",
                "rechnet die Stufe; schreibt nur gerenderte Seiten, nie Refs",
            ),
            ("minds-tui", "Terminal-Oberfläche, nur Ausgabe"),
        ];
        for entry in std::fs::read_dir(crates).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                STORED.contains(&name.as_str())
                    || NOT_STORED.iter().any(|(known, _)| *known == name),
                "crate {name} is neither scanned nor classified as not storing"
            );
        }
        // `xtask` (Workspace-Mitglied außerhalb von `crates/`): Entwickler-
        // Werkzeug, läuft nie auf Evidence.
        assert!(crates.parent().unwrap().join("xtask").is_dir());

        let mut files = Vec::new();
        for name in STORED {
            files.extend(sources(&crates.join(name).join("src")));
            let manifest = std::fs::read_to_string(crates.join(name).join("Cargo.toml")).unwrap();
            assert!(
                !manifest.contains("minds-reader"),
                "{name} depends on the reader"
            );
        }
        // `minds-cli`: alles, außer den reinen Ausgabe-Pfaden — eine neue
        // Datei wird also gescannt, bis jemand sie hier bewusst als Ausgabe
        // einträgt. Keiner dieser Pfade schreibt unter `refs/minds/`, ins
        // Journal oder signiert (geprüft beim Anlegen der Liste).
        //
        // `audit.rs` ist eine bewusste W2-Entscheidung: `audit --export`
        // schreibt ein **abgeleitetes** Bündel für Prüfer (EA-13 legt die
        // Stufe ausdrücklich hinein) — eine Ausgabe wie `verify`, kein
        // gespeichertes Objekt unter `refs/minds/`, und keine Quelle, aus
        // der Minds je wieder liest.
        const OUTPUT_ONLY: &[&str] = &[
            "agent_help.rs", // Hilfetext für Agenten
            "audit.rs",      // `audit --export`: abgeleitetes Bündel (siehe oben)
            "blame.rs",      // Ausgabe
            "brief_cmd.rs",  // Ausgabe
            "context.rs",    // Kontext auf stdout
            "doctor.rs",     // Diagnose
            "fsck.rs",       // Prüfung, Ausgabe
            "inspect.rs",    // Ausgabe
            "metrics.rs",    // Ausgabe
            "recall.rs",     // Ausgabe
            "recap.rs",      // Ausgabe
            "render.rs",     // Terminal-Rendering
            "render_cmd.rs", // statische Seite, kein Ref
            "search.rs",     // Ausgabe
            "seals_cmd.rs",  // Ausgabe
            "show.rs",       // Ausgabe
            "stack.rs",      // Ausgabe
            "text.rs",       // Entschärfen für die Ausgabe
            "verify_cmd.rs", // Prüfung, Ausgabe (EA-12 druckt die Stufe hier)
            "verify_cmd",    // dito
            "why.rs",        // Ausgabe
        ];
        let cli = crates.join("minds-cli").join("src");
        let mut cli_files = 0;
        for (path, text) in sources(&cli) {
            let relative = path.strip_prefix(&cli).unwrap();
            let first = relative.components().next().unwrap().as_os_str();
            if OUTPUT_ONLY.iter().any(|output| first == *output) {
                // Ein Ausgabe-Pfad darf die Stufe nicht an einen
                // Schreibpfad weiterreichen.
                if let Err(finding) = reexports_assurance(&text) {
                    panic!("{}: {finding}", path.display());
                }
                continue;
            }
            cli_files += 1;
            files.push((path, text));
        }
        assert!(cli_files > 20, "scanned only {cli_files} CLI files");
        for output in OUTPUT_ONLY {
            assert!(cli.join(output).exists(), "stale exclusion {output}");
        }

        let module = include_str!("assurance.rs");
        let (body, _) = module.split_once("#[cfg(test)]").unwrap();
        assert!(body.contains("pub fn assess("), "split at the wrong place");
        for layer in ["minds_store", "minds_git", "std::fs", "std::io"] {
            assert!(!body.contains(layer), "assurance touches {layer}");
        }

        for (path, text) in &files {
            let (record, words) = REPLAY_VERDICTS;
            let allowed = if path.ends_with(record) { words } else { &[] };
            if let Err(finding) = scan_allowing(text, allowed) {
                panic!("{}:{finding}", path.display());
            }
        }
        assert!(
            files
                .iter()
                .any(|(path, _)| path.ends_with(REPLAY_VERDICTS.0)),
            "stale exemption {}",
            REPLAY_VERDICTS.0
        );

        // Wer serde im Reader braucht, entscheidet W2 neu — und passt
        // diesen Test an.
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .unwrap();
        let serde = manifest.lines().map(str::trim).any(|line| {
            line.ends_with(".serde]")
                || line.contains("package = \"serde\"")
                || line.starts_with("serde_derive")
                || line
                    .strip_prefix("serde")
                    .is_some_and(|rest| rest.starts_with([' ', '=', '.']))
        });
        assert!(!serde, "minds-reader depends on serde");
        assert!(manifest.contains("serde_json"), "manifest not parsed");
    }

    /// Der Guard schlägt an, wo er soll — und nicht bei Prosa.
    #[test]
    fn the_stored_claim_patterns_match_a_leak() {
        let leaks = [
            r#"serde_json::json!({"profile": profile, "assurance": level.code()})"#,
            r#"    #[serde(rename = "level")]"#,
            "    pub assurance: String,",
            r#"let class = "explained_fs_only";"#,
            r#"payload.insert("a2".into(), true);"#,
            "use minds_reader::assurance::assess;",
            "use minds_reader::{assurance, model};",
            // Umbenennen, um den Mustern auszuweichen (Stolperdraht gegen
            // Versehen, kein Schutz gegen Absicht — siehe Doku oben).
            "use minds_reader as mr;",
            "    assurance as lvl,",
            "    pub corroborated: bool,",
            "    pub recon_class: ReconClass,",
            "    reproduced: bool,",
        ];
        for leak in leaks {
            assert!(stored_claim(leak).is_some(), "{leak}");
        }
        let code = [
            "pub holds_from: Level,",
            r#"short: "who controls the keys","#,
            "use minds_reader::model::EvidenceVerdict;",
            // Gate-Flags (EA-02, EA-12) sind Argumente, keine Felder eines
            // gespeicherten Objekts.
            r#"require_explained: parsed.value("--require-explained"),"#,
            "    require_assurance: Option<String>,",
        ];
        for line in code {
            assert_eq!(stored_claim(line), None, "{line}");
        }
    }

    /// Die Scan-Logik selbst, an künstlichen Quellen: Was sie übersähe,
    /// stünde sonst nur in einer Annahme.
    #[test]
    fn the_scan_follows_the_file_structure() {
        let leak = r#"    let _ = serde_json::json!({"assurance": level});"#;
        // Ein Testmodul am Ende beendet die Datei.
        assert_eq!(
            scan(&format!(
                "fn a() {{}}\n#[cfg(test)]\nmod tests {{\n{leak}\n}}\n"
            )),
            Ok(())
        );
        // Eines mittendrin nicht: Was danach kommt, wird bemängelt.
        for after in [
            "fn write() {}",
            "pub fn write() {}",
            "async fn write() {}",
            "unsafe impl Send for X {}",
            "extern \"C\" {}",
            "my_macro! { x }",
        ] {
            let source = format!("#[cfg(test)]\nmod tests {{\n}}\n{after}\n");
            assert!(scan(&source).is_err(), "{after}");
        }
        // Ein eingerücktes (verschachteltes) Testmodul beendet nichts —
        // der Rest des äußeren Moduls wird weiter gescannt.
        let nested =
            format!("mod inner {{\n    #[cfg(test)]\n    mod tests {{\n    }}\n{leak}\n}}\n");
        assert!(scan(&nested).is_err());
        // `mod tests;` (eigene Datei) beendet nichts.
        assert!(scan(&format!("#[cfg(test)]\nmod tests;\n{leak}\n")).is_err());
        // Mehrzeilige Import-Gruppen und Glob-Importe des Readers.
        assert!(scan("use minds_reader::{\n    assurance as lvl,\n    model,\n};\n").is_err());
        assert!(scan("use minds_reader::{\n    model,\n};\nfn assurance_doc() {}\n").is_ok());
        assert!(scan("use minds_reader::*;\n").is_err());
        // Kommentare sind Prosa.
        assert!(scan(&format!("// {leak}\n")).is_ok());
        // Ausgabe-Pfade dürfen die Stufe nicht weiterreichen.
        assert!(reexports_assurance("pub(crate) use minds_reader::assurance::assess;").is_err());
        assert!(reexports_assurance("pub use minds_reader::{assurance, model};").is_err());
        assert!(reexports_assurance("use minds_reader::assurance::assess;").is_ok());
    }

    /// Prüft eine Quelldatei eines Schreibpfads. `Err` nennt Zeile und Fund.
    ///
    /// - Ein **unverschachteltes** Inline-Testmodul (`#[cfg(test)]` in
    ///   Spalte 0, gefolgt von `mod … {`) beendet die Datei — geprüft, nicht
    ///   angenommen: Folgt ihm noch ein Element auf oberster Ebene, ist das
    ///   ein Fund. Ein eingerücktes beendet nichts (fail-closed).
    /// - Kommentarzeilen sind Prosa.
    /// - Mehrzeilige `use minds_reader::{…}`-Gruppen werden als Ganzes
    ///   betrachtet; ein Glob-Import des Readers ist ein Fund.
    fn scan(text: &str) -> Result<(), String> {
        scan_allowing(text, &[])
    }

    /// Die eine Ausnahme von [`STORED_CLAIMS`] (EA-18b): der Record-Typ und
    /// die beiden Urteils-Wörter, die seine Form verlangt.
    const REPLAY_VERDICTS: (&str, &[&str]) = (
        "minds-core/src/replay.rs",
        &["\"reproduced\"", "\"not_reproduced\""],
    );

    /// Wie [`scan`], aber die Muster in `allowed` gelten nicht.
    fn scan_allowing(text: &str, allowed: &[&str]) -> Result<(), String> {
        let lines: Vec<&str> = text.lines().collect();
        let mut reader_group = false;
        for (number, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if *line == "#[cfg(test)]"
                && lines.get(number + 1).is_some_and(|next| {
                    let next = next.trim();
                    next.starts_with("mod ") && next.ends_with('{')
                })
            {
                return match lines[number + 2..]
                    .iter()
                    .position(|later| top_level_item(later))
                {
                    Some(at) => Err(format!(
                        "{}: item after the inline test module is not scanned: {}",
                        number + 3 + at,
                        lines[number + 2 + at]
                    )),
                    None => Ok(()),
                };
            }
            if code.starts_with("//") {
                continue;
            }
            let lower = code.to_lowercase();
            let finding = |claim: &str| {
                Err(format!(
                    "{}: stored-object path carries {claim:?}: {line}",
                    number + 1
                ))
            };
            if lower.contains("minds_reader::{") && !lower.contains('}') {
                reader_group = true;
            }
            if reader_group {
                if lower.contains("assurance") {
                    return finding("minds_reader::{…assurance…}");
                }
                if lower.contains('}') {
                    reader_group = false;
                }
            }
            if lower.contains("minds_reader::*") {
                return finding("minds_reader::*");
            }
            if let Some(claim) = stored_claim_except(code, allowed) {
                return finding(claim);
            }
        }
        Ok(())
    }

    /// Die Ausnahme ist eng: nur diese Datei, nur diese zwei Wörter.
    #[test]
    fn the_replay_verdict_exemption_is_narrow() {
        let (_, words) = REPLAY_VERDICTS;
        let verdict = r#"            Self::Reproduced => "reproduced","#;
        assert!(scan(verdict).is_err(), "elsewhere still a finding");
        assert!(scan_allowing(verdict, words).is_ok());
        assert!(scan_allowing(r#"    let x = "not_reproduced";"#, words).is_ok());
        for still in [
            r#"    #[serde(rename = "level")]"#,
            r#"    let level = "a3";"#,
            r#"    json!({"assurance": 3})"#,
            "    pub reproduced: bool,",
        ] {
            assert!(scan_allowing(still, words).is_err(), "{still}");
        }
    }

    /// Ob eine Zeile ein Element auf oberster Ebene beginnt (Spalte 0).
    fn top_level_item(line: &str) -> bool {
        const ITEMS: &[&str] = &[
            "pub ",
            "pub(",
            "fn ",
            "async ",
            "unsafe ",
            "extern ",
            "impl ",
            "impl<",
            "struct ",
            "enum ",
            "union ",
            "const ",
            "static ",
            "mod ",
            "use ",
            "type ",
            "trait ",
            "#[",
            "#![",
            "macro_rules!",
        ];
        if ITEMS.iter().any(|item| line.starts_with(item)) {
            return true;
        }
        // Ein Makro-Aufruf auf oberster Ebene: `name! …`.
        let ident: String = line
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        !ident.is_empty()
            && line.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && line[ident.len()..].starts_with('!')
    }

    /// Ob ein Ausgabe-Pfad das Modul weiterreicht (`pub use …assurance…`).
    fn reexports_assurance(text: &str) -> Result<(), String> {
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            let reexport = code.starts_with("pub use")
                || (code.starts_with("pub(") && code.contains(") use "));
            if reexport && code.contains("assurance") {
                return Err(format!(
                    "{}: re-exports the assurance module: {line}",
                    number + 1
                ));
            }
        }
        Ok(())
    }

    /// Das Muster, mit dem eine Code-Zeile eines Schreibpfads eine
    /// gespeicherte Stufe verriete — [`STORED_CLAIMS`] oder ein Import des
    /// Moduls über `minds_reader` (auch als Gruppe in einer Zeile).
    /// Kommentarzeilen sind vorher aussortiert.
    ///
    /// Ein Muster, das mit einem Buchstaben beginnt (ein Feldname), zählt
    /// nur an einer Bezeichner-Grenze: `pub explained: bool` ist ein Fund,
    /// das Gate-Flag `require_explained:` (EA-02, EA-12) nicht.
    fn stored_claim(line: &str) -> Option<&'static str> {
        stored_claim_except(line, &[])
    }

    fn stored_claim_except(line: &str, allowed: &[&str]) -> Option<&'static str> {
        let code = line.to_lowercase();
        if code.contains("minds_reader") && code.contains("assurance") {
            return Some("minds_reader::…assurance");
        }
        let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
        STORED_CLAIMS.iter().copied().find(|claim| {
            !allowed.contains(claim)
                && code.match_indices(claim).any(|(at, _)| {
                    !claim.starts_with(|c: char| c.is_ascii_alphabetic())
                        || !code[..at].ends_with(identifier)
                })
        })
    }
}
