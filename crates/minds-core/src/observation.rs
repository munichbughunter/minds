//! Das Observation-Objekt des Datei-Beobachters (EA-08): was der Witness im
//! Worktree gesehen hat — repo-relative Pfade und blake3-Hashes, nie Inhalt.
//!
//! # Form
//!
//! Kanonisches JSON nach RFC 8785 ([`crate::canonical`]):
//!
//! ```text
//! {"first_at":…,"last_at":…,"observations":[{"at":…,"content":"b3-…"|null,
//!  "path":…,"reason":null|"…","seq":…}],"schema":2,"started_at":…,
//!  "witness":{"isolation":{"agent_uids":[…],"binary_protected":…,
//!  "home_private":…,"witness_uid":…},"profile":"…",
//!  "spec":null|{"envelopes":["b3-…"],"id":"b3-…"}}}
//! ```
//!
//! Alle Schlüssel stehen immer da, auch mit `null` — eine Form, ein Hash.
//! Nur `started_at` (ab Schema 2), `witness` (seit den Witness-Fakten) und
//! `gaps` (seit den Lücken des Beobachters; nur, wenn es welche gab)
//! fehlen in älteren Objekten ganz — ein zusätzliches Feld hebt das Schema
//! nicht (CHANGELOG); der Leser ist tolerant, die Id gilt den gespeicherten
//! Bytes.
//!
//! # Lücken des Beobachters (`gaps`)
//!
//! Wo der Beobachter Meldungen verlor — ein voller Kanal, ein Panic, nicht
//! lesbare Ignore-Regeln, kein Watcher —, steht im Stream des Witness ein
//! `fs.gap`-Event. Es ist gekettet wie jede Beobachtung; hier steht es
//! als [`ObservationGap`], damit der Leser die Lücke **sieht**: Ein Claim,
//! dessen Beobachtung in eine Lücke fiel, ist nicht „nie beobachtet",
//! sondern „der Beobachter war blind" (`observation gap (overflow: 12
//! events dropped)` statt `no file-system observation`). `dropped` zählt
//! die verlorenen Meldungen, wo der Beobachter sie zählen konnte (ein
//! voller Kanal); eine Lücke ohne Zahl ist eine Lücke unbekannter Größe.
//!
//! # Was der Witness über sich festhält (`witness`)
//!
//! `witness` sind Fakten, die der Witness beim Versiegeln **selbst
//! geprüft** hat ([`WitnessFacts`]): sein Profil, seine Trennung vom Agenten
//! (eigene Kennung gegen die Eigentümer von Worktree und `.git`, privates
//! Home, geschütztes Binary) und das Soll, das an HEAD galt, samt den
//! Umschlägen, die er im Store sah. Wie `started_at` sind sie über die
//! Objekt-Id im signierten Seal gebunden. Ob die Trennung als belegt gilt,
//! leitet erst der Leser ab ([`Isolation::proven`]) — gespeichert wird der
//! Befund, nicht das Urteil.
//!
//! # Beginn der Epoche (EA-08a)
//!
//! `started_at` ist der Zeitstempel des **ersten Events der Epoche** im
//! Stream des Witness (`witness.start` für die erste Epoche eines Laufs,
//! sonst das erste Event nach dem vorigen Seal) — gestempelt von der
//! monotonen Uhr des Witness, nie aus Agent-Eingaben. Weil die Objekt-Id in
//! der `session=`-Zeile des signierten Seals steht, ist auch `started_at`
//! signiert. Der Leser verankert damit den Anfang einer Epochen-Kette
//! (`previous = None`): Begann die Epoche vor dem Fenster einer Session,
//! ist die Kette vollständig.
//!
//! Schema 1 (ohne `started_at`) bleibt lesbar: Das Feld fehlt dort und wird
//! dann auch nicht serialisiert — die gespeicherten Bytes eines alten
//! Objekts ergeben neu serialisiert dieselben. Ein Schema-1-Kettenanfang
//! verankert nie.
//!
//! # Identität
//!
//! Wie bei Sessions: `blake3(kanonische Bytes)`, ohne eigene Hash-Domain
//! (W4: neue Domains nur, wo eine Spec sie nennt). Die Id steht in der
//! `session=`-Zeile des `witness-fs/v1`-Seals (Outcome
//! `observations_stored`).
//!
//! # Warum kein `forget`
//!
//! Das Objekt trägt nur Pfade (redigiert) und Hashes — für Secret-Dateien,
//! ignorierte Pfade und alles außerhalb des Repos nicht einmal die. Es gibt
//! nichts Tilgbares darin, das nicht schon im Commit selbst stünde.
//!
//! # Wer es baut
//!
//! Der Typ ist öffentlich konstruierbar, gespeichert wird aber nur die
//! redigierte Form (`minds_redact::RedactedObservations`), die ausschließlich
//! die Pipeline erzeugt — dieselbe Typ-Garantie wie bei Sessions.

use serde::{Deserialize, Serialize};

use crate::{CanonError, ContentHash};

/// Schema-Version des Observation-Objekts, die dieses Binary schreibt.
/// Schema 1 (EA-08) wird nur noch gelesen.
pub const OBSERVATIONS_SCHEMA: u32 = 2;

/// Größte Datei, die der Beobachter liest und hasht (16 MiB); größere
/// erscheinen mit [`ObservationReason::TooLarge`] ohne Hash.
pub const MAX_OBSERVED_BYTES: u64 = 16 * 1024 * 1024;

/// Die Beobachtungen einer versiegelten Epoche des Witness-Streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observations {
    /// [`OBSERVATIONS_SCHEMA`].
    pub schema: u32,
    /// Beginn der Epoche (RFC 3339): das erste Event der Epoche im Stream des
    /// Witness. Ab Schema 2 immer gesetzt; `None` nur in Schema-1-Objekten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Zeitstempel der ersten Beobachtung (RFC 3339), `None` ohne Beobachtung.
    pub first_at: Option<String>,
    /// Zeitstempel der letzten Beobachtung.
    pub last_at: Option<String>,
    /// Nach `seq` aufsteigend.
    pub observations: Vec<Observation>,
    /// Was der Witness beim Versiegeln über sich festhielt; `None` in
    /// älteren Objekten (und wo er nichts festhalten konnte).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<WitnessFacts>,
    /// Wo der Beobachter in dieser Epoche Meldungen verlor — die
    /// `fs.gap`-Events des Streams, nach `seq` aufsteigend. Leer (und nicht
    /// serialisiert), wenn er lückenlos sah.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<ObservationGap>,
}

/// Eine Lücke des Beobachters: `fs.gap { reason, dropped }` im Stream des
/// Witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationGap {
    /// Die Sequenznummer des `fs.gap`-Events in seiner Epoche.
    pub seq: u64,
    /// Wann der Witness die Lücke festhielt (RFC 3339) — die verlorenen
    /// Meldungen liegen davor, seit der vorigen Beobachtung.
    pub at: String,
    /// Warum der Beobachter blind war.
    pub reason: GapReason,
    /// Wie viele Meldungen verloren gingen, wo der Beobachter zählen konnte
    /// (ein voller Kanal); `None`: nicht zählbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<u64>,
}

/// Warum der Beobachter Meldungen verlor — das Wort aus dem `fs.gap`-Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    /// Der Kanal zwischen Watcher und Eventloop (oder die Sammlung
    /// wartender Pfade) lief über; `dropped` zählt, was der Kanal abwies.
    Overflow,
    /// Die Eventloop des Beobachters ist in Agent-bestimmten Bytes
    /// (Index, `.gitignore`) abgestürzt; der Durchlauf ging verloren.
    Panic,
    /// Ignore-Regeln ließen sich nicht sicher lesen; betroffene Pfade wurden
    /// verworfen (lieber blind als einen ignorierten Pfad fingerprinten).
    IgnoreRules,
    /// Kein Watcher (etwa das inotify-Limit): Der Witness lief blind.
    Unavailable,
    /// Ein Grund, den dieses Binary nicht kennt (tolerant gelesen; dieses
    /// Binary schreibt ihn nie).
    #[serde(other)]
    Unknown,
}

impl GapReason {
    /// Das Wire-Wort — dasselbe wie im `fs.gap`-Event.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::Panic => "panic",
            Self::IgnoreRules => "ignore_rules",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }

    /// Das Wort gelesen; alles andere ist [`GapReason::Unknown`].
    pub fn parse(word: &str) -> Self {
        match word {
            "overflow" => Self::Overflow,
            "panic" => Self::Panic,
            "ignore_rules" => Self::IgnoreRules,
            "unavailable" => Self::Unavailable,
            _ => Self::Unknown,
        }
    }
}

/// Was der Witness beim Versiegeln einer Epoche über sich prüfte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessFacts {
    /// Sein Profil aus `witness.json` (`user`, `local`, …) — als Wort
    /// gespeichert, damit ein künftiges Profil ein älteres Binary nicht am
    /// Lesen hindert ([`crate::evidence::WitnessProfile::parse`] gibt dann
    /// `None`).
    pub profile: String,
    /// Seine Trennung vom Agenten.
    pub isolation: Isolation,
    /// Das Soll an HEAD beim Versiegeln; `None` ohne (lesbares) Soll.
    /// Fehlt der Schlüssel (tolerant gelesen), ebenso `None`.
    #[serde(default)]
    pub spec: Option<SpecSeen>,
}

/// Die Trennung des Witness vom Agenten, wie er sie beim Versiegeln vorfand.
/// Der Agent läuft unter der Kennung, der Worktree und `.git` gehören.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Isolation {
    /// Die effektive Kennung des Witness.
    pub witness_uid: u32,
    /// Die Eigentümer von Worktree-Wurzel und `.git` — aufsteigend, ohne
    /// Doppelte.
    pub agent_uids: Vec<u32>,
    /// Home und Schlüssel gehören dem Witness, und niemand sonst kann sie
    /// lesen oder schreiben (außer dem Durchqueren zum Socket).
    pub home_private: bool,
    /// Das laufende Binary und jedes Verzeichnis darüber gehören root oder
    /// dem Witness und sind für niemanden sonst beschreibbar.
    pub binary_protected: bool,
}

impl Isolation {
    /// Ob die Trennung belegt ist: Die Kennung des Witness ist keine des
    /// Agenten, der Agent ist nicht root (der läse jedes Home und ersetzte
    /// jedes Binary), und Home wie Binary sind ihm entzogen.
    ///
    /// Das ist der Befund, nicht die Bürgschaft: Ob ihm zu trauen ist,
    /// entscheidet der Principal, unter dem der Seal gilt
    /// ([`crate::evidence::SEPARATED_WITNESS_PRINCIPAL`]).
    pub fn proven(&self) -> bool {
        !self.agent_uids.is_empty()
            && !self.agent_uids.contains(&self.witness_uid)
            && !self.agent_uids.contains(&0)
            && self.home_private
            && self.binary_protected
    }
}

/// Das Soll, das der Witness an HEAD vorfand, und die Umschläge dazu im
/// Store ([`crate::spec::envelope_id`]) — aufsteigend, ohne Doppelte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecSeen {
    /// Die Soll-Id (`derive_key("minds/spec/v1", kanonisch)`).
    pub id: ContentHash,
    /// Die Umschläge, die der Store unter dieser Id hielt — ungeprüft: Der
    /// Witness kennt die vertrauenswürdigen Signer nicht.
    pub envelopes: Vec<ContentHash>,
}

/// Eine beobachtete Änderung: `fs.observed { path, content, at }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Die Sequenznummer des `fs.observed`-Events in seiner Epoche.
    pub seq: u64,
    /// Beobachtungszeit des Witness (RFC 3339).
    pub at: String,
    /// Repo-relativ, `/`-getrennt — nie der absolute Host-Pfad.
    pub path: String,
    /// blake3 des Inhalts; `None` genau dann, wenn [`reason`](Self::reason)
    /// gesetzt ist.
    pub content: Option<ContentHash>,
    /// Warum kein Hash vorliegt.
    pub reason: Option<ObservationReason>,
}

/// Warum eine Beobachtung keinen Inhalts-Hash trägt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationReason {
    /// Die Datei ist größer als [`MAX_OBSERVED_BYTES`].
    TooLarge,
    /// Secret-Datei (`.env`, Schlüssel …): Es wird nie ein Hash gebildet.
    SecretFile,
    /// Der Pfad existiert nicht mehr — beobachtete Abwesenheit.
    Deleted,
    /// Ein Symlink, dessen Ziel außerhalb des Repos liegt, oder eine Datei
    /// mit einem zweiten harten Link (sie kann eine Host-Datei sein).
    OutsideRepo,
    /// Der Inhalt trägt etwas, das die Redaction-Pipeline ersetzen würde —
    /// kein Hash, der ein Wörterbuch-Orakel über ein Secret wäre (dieselbe
    /// Regel wie `Effect.written`).
    RedactedContent,
    /// Kein UTF-8: nicht scanbar, also auch kein Hash (wie `Effect.written`).
    Unscannable,
    /// Ein Grund, den dieses Binary nicht kennt (tolerant gelesen; dieses
    /// Binary schreibt ihn nie).
    #[serde(other)]
    Unknown,
}

impl ObservationReason {
    /// Das Wire-Wort.
    pub const fn word(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::SecretFile => "secret_file",
            Self::Deleted => "deleted",
            Self::OutsideRepo => "outside_repo",
            Self::RedactedContent => "redacted_content",
            Self::Unscannable => "unscannable",
            Self::Unknown => "unknown",
        }
    }
}

/// Warum ein Observation-Objekt nicht geschrieben werden darf. Nennt das
/// Problem, zitiert nie einen Pfad.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObservationError {
    #[error("unsupported observation schema")]
    Schema,
    #[error("observation {0}: path is not a plain repo-relative path")]
    Path(usize),
    #[error("observation {0}: content and reason must be exclusive")]
    ContentReason(usize),
    #[error("observation {0}: sequence numbers must increase")]
    Order(usize),
    #[error("observation window does not match the observations")]
    Window,
    #[error("observation epoch start is missing, unreadable or after the first observation")]
    Start,
    #[error("witness facts are not in canonical order or name an unknown profile")]
    Witness,
    #[error("gap {0}: unknown reason or sequence numbers not increasing")]
    Gap(usize),
}

impl Observations {
    /// Baut das Objekt einer Epoche, die um `started_at` begann, aus
    /// Beobachtungen in `seq`-Reihenfolge; das Fenster ergibt sich aus der
    /// ersten und letzten.
    pub fn new(started_at: impl Into<String>, observations: Vec<Observation>) -> Self {
        Self {
            schema: OBSERVATIONS_SCHEMA,
            started_at: Some(started_at.into()),
            first_at: observations.first().map(|o| o.at.clone()),
            last_at: observations.last().map(|o| o.at.clone()),
            observations,
            witness: None,
            gaps: Vec::new(),
        }
    }

    /// Dasselbe Objekt mit den Fakten des Witness.
    #[must_use]
    pub fn with_witness(mut self, witness: WitnessFacts) -> Self {
        self.witness = Some(witness);
        self
    }

    /// Dasselbe Objekt mit den Lücken des Beobachters (in `seq`-Reihenfolge).
    #[must_use]
    pub fn with_gaps(mut self, gaps: Vec<ObservationGap>) -> Self {
        self.gaps = gaps;
        self
    }

    /// Die Schreib-Regeln: Schema bekannt, Pfade schlicht repo-relativ,
    /// `content` xor `reason`, `seq` streng steigend, Fenster passend, der
    /// Beginn gesetzt, lesbar und nicht nach der ersten Beobachtung
    /// (verglichen werden geparste Zeitpunkte, nicht Zeichenketten).
    /// Der Leser ist tolerant; geschrieben wird nur, was hier besteht.
    pub fn check(&self) -> Result<(), ObservationError> {
        if self.schema != OBSERVATIONS_SCHEMA {
            return Err(ObservationError::Schema);
        }
        let started = self.started_at().ok_or(ObservationError::Start)?;
        let mut last = None;
        for (index, o) in self.observations.iter().enumerate() {
            if !plain_relative(&o.path) {
                return Err(ObservationError::Path(index));
            }
            let unknown = o.reason == Some(ObservationReason::Unknown);
            if o.content.is_some() == o.reason.is_some() || unknown {
                return Err(ObservationError::ContentReason(index));
            }
            if last.is_some_and(|seq| o.seq <= seq) {
                return Err(ObservationError::Order(index));
            }
            last = Some(o.seq);
        }
        if self.first_at.as_ref() != self.observations.first().map(|o| &o.at)
            || self.last_at.as_ref() != self.observations.last().map(|o| &o.at)
        {
            return Err(ObservationError::Window);
        }
        if let Some(first) = &self.first_at {
            let first: jiff::Timestamp = first.parse().map_err(|_| ObservationError::Window)?;
            if started > first {
                return Err(ObservationError::Start);
            }
        }
        if let Some(witness) = &self.witness {
            let ascending = |values: &[u32]| values.windows(2).all(|pair| pair[0] < pair[1]);
            let known = crate::evidence::WitnessProfile::parse(&witness.profile).is_some();
            let envelopes = witness.spec.as_ref().is_none_or(|spec| {
                spec.envelopes
                    .windows(2)
                    .all(|pair| pair[0].as_str() < pair[1].as_str())
            });
            if !known || !ascending(&witness.isolation.agent_uids) || !envelopes {
                return Err(ObservationError::Witness);
            }
        }
        let mut last = None;
        for (index, gap) in self.gaps.iter().enumerate() {
            if gap.reason == GapReason::Unknown || last.is_some_and(|seq| gap.seq <= seq) {
                return Err(ObservationError::Gap(index));
            }
            last = Some(gap.seq);
        }
        Ok(())
    }

    /// Der Beginn der Epoche als Zeitpunkt — `None` ohne `started_at`
    /// (Schema 1) oder wenn er nicht lesbar ist.
    pub fn started_at(&self) -> Option<jiff::Timestamp> {
        self.started_at.as_deref()?.parse().ok()
    }

    /// Die kanonischen Bytes (RFC 8785).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonError> {
        crate::to_canonical_json(self)
    }

    /// Die Identität: `blake3(kanonische Bytes)`.
    pub fn id(&self) -> Result<ContentHash, CanonError> {
        Ok(Self::id_of_bytes(&self.canonical_bytes()?))
    }

    /// Die Identität gespeicherter Bytes — der Leser prüft gegen sie, nicht
    /// gegen eine Neu-Serialisierung (Vorwärts-Toleranz wie bei Sessions).
    pub fn id_of_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }
}

/// Repo-relativ, `/`-getrennt, ohne leere, `.`- oder `..`-Komponenten, ohne
/// Backslash und Steuerzeichen.
pub fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Observations {
        Observations::new(
            "2026-10-05T09:59:58.125Z",
            vec![
                Observation {
                    seq: 1,
                    at: "2026-10-05T10:00:00.250Z".into(),
                    path: "src/sort/merge.rs".into(),
                    content: Some(ContentHash::from_bytes(
                        *blake3::hash(b"fn merge() {}\n").as_bytes(),
                    )),
                    reason: None,
                },
                Observation {
                    seq: 2,
                    at: "2026-10-05T10:00:01Z".into(),
                    path: ".env".into(),
                    content: None,
                    reason: Some(ObservationReason::SecretFile),
                },
                Observation {
                    seq: 4,
                    at: "2026-10-05T10:00:02Z".into(),
                    path: "old.txt".into(),
                    content: None,
                    reason: Some(ObservationReason::Deleted),
                },
            ],
        )
    }

    /// Die eingefrorene kanonische Form eines Schema-1-Objekts (EA-08), wie
    /// sie in bestehenden Repositories liegt.
    const SCHEMA1: &str = concat!(
        r#"{"first_at":"2026-10-05T10:00:00.250Z","last_at":"2026-10-05T10:00:02Z","#,
        r#""observations":[{"at":"2026-10-05T10:00:00.250Z","#,
        r#""content":"b3-77d93191302c0026c78d4347b6b5b6ecff6225060cb3128e487a849f474f0857","path":"src/sort/merge.rs","reason":null,"seq":1},"#,
        r#"{"at":"2026-10-05T10:00:01Z","content":null,"path":".env","reason":"secret_file","seq":2},"#,
        r#"{"at":"2026-10-05T10:00:02Z","content":null,"path":"old.txt","reason":"deleted","seq":4}],"#,
        r#""schema":1}"#
    );

    /// Die Fakten eines getrennten `user`-Witness mit Soll.
    fn facts() -> WitnessFacts {
        WitnessFacts {
            profile: "user".into(),
            isolation: Isolation {
                witness_uid: 498,
                agent_uids: vec![501],
                home_private: true,
                binary_protected: true,
            },
            spec: Some(SpecSeen {
                id: ContentHash::from_bytes([0x5a; 32]),
                envelopes: vec![
                    ContentHash::from_bytes([0x01; 32]),
                    ContentHash::from_bytes([0x02; 32]),
                ],
            }),
        }
    }

    #[test]
    fn observation_object_with_witness_facts_golden() {
        let object = sample().with_witness(facts());
        object.check().unwrap();
        let text = String::from_utf8(object.canonical_bytes().unwrap()).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"first_at":"2026-10-05T10:00:00.250Z","last_at":"2026-10-05T10:00:02Z","#,
                r#""observations":[{"at":"2026-10-05T10:00:00.250Z","#,
                r#""content":"b3-77d93191302c0026c78d4347b6b5b6ecff6225060cb3128e487a849f474f0857","path":"src/sort/merge.rs","reason":null,"seq":1},"#,
                r#"{"at":"2026-10-05T10:00:01Z","content":null,"path":".env","reason":"secret_file","seq":2},"#,
                r#"{"at":"2026-10-05T10:00:02Z","content":null,"path":"old.txt","reason":"deleted","seq":4}],"#,
                r#""schema":2,"started_at":"2026-10-05T09:59:58.125Z","#,
                r#""witness":{"isolation":{"agent_uids":[501],"binary_protected":true,"#,
                r#""home_private":true,"witness_uid":498},"profile":"user","#,
                r#""spec":{"envelopes":["b3-0101010101010101010101010101010101010101010101010101010101010101","#,
                r#""b3-0202020202020202020202020202020202020202020202020202020202020202"],"#,
                r#""id":"b3-5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a"}}}"#
            )
        );
        let back: Observations = serde_json::from_str(&text).unwrap();
        assert_eq!(back, object);
        assert_eq!(
            object.id().unwrap().as_str(),
            "b3-d0f51774c80f3ed56ba74b8adf0efaf9ad8e8463615d8f7c812dc4b7aab9fced"
        );
    }

    /// Die Trennung gilt nur, wenn die Kennung des Witness keine des Agenten
    /// ist und Home wie Binary ihm entzogen sind.
    #[test]
    fn isolation_is_proven_only_when_everything_holds() {
        let proven = facts().isolation;
        assert!(proven.proven());
        for broken in [
            Isolation {
                agent_uids: vec![498, 501],
                ..proven.clone()
            },
            Isolation {
                agent_uids: vec![],
                ..proven.clone()
            },
            Isolation {
                agent_uids: vec![0],
                ..proven.clone()
            },
            Isolation {
                home_private: false,
                ..proven.clone()
            },
            Isolation {
                binary_protected: false,
                ..proven.clone()
            },
        ] {
            assert!(!broken.proven(), "{broken:?}");
        }
    }

    /// Geschrieben werden nur Fakten in kanonischer Ordnung mit bekanntem
    /// Profil.
    #[test]
    fn witness_facts_are_checked_before_writing() {
        let mut unknown = facts();
        unknown.profile = "sandbox".into();
        let mut unsorted = facts();
        unsorted.isolation.agent_uids = vec![501, 20];
        let mut doubled = facts();
        doubled.spec.as_mut().unwrap().envelopes.reverse();
        for witness in [unknown, unsorted, doubled] {
            assert_eq!(
                sample().with_witness(witness).check(),
                Err(ObservationError::Witness)
            );
        }
    }

    #[test]
    fn observation_object_without_witness_facts_golden() {
        let object = sample();
        object.check().unwrap();
        let text = String::from_utf8(object.canonical_bytes().unwrap()).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"first_at":"2026-10-05T10:00:00.250Z","last_at":"2026-10-05T10:00:02Z","#,
                r#""observations":[{"at":"2026-10-05T10:00:00.250Z","#,
                r#""content":"b3-77d93191302c0026c78d4347b6b5b6ecff6225060cb3128e487a849f474f0857","path":"src/sort/merge.rs","reason":null,"seq":1},"#,
                r#"{"at":"2026-10-05T10:00:01Z","content":null,"path":".env","reason":"secret_file","seq":2},"#,
                r#"{"at":"2026-10-05T10:00:02Z","content":null,"path":"old.txt","reason":"deleted","seq":4}],"#,
                r#""schema":2,"started_at":"2026-10-05T09:59:58.125Z"}"#
            )
        );
        // Ohne Fakten dieselben Bytes und Id wie vor ihnen — kein
        // `witness: null`.
        assert_eq!(
            object.id().unwrap().as_str(),
            "b3-3d7c5cd8fbc5e3ff126d5bb75a218f6750e924da8ec2c2f0761e389d3586a26f"
        );
    }

    #[test]
    fn observation_object_schema1_still_reads() {
        // Gelesen wird gegen die gespeicherten Bytes; die Id bleibt die alte.
        assert_eq!(
            Observations::id_of_bytes(SCHEMA1.as_bytes()).as_str(),
            "b3-f5817f8e867da041c4993e0efb23b7c1ce4bc318f18312c08d2f323b3aca81f0"
        );
        let old: Observations = serde_json::from_str(SCHEMA1).unwrap();
        assert_eq!(old.schema, 1);
        assert_eq!(old.started_at, None);
        assert_eq!(old.started_at(), None);
        assert_eq!(old.observations, sample().observations);
        // Neu serialisiert: dieselben Bytes — kein `started_at: null`.
        assert_eq!(old.canonical_bytes().unwrap(), SCHEMA1.as_bytes());
        // Geschrieben wird Schema 1 nicht mehr.
        assert_eq!(old.check(), Err(ObservationError::Schema));
    }

    #[test]
    fn the_epoch_start_is_required_readable_and_not_after_the_first_observation() {
        let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
        assert_eq!(sample().started_at(), Some(at("2026-10-05T09:59:58.125Z")));
        // Gleichstand ist erlaubt; der Vergleich läuft über Zeitpunkte, nicht
        // über Zeichenketten (".250Z" < "Z" wäre lexikalisch falsch herum).
        for ok in ["2026-10-05T10:00:00.250Z", "2026-10-05T10:00:00Z"] {
            let mut object = sample();
            object.started_at = Some(ok.into());
            assert_eq!(object.check(), Ok(()), "{ok}");
        }
        for bad in [
            None,
            Some("2026-10-05T10:00:00.251Z"),
            Some("2026-10-05T10:00:01Z"),
            Some("gestern"),
            Some(""),
        ] {
            let mut object = sample();
            object.started_at = bad.map(str::to_owned);
            assert_eq!(object.check(), Err(ObservationError::Start), "{bad:?}");
        }
        // Ein unlesbares `first_at` ist ein Fehler des Fensters.
        let mut object = sample();
        object.first_at = Some("kein datum".into());
        object.observations[0].at = "kein datum".into();
        assert_eq!(object.check(), Err(ObservationError::Window));
        // Ohne Beobachtung zählt nur, dass der Beginn lesbar ist.
        assert!(
            Observations::new("2026-10-05T10:00:00Z", Vec::new())
                .check()
                .is_ok()
        );
        assert_eq!(
            Observations::new("kein datum", Vec::new()).check(),
            Err(ObservationError::Start)
        );
    }

    #[test]
    fn observation_object_roundtrips_and_reads_tolerantly() {
        let object = sample();
        let bytes = object.canonical_bytes().unwrap();
        let back: Observations = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, object);
        assert_eq!(Observations::id_of_bytes(&bytes), object.id().unwrap());
        // Ein künftiger Grund und ein unbekanntes Feld brechen den Leser nicht.
        let future = String::from_utf8(bytes)
            .unwrap()
            .replace("\"deleted\"", "\"renamed\"")
            .replace("\"schema\":2", "\"schema\":2,\"extra\":true");
        let read: Observations = serde_json::from_str(&future).unwrap();
        assert_eq!(
            read.observations[2].reason,
            Some(ObservationReason::Unknown)
        );
        // … geschrieben wird so etwas aber nie.
        assert_eq!(read.check(), Err(ObservationError::ContentReason(2)));
    }

    /// Lücken des Beobachters stehen im Objekt — und fehlen ganz, wo es
    /// keine gab: Ein Objekt ohne Lücken serialisiert byte-identisch wie
    /// vor dem Feld (dieselbe Id).
    #[test]
    fn observer_gaps_are_carried_and_absent_when_there_were_none() {
        let plain = sample();
        let bytes = plain.canonical_bytes().unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("gaps"));
        let gapped = sample().with_gaps(vec![
            ObservationGap {
                seq: 3,
                at: "2026-10-05T10:00:01.500Z".into(),
                reason: GapReason::Overflow,
                dropped: Some(12),
            },
            ObservationGap {
                seq: 5,
                at: "2026-10-05T10:00:03Z".into(),
                reason: GapReason::Panic,
                dropped: None,
            },
        ]);
        gapped.check().unwrap();
        let bytes = gapped.canonical_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(
            text.contains(
                r#""gaps":[{"at":"2026-10-05T10:00:01.500Z","dropped":12,"reason":"overflow","seq":3},{"at":"2026-10-05T10:00:03Z","reason":"panic","seq":5}]"#
            ),
            "{text}"
        );
        let back: Observations = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, gapped);
        assert_ne!(back.id().unwrap(), plain.id().unwrap());
        // Ein künftiger Grund liest sich tolerant — und wird nie geschrieben.
        let future: Observations =
            serde_json::from_str(&text.replace("\"panic\"", "\"eclipse\"")).unwrap();
        assert_eq!(future.gaps[1].reason, GapReason::Unknown);
        assert_eq!(future.check(), Err(ObservationError::Gap(1)));
        let mut order = gapped.clone();
        order.gaps[1].seq = 3;
        assert_eq!(order.check(), Err(ObservationError::Gap(1)));
        for reason in [
            GapReason::Overflow,
            GapReason::Panic,
            GapReason::IgnoreRules,
            GapReason::Unavailable,
        ] {
            assert_eq!(GapReason::parse(reason.word()), reason);
        }
        assert_eq!(GapReason::parse("eclipse"), GapReason::Unknown);
    }

    #[test]
    fn check_refuses_what_must_not_be_written() {
        let mut absolute = sample();
        absolute.observations[0].path = "/home/dev/repo/a.rs".into();
        assert_eq!(absolute.check(), Err(ObservationError::Path(0)));
        for path in ["", "a/../b", "./a", "a//b", "a\\b", "a\nb"] {
            let mut bad = sample();
            bad.observations[0].path = path.into();
            assert_eq!(bad.check(), Err(ObservationError::Path(0)), "{path:?}");
        }
        let mut both = sample();
        both.observations[1].content = both.observations[0].content.clone();
        assert_eq!(both.check(), Err(ObservationError::ContentReason(1)));
        let mut neither = sample();
        neither.observations[0].content = None;
        assert_eq!(neither.check(), Err(ObservationError::ContentReason(0)));
        let mut order = sample();
        order.observations[2].seq = 1;
        assert_eq!(order.check(), Err(ObservationError::Order(2)));
        let mut window = sample();
        window.last_at = None;
        assert_eq!(window.check(), Err(ObservationError::Window));
        for version in [1, 3] {
            let mut schema = sample();
            schema.schema = version;
            assert_eq!(schema.check(), Err(ObservationError::Schema));
        }
        assert!(
            Observations::new("2026-10-05T10:00:00Z", Vec::new())
                .check()
                .is_ok()
        );
    }
}
