//! Die Entscheidung als Record: der gewählte Weg, die verworfenen
//! Alternativen — und die **Annahmen**, unter denen die Begründung gilt.
//!
//! # Warum nicht nur „verworfen, weil"
//!
//! Dass eine Alternative verworfen wurde, steht heute in `intent.discarded`
//! — eine Zeile Text, deterministisch aus gelöschten Dateien gewonnen. Das
//! sagt, *dass* etwas nicht gemacht wurde, nicht *warum*, und schon gar
//! nicht, *wie lange* das Warum trägt. Eine Begründung gilt unter
//! Bedingungen: „Unix-Socket verworfen, weil macOS Socket-fds beim `fork`
//! vererbt." Fällt die Bedingung — der Daemon forkt nicht mehr —, ist die
//! Alternative wieder offen. Genau das soll ein späterer Agent sehen, ohne
//! die alte Session zu lesen.
//!
//! # Das Gewicht sitzt auf der Kante
//!
//! Eine Annahme ist nicht an sich tragend, sondern tragend *für eine
//! bestimmte Verwerfung*. Dieselbe Annahme kann die Verwerfung von B tragen
//! und bei C nur am Rand mitspielen. Deshalb hängt [`Weight`] an der
//! [`Reason`] — der Kante zwischen Alternative und Annahme —, nicht an der
//! [`Assumption`]. Fällt eine tragende Annahme hinter der Verwerfung von B,
//! kommt genau B zurück auf den Tisch, nicht alles.
//!
//! # Die Probe macht die Annahme prüfbar
//!
//! Eine [`Assumption`] kann eine [`Probe`] tragen: eine deterministische
//! Prüfung gegen den Worktree, ohne Modell und ohne Tokens. Drei Arten —
//! ein Muster in einer Datei oder einem Glob, ein Pfad, der (nicht)
//! existiert, eine Datei, die seit der Entscheidung unverändert ist. Keine
//! Kommandos: Ausführung fremden Inhalts ist keine Probe, sondern ein
//! Angriffsweg. Eine Annahme ohne Probe ist erlaubt und heißt später „nicht
//! prüfbar" — nie „gefallen".
//!
//! # Zwei Zustände, ein Record
//!
//! Der Agent entscheidet selbst (`decided_by: agent`, `chosen` belegt) —
//! oder er legt die Wahl dem Menschen vor (`open: true`, kein `chosen`, die
//! Optionen samt Annahmen stehen in `alternatives`). Der Mensch entscheidet
//! später mit einem neuen Record, der per `supersedes` auf den offenen
//! zeigt. So ist auch die Übergabe Teil der Historie, nicht nur ihr
//! Ergebnis. `decided_by` ist dabei eine Angabe des Schreibers, kein Beleg:
//! Wer `refs/minds/` schreibt, kann auch `human` eintragen. Belegt wird der
//! Urheber erst, wenn eine Session den Record nennt (A1/A2) oder ein
//! Mensch ihn signiert — beides rechnet der Reader, nie der Record.
//!
//! # Form, Identität und der Schreibweg
//!
//! Kanonisches JSON nach RFC 8785; die Id ist `blake3(kanonische Bytes)`
//! wie beim Observation- und Replay-Record — keine neue Hash-Domäne (W4).
//! Gelesen wird tolerant (unbekannte Felder ignoriert), geschrieben
//! kanonisch; was [`DecisionRecord::check`] ablehnt, legt der Store nie ab.
//!
//! Der Record trägt Freitext (Thema, Aussagen, Ansätze) und die Muster der
//! Proben. Beides geht vor dem Ablegen durch die Redaction — und die Id
//! entsteht **danach**, über genau die Bytes, die abgelegt werden:
//! `check → redact → id → store`. Eine Id über den Klartext wäre ein
//! Wörterbuch-Orakel für das Entfernte und aus dem Store nicht
//! nachrechenbar. Die Id, die eine Session als [`DecisionEvent`] nennt,
//! stammt deshalb aus derselben Stelle, die den Record ablegt (`minds
//! decide` druckt sie), nie aus einer eigenen Rechnung des Agenten.
//!
//! # Was hier **nicht** steht
//!
//! Ob eine Annahme heute noch gilt, ob eine Alternative zurück auf den
//! Tisch gehört, welcher Record die Spitze seiner Kette ist — das rechnet
//! `minds-reader` zur Lesezeit (W2/W5). Der Record trägt Aussagen des
//! Entscheiders, nie deren Bewertung.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{CanonError, ContentHash};

/// Die Schema-Version des Records.
pub const DECISION_SCHEMA: u32 = 1;

/// Die Art des Dokuments — ein Record ohne zählt nirgends (wie beim
/// Replay-Record: ein anders gemeintes, tolerant gelesenes JSON soll nie
/// als Entscheidung durchgehen).
pub const DECISION_KIND: &str = "decision";

/// Höchstgröße eines Records in Bytes — so viel liest der Store
/// (`refs/minds/decisions/`, Folge-Commit) zurück und so viel nimmt
/// `minds decide` an.
pub const MAX_RECORD: usize = 256 * 1024;

/// Höchstzahl an Annahmen bzw. Alternativen je Record. Begrenzt, was der
/// Reader je Entscheidung prüft: Jede Probe kostet Dateizugriffe.
pub const MAX_ITEMS: usize = 64;

/// Höchstlänge in Bytes für Thema, Aussage, Ansatz und ein Probenmuster.
pub const MAX_TEXT: usize = 2_000;

/// Höchstlänge in Bytes für einen Glob (Probe oder Scope). Der Abgleich
/// kostet je Pfad bis zu einem Budget; ein langer Glob kann es an jedem
/// Pfad ausschöpfen.
pub const MAX_GLOB: usize = 512;

/// Höchstlänge in Bytes für die Kennung einer Annahme.
pub const MAX_ID: usize = 64;

/// Höchstlänge in Bytes für einen Pfad (`PATH_MAX` der üblichen Systeme) —
/// ein längerer kann in keiner Dateiliste stehen.
pub const MAX_PATH_BYTES: usize = 4096;

/// Wie schwer eine Annahme für eine Verwerfung (oder den gewählten Weg)
/// wiegt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Weight {
    /// Fällt sie, fällt die Begründung: Die Alternative gehört neu bewertet.
    LoadBearing,
    /// Fällt sie, bleibt die Begründung stehen — ein Vermerk in der
    /// Historie genügt.
    Marginal,
}

impl Weight {
    /// Das Wort in der Anzeige.
    pub fn as_str(&self) -> &'static str {
        match self {
            Weight::LoadBearing => "load-bearing",
            Weight::Marginal => "marginal",
        }
    }
}

/// Wer entschieden hat — oder, bei einem offenen Record, wer die Wahl
/// vorlegt. Eine Angabe, kein Beleg (siehe Modul-Doku).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecidedBy {
    /// Der Agent, in seiner Session.
    Agent,
    /// Ein Mensch, am Terminal.
    Human,
}

impl DecidedBy {
    /// Das Wort in der Anzeige.
    pub fn as_str(&self) -> &'static str {
        match self {
            DecidedBy::Agent => "agent",
            DecidedBy::Human => "human",
        }
    }
}

/// Eine deterministische Prüfung gegen den Worktree.
///
/// Pfade sind repo-relativ mit `/` als Trenner ([`is_repo_relative`]). Der
/// Reader wertet aus; hier steht nur, was eine Probe ist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Probe {
    /// Ein regulärer Ausdruck muss (nicht) in einer Datei oder in einer
    /// Datei eines Globs vorkommen. Genau eines von `path`/`glob`,
    /// mindestens eines von `must_match`/`must_not_match`.
    Pattern {
        /// Eine Datei.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Oder ein Glob über die Dateien des Repositories.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob: Option<String>,
        /// Muss vorkommen (bei einem Glob: in mindestens einer Datei).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        must_match: Option<String>,
        /// Darf nicht vorkommen (bei einem Glob: in keiner Datei).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        must_not_match: Option<String>,
    },
    /// Ein Pfad ist im Repository getrackt (er oder etwas darunter steht
    /// in der Dateiliste) — oder nicht. Ignorierte und ungetrackte Dateien
    /// gelten als nicht vorhanden: Der Reader sieht nur die Liste.
    Path {
        /// Der Pfad.
        path: String,
        /// `true`: Die Annahme ist, dass er da ist; `false`: dass er fehlt.
        exists: bool,
    },
    /// Eine Datei ist seit der Entscheidung unverändert: ihre Git-Blob-Id
    /// (wie `git hash-object --no-filters`) ist noch dieselbe. Eine
    /// Änderung heißt nicht, dass die Annahme gefallen ist — nur, dass sie
    /// neu anzusehen ist.
    Blob {
        /// Der Pfad.
        path: String,
        /// Die Blob-Id zur Zeit der Entscheidung (40 oder 64 Hex, klein).
        blob: String,
    },
}

/// Eine Annahme: eine Aussage über die Umstände, optional prüfbar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumption {
    /// Kurze Kennung, innerhalb des Records eindeutig: bis [`MAX_ID`]
    /// Zeichen aus `A-Z a-z 0-9 _ -` (`a1`, `no-windows`).
    pub id: String,
    /// Die Aussage in einem Satz.
    pub statement: String,
    /// Die Probe — fehlt sie, ist die Annahme nicht prüfbar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// Die Kante: Dieser Weg ruht auf dieser Annahme (oder wurde ihretwegen
/// verworfen), mit diesem Gewicht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// Die Kennung der Annahme ([`Assumption::id`]).
    pub assumption: String,
    /// Wie schwer sie hier wiegt.
    pub weight: Weight,
}

/// Der gewählte Weg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// Der Ansatz in einem Satz.
    pub approach: String,
    /// Die Annahmen, auf denen er ruht.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rests_on: Vec<Reason>,
}

/// Eine verworfene Alternative — oder, bei einem offenen Record, eine
/// Option, die dem Menschen vorliegt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alternative {
    /// Der Ansatz in einem Satz.
    pub approach: String,
    /// Warum er verworfen wurde: je Annahme eine Kante mit Gewicht. Bei
    /// einer Option eines offenen Records leer — [`check`](DecisionRecord::check)
    /// erzwingt das.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_because: Vec<Reason>,
    /// Die Annahmen, unter denen dieser Ansatz tragfähig wäre — bei einer
    /// Option das, was der Mensch wissen muss. Der Reader vermerkt, wenn
    /// eine davon fällt; eine Neubewertung löst nur `rejected_because` aus.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rests_on: Vec<Reason>,
}

/// Eine Entscheidung — getroffen oder dem Menschen vorgelegt.
///
/// Gelesen wird tolerant, geschrieben kanonisch. Ob der Record in sich
/// stimmig ist, sagt [`check`](Self::check); der Store legt nur ab, was
/// die Prüfung besteht, und der Reader wertet nur aus, was sie besteht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// [`DECISION_KIND`].
    #[serde(default)]
    pub kind: String,
    /// [`DECISION_SCHEMA`].
    pub schema: u32,
    /// Worum es geht, in einem Satz („lock for the witness socket on macOS").
    pub topic: String,
    /// Wer entschieden hat bzw. die Wahl vorlegt.
    pub decided_by: DecidedBy,
    /// Wann, RFC 3339 — `minds decide` schreibt UTC (`Z`); gelesen wird
    /// jeder Offset.
    pub at: String,
    /// Der gewählte Weg — fehlt genau dann, wenn der Record offen ist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<Choice>,
    /// Die Wahl liegt beim Menschen; `alternatives` sind seine Optionen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// Verworfene Alternativen bzw. die offenen Optionen.
    #[serde(default)]
    pub alternatives: Vec<Alternative>,
    /// Die Annahmen, auf die sich Wege und Verwerfungen beziehen.
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
    /// Pfad-Globs, die die Entscheidung betrifft. Leer: das ganze
    /// Repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    /// Der Record, den dieser ablöst — die Kette der Neubewertungen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<ContentHash>,
}

/// Warum ein Record nicht in sich stimmig ist. Nennt Feld und **Position**
/// (`assumptions[2]`, `alternatives[0].rejected_because`, Kante 1), nie
/// einen Wert: Auch eine Kennung, die ihre Form hat, kann ein Token sein
/// — und die Meldung landet in der Tool-Antwort des Agenten.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionError {
    /// `kind` ist nicht [`DECISION_KIND`].
    #[error("not a decision record (kind)")]
    Kind,
    /// Eine Schema-Version, die dieses Binary nicht kennt.
    #[error("unsupported decision schema {0}")]
    Schema(u32),
    /// Ein Pflichttext ist leer oder länger als [`MAX_TEXT`].
    #[error("decision field {0} is empty or too long")]
    Text(&'static str),
    /// `at` ist kein RFC-3339-Zeitstempel.
    #[error("decision field at is not an RFC 3339 timestamp")]
    At,
    /// Offen und entschieden zugleich — oder keines von beiden.
    #[error("a decision is either open (no chosen path) or has a chosen path")]
    OpenOrChosen,
    /// Ein offener Record ohne Optionen.
    #[error("an open decision lists at least one option")]
    NoOptions,
    /// Eine Option eines offenen Records trägt Verwerfungsgründe.
    #[error("an option of an open decision has no rejected_because")]
    OpenWithRejection,
    /// Mehr als [`MAX_ITEMS`] Annahmen oder Alternativen.
    #[error("a decision has at most {MAX_ITEMS} {0}")]
    TooMany(&'static str),
    /// Die Kennung von `assumptions[i]` ist leer, zu lang, trägt andere
    /// Zeichen als `A-Z a-z 0-9 _ -` oder kommt schon vor.
    #[error(
        "the id of assumptions[{0}] is empty, too long, has other characters than A-Z a-z 0-9 _ -, or is not unique"
    )]
    AssumptionId(usize),
    /// Die Kante `{1}` in der Liste `{0}` nennt eine Annahme, die es nicht
    /// gibt.
    #[error("reason {1} in {0} refers to an assumption id that is not declared")]
    UnknownAssumption(String, usize),
    /// Die Kante `{1}` in der Liste `{0}` nennt eine Annahme ein zweites
    /// Mal.
    #[error("reason {1} in {0} names an assumption that the list already names")]
    DuplicateReason(String, usize),
    /// Die Probe von `assumptions[i]` ist unvollständig oder widersprüchlich.
    #[error("the probe of assumptions[{0}] is invalid: {1}")]
    Probe(usize, &'static str),
    /// Ein Scope-Glob ist leer, zu lang oder verlässt das Repository.
    #[error("scope globs are non-empty, at most {MAX_GLOB} bytes and repo-relative")]
    Scope,
}

impl DecisionRecord {
    /// Die kanonischen Bytes (RFC 8785).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonError> {
        crate::to_canonical_json(self)
    }

    /// Die Identität: `blake3(kanonische Bytes)` — über den **redigierten**
    /// Record, siehe Modul-Doku („der Schreibweg").
    pub fn id(&self) -> Result<ContentHash, CanonError> {
        Ok(Self::id_of_bytes(&self.canonical_bytes()?))
    }

    /// Die Identität gespeicherter Bytes — der Leser prüft gegen sie, nicht
    /// gegen eine Neu-Serialisierung (Vorwärts-Toleranz wie bei Sessions).
    pub fn id_of_bytes(bytes: &[u8]) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }

    /// Die Annahme zu einer Kennung.
    pub fn assumption(&self, id: &str) -> Option<&Assumption> {
        self.assumptions.iter().find(|a| a.id == id)
    }

    /// Prüft, ob der Record in sich stimmig ist: Art und Schema bekannt,
    /// Texte belegt und begrenzt, offen *oder* entschieden, jede Kante
    /// zeigt genau einmal auf eine vorhandene Annahme, jede Probe ist
    /// vollständig, jeder Pfad und jeder Glob bleibt im Repository.
    pub fn check(&self) -> Result<(), DecisionError> {
        if self.kind != DECISION_KIND {
            return Err(DecisionError::Kind);
        }
        if self.schema != DECISION_SCHEMA {
            return Err(DecisionError::Schema(self.schema));
        }
        text(&self.topic, "topic")?;
        if self.at.parse::<jiff::Timestamp>().is_err() {
            return Err(DecisionError::At);
        }
        if self.open == self.chosen.is_some() {
            return Err(DecisionError::OpenOrChosen);
        }
        if self.open && self.alternatives.is_empty() {
            return Err(DecisionError::NoOptions);
        }
        if self.assumptions.len() > MAX_ITEMS {
            return Err(DecisionError::TooMany("assumptions"));
        }
        if self.alternatives.len() > MAX_ITEMS {
            return Err(DecisionError::TooMany("alternatives"));
        }
        if self.scope.len() > MAX_ITEMS {
            return Err(DecisionError::TooMany("scope globs"));
        }

        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for (i, assumption) in self.assumptions.iter().enumerate() {
            let id = assumption.id.as_str();
            if !is_id(id) || !ids.insert(id) {
                return Err(DecisionError::AssumptionId(i));
            }
            text(&assumption.statement, "assumption.statement")?;
            if let Some(probe) = &assumption.probe {
                check_probe(probe).map_err(|why| DecisionError::Probe(i, why))?;
            }
        }

        let reasons = |list: &[Reason], name: &str| -> Result<(), DecisionError> {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for (i, reason) in list.iter().enumerate() {
                let id = reason.assumption.as_str();
                if !ids.contains(id) {
                    return Err(DecisionError::UnknownAssumption(name.to_owned(), i));
                }
                if !seen.insert(id) {
                    return Err(DecisionError::DuplicateReason(name.to_owned(), i));
                }
            }
            Ok(())
        };
        if let Some(chosen) = &self.chosen {
            text(&chosen.approach, "chosen.approach")?;
            reasons(&chosen.rests_on, "chosen.rests_on")?;
        }
        for (i, alternative) in self.alternatives.iter().enumerate() {
            text(&alternative.approach, "alternative.approach")?;
            if self.open && !alternative.rejected_because.is_empty() {
                return Err(DecisionError::OpenWithRejection);
            }
            reasons(
                &alternative.rejected_because,
                &format!("alternatives[{i}].rejected_because"),
            )?;
            reasons(
                &alternative.rests_on,
                &format!("alternatives[{i}].rests_on"),
            )?;
        }
        for glob in &self.scope {
            if !is_glob(glob) {
                return Err(DecisionError::Scope);
            }
        }
        Ok(())
    }
}

/// Belegt und höchstens [`MAX_TEXT`] Bytes lang.
fn text(value: &str, field: &'static str) -> Result<(), DecisionError> {
    if value.trim().is_empty() || value.len() > MAX_TEXT {
        return Err(DecisionError::Text(field));
    }
    Ok(())
}

/// Eine Kennung: 1 bis [`MAX_ID`] Zeichen aus `A-Z a-z 0-9 _ -`.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Ein Glob, der im Repository bleibt: belegt, höchstens [`MAX_GLOB`]
/// Bytes, kein Steuerzeichen, kein `\`, kein `:` (Laufwerk, NTFS-Stream),
/// kein `..`-Segment, kein `//`-Anfang (UNC), keine Negation (`!…` — ein
/// Scope nennt, was dazugehört, wie der Intent-Anker). Ein Glob wird nur gegen die
/// Dateiliste abgeglichen, nie geöffnet — die Regeln halten trotzdem beide
/// Schreibweisen gleich streng.
pub fn is_glob(glob: &str) -> bool {
    let body = glob
        .strip_prefix("./")
        .or_else(|| glob.strip_prefix('/'))
        .unwrap_or(glob);
    !body.trim().is_empty()
        && glob.len() <= MAX_GLOB
        && !glob.starts_with('!')
        && !glob.contains('\\')
        && !glob.contains(':')
        && !glob.chars().any(is_control_or_format)
        && !body.starts_with('/')
        && !body
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
}

/// Ein repo-relativer Pfad: nicht leer, nicht absolut, kein `\`, kein
/// Steuerzeichen, kein `:` (Laufwerk `C:`, NTFS-Stream `x:$DATA`), kein
/// leeres und kein `..`-Segment, kein `.git`-Segment — auch nicht in den
/// Schreibweisen, die Windows darauf abbildet: `.git.`/`.git ` (Punkte und
/// Leerzeichen am Ende fallen weg) und der 8.3-Kurzname `GIT~1`. Auch kein
/// `.`-Segment: Die Dateiliste des Repositories kennt nur die kanonische
/// Schreibweise, und `./a` fände `a` dort nicht. Im `.git`-Verzeichnis
/// liegen Konfiguration und Zugangsdaten-Helfer; der Worktree-Vertrag des
/// Readers liest ohnehin nur, was die Dateiliste nennt.
pub fn is_repo_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.chars().any(is_control_or_format)
        && path.split('/').all(is_plain_segment)
}

/// Ein Segment, das weder leer ist noch `.git` meint — in keiner der
/// Schreibweisen, die ein Dateisystem darauf abbildet. Gemeinsam für
/// Record-Pfade (hier) und die Dateiliste des Readers.
///
/// Windows streicht Punkte und Leerzeichen am Ende eines Segments: `.`,
/// `..`, `...`, `.. ` werden zu nichts — und `.git.` zu `.git`; `GIT~1`
/// ist der 8.3-Kurzname. HFS+ und APFS ignorieren Unicode-Formatzeichen
/// (`.g\u{200c}it` ist `.git`), die [`is_control_or_format`] vorher
/// ausschließt.
pub fn is_plain_segment(segment: &str) -> bool {
    let bare = segment.trim_end_matches(['.', ' ']);
    !bare.is_empty() && !bare.eq_ignore_ascii_case(".git") && !is_git_short_name(bare)
}

/// Steuerzeichen (`Cc`) und die Formatzeichen, die Git selbst in
/// `.git`-Namen abfängt (`U+200C`–`U+200F`, `U+202A`–`U+202E`,
/// `U+206A`–`U+206F`, `U+FEFF`): unsichtbar, und HFS+ ignoriert sie beim
/// Vergleich.
pub fn is_control_or_format(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
        )
}

/// `GIT~1`, `git~12` — der 8.3-Kurzname, unter dem Win32 `.git` erreicht.
fn is_git_short_name(segment: &str) -> bool {
    segment
        .get(..4)
        .is_some_and(|head| head.eq_ignore_ascii_case("git~"))
        && segment.len() > 4
        && segment[4..].bytes().all(|b| b.is_ascii_digit())
}

fn check_probe(probe: &Probe) -> Result<(), &'static str> {
    match probe {
        Probe::Pattern {
            path,
            glob,
            must_match,
            must_not_match,
        } => {
            match (path, glob) {
                (Some(path), None) => {
                    if !is_repo_relative(path) {
                        return Err("path is not repo-relative");
                    }
                }
                (None, Some(glob)) => {
                    if !is_glob(glob) {
                        return Err("glob is empty, too long, or not repo-relative");
                    }
                }
                _ => return Err("exactly one of path and glob"),
            }
            if must_match.is_none() && must_not_match.is_none() {
                return Err("at least one of must_match and must_not_match");
            }
            for pattern in [must_match, must_not_match].into_iter().flatten() {
                if pattern.is_empty() || pattern.len() > MAX_TEXT {
                    return Err("pattern is empty or too long");
                }
            }
            Ok(())
        }
        Probe::Path { path, .. } => is_repo_relative(path)
            .then_some(())
            .ok_or("path is not repo-relative"),
        Probe::Blob { path, blob } => {
            if !is_repo_relative(path) {
                return Err("path is not repo-relative");
            }
            let hex = (blob.len() == 40 || blob.len() == 64)
                && blob
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            hex.then_some(())
                .ok_or("blob is not a lowercase hex object id")
        }
    }
}

/// Nennt `command` `minds decide` als Kommando?
///
/// Die Regel, nach der der Checkpoint die Tool-Antwort nach
/// `decision`-Zeilen durchsucht und die Story den Schritt zeigt. Ein Glied
/// der Kette genügt: Das Kommando wird an Zeilenenden, `;`, `|`, `&` und
/// `(` in Glieder zerlegt; beginnt ein Glied mit `minds decide` oder
/// `…/minds decide` (ein gebautes `target/debug/minds` ist ein Aufruf), ist
/// es einer. `cd a && minds decide < d.json`, `cat d.json | minds decide;
/// minds decisions` und ein Heredoc `minds decide <<'EOF' … EOF` sind
/// Aufrufe; `minds decisions`, `echo minds decided` und `cargo run --
/// decide` nicht.
///
/// Vor dem Programm dürfen Zuweisungen (`FOO=1 minds decide`) und die
/// üblichen Einleitungen stehen (`time`, `exec`, `if`, `!`, `{` …); ein
/// Aufruf in `$(…)` gilt über die Klammer ebenfalls. Nicht erkannt wird
/// eine Fortsetzung mit `\` über Zeilen — fail-closed, keine Verknüpfung.
/// Ein Treffer zu viel schadet nicht: Ohne `decision`-Zeile in der Antwort
/// entsteht kein Event, und eine fremde Zeile scheitert am Zeitfenster. Wer
/// die Regel weitet, lese dennoch zuerst das Bedrohungsmodell dort.
///
/// Die Regel ist bewusst weit — sie sagt nur, **wo** der Checkpoint nach
/// der Zeile sucht. Dass die Zeile von genau diesem Aufruf stammt, belegt
/// sie nicht (ein `echo` daneben könnte sie drucken, ein anderes Programm
/// namens `minds` auch): Das prüft der Reader über die Zeit des Records,
/// die innerhalb des Aufrufs liegen muss ([`DecisionEvent`]).
pub fn names_decide(command: &str) -> bool {
    /// Wörter, die vor dem Programm stehen dürfen: Zuweisungen (`FOO=1`)
    /// und die Shell-Wörter, mit denen Agenten einen Aufruf einleiten.
    fn is_prefix(word: &str) -> bool {
        const PREFIXES: [&str; 12] = [
            "time", "exec", "command", "nohup", "if", "then", "else", "do", "!", "{", "sudo", "env",
        ];
        PREFIXES.contains(&word)
            || word.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
    }
    command
        .split(['\n', '\r', ';', '|', '&', '(', '`'])
        .any(|segment| {
            let mut words = segment
                .split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | ')'))
                .filter(|w| !w.is_empty())
                .skip_while(|w| is_prefix(w));
            let program = words.next().unwrap_or("");
            (program == "minds" || program.ends_with("/minds")) && words.next() == Some("decide")
        })
}

/// Die Zeile `decision b3-<64 hex>`, wie `minds decide` sie als erste
/// druckt — genau diese Form (Kleinbuchstaben), sonst nichts.
pub fn decision_line(line: &str) -> Option<ContentHash> {
    let id = line.trim_end().strip_prefix("decision ")?;
    let hex = id.strip_prefix("b3-")?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    id.parse().ok()
}

/// Alle `decision b3-…`-Zeilen einer Tool-Ausgabe, in Reihenfolge, jede Id
/// einmal — die Ausgabe einer Kette trägt sie irgendwo, nicht zuerst.
pub fn decision_lines(output: &str) -> Vec<ContentHash> {
    let mut out: Vec<ContentHash> = Vec::new();
    for id in output.lines().filter_map(decision_line) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Die Beobachtung im gespeicherten Envelope: In dieser Session wurde ein
/// Record aufgenommen.
///
/// Der Agent nimmt eine Entscheidung mit seinem eigenen Werkzeug auf —
/// `minds decide` als Tool-Aufruf, den die Hooks wie jeden anderen sehen
/// (A1) und ein Witness verkettet (A2). Das Kommando druckt die Id des
/// abgelegten Records; der Checkpoint liest sie aus der Tool-Antwort dieses
/// Aufrufs und trägt sie hier ein, mit der Journal-Nummer des Events. Kein
/// eigener Journal-Schreiber, kein Freitext im Envelope — nur Nummer und
/// Id (W2). Ob der Record existiert, prüfbar ist oder noch gilt, rechnet
/// der Reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionEvent {
    /// Die Journal-Nummer des Tool-Events, dessen Antwort die Id nannte.
    pub seq: u64,
    /// Die Record-Id.
    pub decision_id: ContentHash,
    /// Der Zug in [`Session::turns`](crate::Session::turns), dessen Aufruf
    /// die Id nannte — damit der Reader die Zeit des Records gegen das
    /// Fenster des Aufrufs hält (Anfang des `PreToolUse`, Ende des
    /// `PostToolUse`). Fehlt es (Records älterer Binaries), ist die Nennung
    /// nicht prüfbar und wird nicht gezeigt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<usize>,
    /// Der Aufruf in `turns[turn].tool_calls`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Entscheidung aus dem Entwurf: flock statt Socket, zwei
    /// verworfene Alternativen, drei Annahmen, eine davon ohne Probe.
    fn sample() -> DecisionRecord {
        DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "lock for the witness socket on macOS".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T13:09:00Z".into(),
            chosen: Some(Choice {
                approach: "flock on a lock file".into(),
                rests_on: vec![Reason {
                    assumption: "a1".into(),
                    weight: Weight::LoadBearing,
                }],
            }),
            open: false,
            alternatives: vec![
                Alternative {
                    approach: "Unix socket as the lock".into(),
                    rejected_because: vec![
                        Reason {
                            assumption: "a2".into(),
                            weight: Weight::LoadBearing,
                        },
                        Reason {
                            assumption: "a3".into(),
                            weight: Weight::Marginal,
                        },
                    ],
                    rests_on: Vec::new(),
                },
                Alternative {
                    approach: "named pipe".into(),
                    rejected_because: vec![Reason {
                        assumption: "a3".into(),
                        weight: Weight::LoadBearing,
                    }],
                    rests_on: Vec::new(),
                },
            ],
            assumptions: vec![
                Assumption {
                    id: "a1".into(),
                    statement: "the lock file lives on a local file system".into(),
                    probe: None,
                },
                Assumption {
                    id: "a2".into(),
                    statement: "daemon.rs forks".into(),
                    probe: Some(Probe::Pattern {
                        path: Some("crates/minds-cli/src/witness_cmd/daemon.rs".into()),
                        glob: None,
                        must_match: Some(r"\bfork\(".into()),
                        must_not_match: None,
                    }),
                },
                Assumption {
                    id: "a3".into(),
                    statement: "no Windows support is needed for the lock".into(),
                    probe: Some(Probe::Pattern {
                        path: None,
                        glob: Some("crates/*/Cargo.toml".into()),
                        must_match: None,
                        must_not_match: Some("windows-sys".into()),
                    }),
                },
            ],
            scope: vec!["crates/minds-cli/src/witness_cmd/**".into()],
            supersedes: None,
        }
    }

    /// Eingefrorene kanonische Form und Id: Ändert sich ein Schlüssel,
    /// ändert sich jede Id — und jedes `supersedes` zeigt ins Leere.
    #[test]
    fn canonical_form_golden() {
        let record = sample();
        record.check().unwrap();
        let json = crate::to_canonical_string(&record).unwrap();
        assert_eq!(
            json,
            r#"{"alternatives":[{"approach":"Unix socket as the lock","rejected_because":[{"assumption":"a2","weight":"load-bearing"},{"assumption":"a3","weight":"marginal"}]},{"approach":"named pipe","rejected_because":[{"assumption":"a3","weight":"load-bearing"}]}],"assumptions":[{"id":"a1","statement":"the lock file lives on a local file system"},{"id":"a2","probe":{"must_match":"\\bfork\\(","path":"crates/minds-cli/src/witness_cmd/daemon.rs","type":"pattern"},"statement":"daemon.rs forks"},{"id":"a3","probe":{"glob":"crates/*/Cargo.toml","must_not_match":"windows-sys","type":"pattern"},"statement":"no Windows support is needed for the lock"}],"at":"2026-10-21T13:09:00Z","chosen":{"approach":"flock on a lock file","rests_on":[{"assumption":"a1","weight":"load-bearing"}]},"decided_by":"agent","kind":"decision","schema":1,"scope":["crates/minds-cli/src/witness_cmd/**"],"topic":"lock for the witness socket on macOS"}"#
        );
        assert_eq!(
            record.id().unwrap().as_str(),
            "b3-f1d311757e0f65b9852d2d5ad873beaf6ab3caf9e1e1caafe6252cfaeb9c7b53"
        );
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }

    /// Eingefrorene Form eines offenen Records mit `supersedes`: beide
    /// Schlüssel gehören zur Id.
    #[test]
    fn open_record_with_supersedes_golden() {
        let record = DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "retry policy for the mirror".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T14:00:00Z".into(),
            chosen: None,
            open: true,
            alternatives: vec![
                Alternative {
                    approach: "exponential backoff".into(),
                    rejected_because: Vec::new(),
                    rests_on: vec![Reason {
                        assumption: "flaky".into(),
                        weight: Weight::LoadBearing,
                    }],
                },
                Alternative {
                    approach: "fail fast".into(),
                    rejected_because: Vec::new(),
                    rests_on: Vec::new(),
                },
            ],
            assumptions: vec![Assumption {
                id: "flaky".into(),
                statement: "the GitLab instance drops connections under load".into(),
                probe: None,
            }],
            scope: Vec::new(),
            supersedes: Some(ContentHash::from_bytes([7; 32])),
        };
        record.check().unwrap();
        assert_eq!(
            crate::to_canonical_string(&record).unwrap(),
            r#"{"alternatives":[{"approach":"exponential backoff","rests_on":[{"assumption":"flaky","weight":"load-bearing"}]},{"approach":"fail fast"}],"assumptions":[{"id":"flaky","statement":"the GitLab instance drops connections under load"}],"at":"2026-10-21T14:00:00Z","decided_by":"agent","kind":"decision","open":true,"schema":1,"supersedes":"b3-0707070707070707070707070707070707070707070707070707070707070707","topic":"retry policy for the mirror"}"#
        );
    }

    #[test]
    fn an_open_decision_has_options_and_no_chosen_path() {
        let mut open = sample();
        open.chosen = None;
        open.open = true;
        for alternative in &mut open.alternatives {
            alternative.rejected_because.clear();
        }
        open.check().unwrap();
        let json = crate::to_canonical_string(&open).unwrap();
        assert!(json.contains("\"open\":true"));
        assert!(!json.contains("\"chosen\""));

        let mut both = sample();
        both.open = true;
        assert_eq!(both.check(), Err(DecisionError::OpenOrChosen));
        let mut neither = sample();
        neither.chosen = None;
        assert_eq!(neither.check(), Err(DecisionError::OpenOrChosen));
        let mut empty = open.clone();
        empty.alternatives.clear();
        assert_eq!(empty.check(), Err(DecisionError::NoOptions));
        // Eine Option, die schon verworfen ist, ist keine Option.
        let mut rejected = open.clone();
        rejected.alternatives[0].rejected_because.push(Reason {
            assumption: "a2".into(),
            weight: Weight::Marginal,
        });
        assert_eq!(rejected.check(), Err(DecisionError::OpenWithRejection));
    }

    #[test]
    fn every_reason_points_at_a_known_assumption_exactly_once() {
        let mut record = sample();
        record.alternatives[0].rejected_because[0].assumption = "a9".into();
        assert_eq!(
            record.check(),
            Err(DecisionError::UnknownAssumption(
                "alternatives[0].rejected_because".into(),
                0
            ))
        );
        let mut record = sample();
        record.chosen.as_mut().unwrap().rests_on[0].assumption = "nope".into();
        assert_eq!(
            record.check(),
            Err(DecisionError::UnknownAssumption(
                "chosen.rests_on".into(),
                0
            ))
        );
        let mut record = sample();
        let twice = record.chosen.as_ref().unwrap().rests_on[0].clone();
        record.chosen.as_mut().unwrap().rests_on.push(twice);
        assert_eq!(
            record.check(),
            Err(DecisionError::DuplicateReason("chosen.rests_on".into(), 1))
        );
        // Kein Fehler nennt einen Wert — auch keine token-förmige Kennung.
        let mut record = sample();
        record.alternatives[0].rejected_because[0].assumption =
            "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d".into();
        let err = record.check().unwrap_err();
        assert!(!err.to_string().contains("ghp_"), "{err}");
        let mut record = sample();
        record.assumptions[1].id = "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d".into();
        record.assumptions[1].probe = Some(Probe::Pattern {
            path: Some("a.rs".into()),
            glob: Some("*".into()),
            must_match: Some("x".into()),
            must_not_match: None,
        });
        let err = record.check().unwrap_err();
        assert_eq!(err, DecisionError::Probe(1, "exactly one of path and glob"));
        assert!(!err.to_string().contains("ghp_"), "{err}");
    }

    #[test]
    fn assumption_ids_are_short_plain_and_unique() {
        let mut record = sample();
        record.assumptions[1].id = "a1".into();
        assert_eq!(record.check(), Err(DecisionError::AssumptionId(1)));
        for bad in ["a 1", "", "ä", "a/b", "a.b", &"a".repeat(MAX_ID + 1)] {
            let mut record = sample();
            record.assumptions[0].id = bad.into();
            assert!(
                matches!(record.check(), Err(DecisionError::AssumptionId(_))),
                "{bad:?}"
            );
        }
        let mut ok = sample();
        ok.assumptions[0].id = "No_Windows-2".into();
        ok.chosen.as_mut().unwrap().rests_on[0].assumption = "No_Windows-2".into();
        ok.check().unwrap();
    }

    #[test]
    fn probes_are_complete_and_repo_relative() {
        let bad = |probe: Probe| {
            let mut record = sample();
            record.assumptions[0].probe = Some(probe);
            record.check().unwrap_err()
        };
        assert!(matches!(
            bad(Probe::Pattern {
                path: Some("a.rs".into()),
                glob: Some("*.rs".into()),
                must_match: Some("x".into()),
                must_not_match: None,
            }),
            DecisionError::Probe(_, "exactly one of path and glob")
        ));
        assert!(matches!(
            bad(Probe::Pattern {
                path: Some("a.rs".into()),
                glob: None,
                must_match: None,
                must_not_match: None,
            }),
            DecisionError::Probe(_, "at least one of must_match and must_not_match")
        ));
        for glob in [
            "",
            "../**",
            &"*".repeat(MAX_GLOB + 1),
            "C:/**",
            "a\\b",
            "//server/share/**",
            "!src/**",
            "./",
            "/",
            ".",
        ] {
            assert!(
                matches!(
                    bad(Probe::Pattern {
                        path: None,
                        glob: Some(glob.into()),
                        must_match: Some("x".into()),
                        must_not_match: None,
                    }),
                    DecisionError::Probe(_, "glob is empty, too long, or not repo-relative")
                ),
                "{glob:?}"
            );
        }
        for path in [
            "../etc/passwd",
            "/etc/passwd",
            "C:/Users/x",
            "C:x",
            ".git/config",
            "a/.GIT/x",
            "GIT~1/config",
            ".git./config",
            "a/.git /x",
            ".git:$I30:$INDEX_ALLOCATION",
            "a:b/c",
        ] {
            assert!(
                matches!(
                    bad(Probe::Path {
                        path: path.into(),
                        exists: true,
                    }),
                    DecisionError::Probe(_, "path is not repo-relative")
                ),
                "{path:?}"
            );
        }
        assert!(matches!(
            bad(Probe::Blob {
                path: "Cargo.toml".into(),
                blob: "DEADBEEF".into(),
            }),
            DecisionError::Probe(_, "blob is not a lowercase hex object id")
        ));
        let mut ok = sample();
        ok.assumptions[0].probe = Some(Probe::Blob {
            path: "Cargo.toml".into(),
            blob: "3f9c1e2a4b5d6e7f8091a2b3c4d5e6f708192a3b".into(),
        });
        ok.check().unwrap();
    }

    #[test]
    fn kind_schema_texts_time_and_scope_are_checked() {
        let mut record = sample();
        record.kind = "replay".into();
        assert_eq!(record.check(), Err(DecisionError::Kind));
        let mut record = sample();
        record.schema = 2;
        assert_eq!(record.check(), Err(DecisionError::Schema(2)));
        let mut record = sample();
        record.topic = "  ".into();
        assert_eq!(record.check(), Err(DecisionError::Text("topic")));
        let mut record = sample();
        record.at = "gestern".into();
        assert_eq!(record.check(), Err(DecisionError::At));
        let mut record = sample();
        record.at = "2026-10-21T15:09:00+02:00".into();
        record.check().unwrap();
        for glob in ["", "../x", "C:/x", &"a".repeat(MAX_GLOB + 1)] {
            let mut record = sample();
            record.scope.push(glob.into());
            assert_eq!(record.check(), Err(DecisionError::Scope), "{glob:?}");
        }
        let mut record = sample();
        record.scope = (0..=MAX_ITEMS).map(|i| format!("g{i}/**")).collect();
        assert_eq!(record.check(), Err(DecisionError::TooMany("scope globs")));
        let mut record = sample();
        record.assumptions = (0..=MAX_ITEMS)
            .map(|i| Assumption {
                id: format!("a{i}"),
                statement: "x".into(),
                probe: None,
            })
            .collect();
        assert_eq!(record.check(), Err(DecisionError::TooMany("assumptions")));
    }

    #[test]
    fn unknown_fields_are_tolerated_and_kind_is_required() {
        // Vorwärts-Toleranz beim Lesen …
        let json = r#"{"kind":"decision","schema":1,"topic":"t","decided_by":"human","at":"2026-10-21T13:09:00Z","chosen":{"approach":"x"},"future":1}"#;
        let record: DecisionRecord = serde_json::from_str(json).unwrap();
        record.check().unwrap();
        assert!(record.assumptions.is_empty());
        // … aber ein Dokument ohne `kind` ist keine Entscheidung.
        let json = r#"{"schema":1,"topic":"t","decided_by":"human","at":"2026-10-21T13:09:00Z","chosen":{"approach":"x"}}"#;
        let record: DecisionRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.check(), Err(DecisionError::Kind));
    }

    /// Die Regel für den eigenen `minds decide`-Aufruf: ein Glied der Kette
    /// genügt, Zuweisungen und Einleitungen davor sind erlaubt — die Zeile
    /// in der Antwort zählt überall, die Prüfung macht der Reader am
    /// Zeitfenster.
    #[test]
    fn a_decide_call_is_recognized_as_a_link_of_a_chain() {
        for yes in [
            "minds decide",
            "minds decide --human",
            "minds decide < decision.json",
            "cat d.json | minds decide",
            "cd a && minds decide < d.json",
            "S=/tmp/x && sed 's/a/b/' $S/d.json > $S/e.json && minds decide < $S/e.json; minds decisions",
            "./target/debug/minds decide <<'EOF'\n{\"topic\":\"x\"}\nEOF",
            "minds decide --template >/dev/null; echo done",
            "(minds decide < d.json)",
            "minds decide < d.json 2>&1 | tail -3",
            "minds decide < d.json\r\nminds decisions\r\n",
            "FOO=1 BAR=x minds decide < d.json",
            "{ minds decide < d.json; }",
            "time minds decide < d.json",
            "if minds decide < d.json; then echo ok; fi",
            "! minds decide < d.json",
            "exec minds decide < d.json",
            "echo `minds decide < d.json`",
            "minds decide --template >/dev/null; sleep 115; minds decisions",
        ] {
            assert!(names_decide(yes), "{yes:?}");
        }
        for no in [
            "minds decisions",
            "minds decided",
            "reminds decide",
            "echo minds decide",
            "echo decision | tee x",
            "cargo run -- decide",
            "minds agent-help | grep decide",
            // Eine Umformung allein öffnet kein Fenster.
            "minds decisions | sed 's/^/decision /'",
            // Fortsetzung über Zeilen: kein Aufruf (fail-closed).
            "minds \\\ndecide < d.json",
            "",
        ] {
            assert!(!names_decide(no), "{no:?}");
        }
        let hex = "f1d311757e0f65b9852d2d5ad873beaf6ab3caf9e1e1caafe6252cfaeb9c7b53";
        assert_eq!(
            decision_line(&format!("decision b3-{hex}")).unwrap().hex(),
            hex
        );
        assert_eq!(
            decision_line(&format!("decision b3-{hex}  ")).map(|_| ()),
            Some(())
        );
        assert_eq!(decision_line(&format!("decision {hex}")), None);
        assert_eq!(decision_line("decision b3-abc"), None);
        assert_eq!(decision_line(&format!(" decision b3-{hex}")), None);
        assert_eq!(
            decision_line(&format!("decision b3-{}", hex.to_ascii_uppercase())),
            None
        );
        // Alle Zeilen, in Reihenfolge, jede Id einmal.
        let other = "0000000000000000000000000000000000000000000000000000000000000001";
        let out =
            format!("note\ndecision b3-{hex}\n  topic x\ndecision b3-{other}\ndecision b3-{hex}\n");
        let ids: Vec<String> = decision_lines(&out)
            .iter()
            .map(|id| id.hex().to_owned())
            .collect();
        assert_eq!(ids, vec![hex.to_owned(), other.to_owned()]);
        assert!(decision_lines("nothing here\n").is_empty());
    }

    #[test]
    fn repo_relative_paths() {
        assert!(is_repo_relative("src/lib.rs"));
        assert!(is_repo_relative("Cargo.toml"));
        assert!(is_repo_relative("a/.gitignore"));
        assert!(!is_repo_relative(""));
        assert!(!is_repo_relative("/abs"));
        assert!(!is_repo_relative("a/../b"));
        assert!(!is_repo_relative("a//b"));
        assert!(!is_repo_relative("a\\b"));
        assert!(!is_repo_relative("a\nb"));
        assert!(!is_repo_relative("C:/x"));
        assert!(!is_repo_relative("C:x"));
        assert!(!is_repo_relative(".git/config"));
        assert!(!is_repo_relative("sub/.git"));
        assert!(!is_repo_relative("GIT~1/config"));
        assert!(!is_repo_relative("sub/.git./x"));
        assert!(!is_repo_relative(".git:$I30:$INDEX_ALLOCATION"));
        assert!(is_repo_relative("git~x/y"));
        assert!(!is_repo_relative("a/./b"));
        assert!(!is_repo_relative("./a"));
        assert!(!is_repo_relative("a/.. /b"));
        assert!(!is_repo_relative("a/. /b"));
        assert!(!is_repo_relative("a/.../b"));
        assert!(!is_repo_relative("a/ /b"));
        assert!(!is_repo_relative(".g\u{200c}it/config"));
        assert!(!is_repo_relative("a\u{feff}b"));
        assert!(!is_repo_relative(&"a".repeat(MAX_PATH_BYTES + 1)));
        assert!(is_repo_relative(&"a".repeat(MAX_PATH_BYTES)));
    }
}
