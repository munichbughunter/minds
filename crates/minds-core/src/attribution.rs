//! Attribution: wie viel eines Commits von einem Agenten stammt, mit welchem
//! Modell, auf welche Session hin.
//!
//! Wo die [`SessionId`] den *Kontext* einer Änderung trägt (was verlangt wurde,
//! was das Modell tat), beantwortet die [`Attribution`] die stumpfe Audit-Frage
//! aus der Vision: *Wer hat diese Zeilen geschrieben — Mensch oder Maschine, und
//! mit welchem Modell?* Sie hält drei Dinge:
//!
//! - **Prompt-Ref** ([`SessionId`]): welche Session diese Zeilen erzeugt hat.
//!   Damit ist Attribution *pro Session*, nicht pro Commit aggregiert — trägt ein
//!   Commit mehrere Sessions, trägt er mehrere Attribution-Trailer, jeder mit
//!   seiner eigenen Session-Ref.
//! - **Modell** ([`Model`]): welches Modell hinter den Agent-Zeilen stand.
//! - **Zeilen-Zähler** (`agent_lines` / `total_lines`): die menschlichen Zeilen
//!   sind die Differenz, der Prozentsatz wird abgeleitet — nie gespeichert.
//!
//! # Warum das Modell inline im Trailer steht
//!
//! Das Modell wäre über die Session-Ref auflösbar und ist damit streng genommen
//! redundant. Es steht trotzdem im Trailer, weil genau das die „graceful
//! degradation" aus dem Plan stützt: Ist der Kontext-Ref gerade nicht erreichbar
//! (Air-Gap, Child-Repo offline), zeigt `git log` immer noch „73% Agent, Modell
//! claude-opus-4". Die Audit-Antwort hängt nicht an der Erreichbarkeit des
//! Kontext-Stores.
//!
//! # Textform
//!
//! Die kanonische Wertform ist eine Folge von `key=value`-Token in fester
//! Reihenfolge:
//!
//! ```text
//! session=b3-<64 Hex> model=<provider>/<id> agent=<agent_lines>/<total_lines>
//! ```
//!
//! Genau dieser Wert steht rechts vom Doppelpunkt eines
//! `Minds-Attribution:`-Trailers (der Schlüssel lebt in [`crate::trailer`]).
//!
//! **Lesen tolerant, Schreiben kanonisch** — wie bei [`SessionId`] und
//! [`crate::trailer::Trailer`]: [`FromStr`] akzeptiert die drei Felder in
//! beliebiger Reihenfolge und mit beliebigem Leerraum dazwischen und erbt die
//! Hex-Toleranz der [`SessionId`]; [`Display`](fmt::Display) gibt ausschließlich
//! die kanonische Form aus (feste Feldreihenfolge, Kleinschreibung im Hex).
//!
//! # Kein Prozentsatz, keine Gleitkommazahl
//!
//! `73%` ist aus `146/200` ableitbar; beides zu speichern wäre eine
//! Denormalisierung, die auseinanderlaufen kann. Gespeichert werden nur die zwei
//! Ganzzahlen; [`Attribution::agent_percent`] rechnet bei Bedarf. Damit bleibt
//! das Modell — wie das restliche Envelope — frei von Fließkommazahlen und von
//! Rundungsfragen im gespeicherten Zustand.
//!
//! Dieses Modul hat **kein I/O**: es validiert, wandelt zwischen Text und Typ und
//! rechnet in-memory.

use std::fmt;
use std::str::FromStr;

use crate::id::{SessionId, SessionIdParseError};
use crate::session::Model;

/// Wie viel eines Commits von einem Agenten stammt, mit welchem Modell, auf
/// welche Session hin.
///
/// Die Felder sind privat und die Invarianten (`agent_lines <= total_lines`,
/// trailer-sicheres Modell) werden im Konstruktor [`Attribution::new`] und beim
/// Parsen erzwungen. Ein einmal existierender Wert ist damit immer gültig und
/// immer als kanonischer Trailer darstellbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    session: SessionId,
    model: Model,
    agent_lines: u32,
    total_lines: u32,
}

impl Attribution {
    /// Baut eine geprüfte Attribution.
    ///
    /// Schlägt fehl, wenn
    /// - `agent_lines > total_lines` (mehr Agent- als Gesamtzeilen ist unmöglich),
    /// - der Modell-`provider` leer ist, Leerraum/Steuerzeichen oder ein `/`
    ///   enthält, oder
    /// - die Modell-`id` leer ist oder Leerraum/Steuerzeichen enthält.
    ///
    /// Die Modell-Regeln sind das, was die Textform braucht: `provider/id` wird
    /// beim Lesen am **ersten** `/` getrennt, deshalb darf der Provider keins
    /// enthalten, die Id hingegen schon (HF-artige Ids wie `org/model`). Leerraum
    /// ist in beiden verboten, weil er die Token-Trennung des Trailers bräche.
    pub fn new(
        session: SessionId,
        model: Model,
        agent_lines: u32,
        total_lines: u32,
    ) -> Result<Self, AttributionError> {
        if agent_lines > total_lines {
            return Err(AttributionError::AgentExceedsTotal {
                agent: agent_lines,
                total: total_lines,
            });
        }
        check_provider(&model.provider)?;
        check_model_id(&model.id)?;
        Ok(Self {
            session,
            model,
            agent_lines,
            total_lines,
        })
    }

    /// Die Session, die diese Zeilen erzeugt hat (der „Prompt-Ref").
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Das Modell hinter den Agent-Zeilen.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Vom Agenten geschriebene Zeilen.
    pub fn agent_lines(&self) -> u32 {
        self.agent_lines
    }

    /// Gesamtzeilen der Änderung (Mensch + Agent).
    pub fn total_lines(&self) -> u32 {
        self.total_lines
    }

    /// Vom Menschen geschriebene Zeilen — die Differenz. Nie unterlaufend, weil
    /// `agent_lines <= total_lines` invariant ist.
    pub fn human_lines(&self) -> u32 {
        self.total_lines - self.agent_lines
    }

    /// Agent-Anteil in ganzen Prozent, kaufmännisch gerundet (round-half-up).
    ///
    /// Abgeleitet, nie gespeichert. Bei `total_lines == 0` (leere Änderung)
    /// liefert die `checked_div`-Kette `None` und damit definiert `0`, statt
    /// durch null zu teilen. Gerechnet wird in `u64`, damit auch große Zähler
    /// nicht überlaufen; das Ergebnis ist stets `<= 100` und passt in `u32`.
    pub fn agent_percent(&self) -> u32 {
        let agent = self.agent_lines as u64;
        let total = self.total_lines as u64;
        let rounded = (agent * 100 + total / 2).checked_div(total).unwrap_or(0);
        rounded as u32
    }
}

/// Kanonische Wertform: `session=… model=…/… agent=…/…`, feste Feldreihenfolge.
/// Das ist der Text rechts vom `Minds-Attribution:`-Doppelpunkt.
impl fmt::Display for Attribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "session={} model={}/{} agent={}/{}",
            self.session, self.model.provider, self.model.id, self.agent_lines, self.total_lines
        )
    }
}

/// Feldschlüssel der Textform.
const FIELD_SESSION: &str = "session";
const FIELD_MODEL: &str = "model";
const FIELD_AGENT: &str = "agent";

impl FromStr for Attribution {
    type Err = AttributionError;

    /// Parst die Wertform. Reihenfolge-tolerant: die drei Felder dürfen in
    /// beliebiger Reihenfolge stehen, getrennt durch beliebigen Leerraum; jedes
    /// muss genau einmal vorkommen. Fehlt eins, kommt eins doppelt vor oder steht
    /// ein unbekanntes Feld da, bricht das Parsen ab (fail-closed) — so trifft
    /// das Muster keine Prosa zufällig.
    ///
    /// Geschrieben wird dagegen ausschließlich die feste Reihenfolge aus
    /// [`Display`](fmt::Display).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut session: Option<SessionId> = None;
        let mut model: Option<Model> = None;
        let mut agent: Option<(u32, u32)> = None;

        for token in s.split_whitespace() {
            let (key, value) = token
                .split_once('=')
                .ok_or_else(|| AttributionError::MalformedToken(token.to_string()))?;
            match key {
                FIELD_SESSION => set_once(&mut session, FIELD_SESSION, value.parse()?)?,
                FIELD_MODEL => set_once(&mut model, FIELD_MODEL, parse_model(value)?)?,
                FIELD_AGENT => set_once(&mut agent, FIELD_AGENT, parse_counts(value)?)?,
                other => return Err(AttributionError::UnknownField(other.to_string())),
            }
        }

        let session = session.ok_or(AttributionError::MissingField(FIELD_SESSION))?;
        let model = model.ok_or(AttributionError::MissingField(FIELD_MODEL))?;
        let (agent_lines, total_lines) =
            agent.ok_or(AttributionError::MissingField(FIELD_AGENT))?;

        // `new` ist die einzige Stelle, die die Invarianten erzwingt — auch der
        // Parse-Pfad geht durch sie hindurch (agent<=total, Modell-Sicherheit).
        Attribution::new(session, model, agent_lines, total_lines)
    }
}

/// Trägt einen Wert genau einmal ein; ein zweiter Treffer ist ein
/// [`AttributionError::DuplicateField`].
fn set_once<T>(
    slot: &mut Option<T>,
    field: &'static str,
    value: T,
) -> Result<(), AttributionError> {
    if slot.is_some() {
        return Err(AttributionError::DuplicateField(field));
    }
    *slot = Some(value);
    Ok(())
}

/// Zerlegt `provider/id` am **ersten** `/`. Die Feinvalidierung (leer, verbotene
/// Zeichen) macht [`Attribution::new`] — hier fällt nur die grobe Struktur an.
fn parse_model(value: &str) -> Result<Model, AttributionError> {
    let (provider, id) = value
        .split_once('/')
        .ok_or_else(|| AttributionError::MalformedModel(value.to_string()))?;
    Ok(Model {
        provider: provider.to_string(),
        id: id.to_string(),
    })
}

/// Zerlegt `agent_lines/total_lines`. Die Invariante `agent <= total` prüft
/// [`Attribution::new`], nicht diese Funktion.
fn parse_counts(value: &str) -> Result<(u32, u32), AttributionError> {
    let (agent, total) = value
        .split_once('/')
        .ok_or_else(|| AttributionError::MalformedCounts(value.to_string()))?;
    let agent = agent
        .parse::<u32>()
        .map_err(|_| AttributionError::InvalidCount(agent.to_string()))?;
    let total = total
        .parse::<u32>()
        .map_err(|_| AttributionError::InvalidCount(total.to_string()))?;
    Ok((agent, total))
}

fn check_provider(provider: &str) -> Result<(), AttributionError> {
    if provider.is_empty() {
        return Err(AttributionError::EmptyModelProvider);
    }
    if provider.contains('/') {
        return Err(AttributionError::ProviderHasSlash(provider.to_string()));
    }
    if provider.chars().any(is_forbidden_in_model) {
        return Err(AttributionError::ModelHasWhitespace(provider.to_string()));
    }
    Ok(())
}

fn check_model_id(id: &str) -> Result<(), AttributionError> {
    if id.is_empty() {
        return Err(AttributionError::EmptyModelId);
    }
    if id.chars().any(is_forbidden_in_model) {
        return Err(AttributionError::ModelHasWhitespace(id.to_string()));
    }
    Ok(())
}

/// Leerraum und Steuerzeichen sind in Modell-Feldern verboten — sie würden die
/// Token-Trennung des Trailers brechen bzw. eine nicht mehr eindeutig lesbare
/// Zeile erzeugen.
fn is_forbidden_in_model(c: char) -> bool {
    c.is_whitespace() || c.is_control()
}

/// Fehler beim Bauen oder Parsen einer [`Attribution`].
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttributionError {
    /// Mehr Agent- als Gesamtzeilen — unmöglich.
    #[error("agent-Zeilen ({agent}) übersteigen Gesamtzeilen ({total})")]
    AgentExceedsTotal { agent: u32, total: u32 },

    /// Der Modell-Provider war leer.
    #[error("Modell-Provider darf nicht leer sein")]
    EmptyModelProvider,

    /// Die Modell-Id war leer.
    #[error("Modell-Id darf nicht leer sein")]
    EmptyModelId,

    /// Der Provider enthielt ein `/` (die Textform trennt `provider/id` am
    /// ersten `/`, deshalb muss der Provider frei davon sein).
    #[error("Modell-Provider darf kein '/' enthalten: {0:?}")]
    ProviderHasSlash(String),

    /// Ein Modell-Feld enthielt Leerraum oder Steuerzeichen.
    #[error("Modell-Feld darf keinen Leerraum und keine Steuerzeichen enthalten: {0:?}")]
    ModelHasWhitespace(String),

    /// Ein Token der Wertform hatte kein `=`.
    #[error("Attribution-Token ohne '=': {0:?}")]
    MalformedToken(String),

    /// Ein Feldschlüssel war keiner der erwarteten (`session`, `model`, `agent`).
    #[error("unbekanntes Attribution-Feld: {0:?}")]
    UnknownField(String),

    /// Ein Pflichtfeld kam mehr als einmal vor.
    #[error("doppeltes Attribution-Feld: {0}")]
    DuplicateField(&'static str),

    /// Ein Pflichtfeld fehlte.
    #[error("fehlendes Attribution-Feld: {0}")]
    MissingField(&'static str),

    /// Die Modell-Angabe war nicht `provider/id`.
    #[error("ungültige Modell-Angabe (erwartet provider/id): {0:?}")]
    MalformedModel(String),

    /// Die Zeilen-Angabe war nicht `agent/total`.
    #[error("ungültige Zeilen-Angabe (erwartet agent/total): {0:?}")]
    MalformedCounts(String),

    /// Ein Zeilen-Zähler war keine gültige u32.
    #[error("ungültige Zahl in Zeilen-Angabe: {0:?}")]
    InvalidCount(String),

    /// Die `session`-Angabe war keine gültige [`SessionId`].
    #[error("ungültige SessionId in Attribution: {0}")]
    Session(#[from] SessionIdParseError),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feste SessionId aus dem Golden-Vektor in [`crate::id`] — bindet die
    /// Attribution-Golden an die schon eingefrorene Session-Golden.
    const SAMPLE_SESSION: &str =
        "b3-a20e4a60acb3c7973efd344b3f27e91bf3b21211dbb64fc965bc32b4a8140bbd";

    /// Eingefrorene kanonische Wertform von [`sample()`]. Ändert sich hier etwas,
    /// ist das ein bewusster Bruch der Trailer-Grammatik — kein Refactor. Ein
    /// Dritter (Python, Go, ein Auditor) liest denselben Trailer.
    const GOLDEN_VALUE: &str = "session=b3-a20e4a60acb3c7973efd344b3f27e91bf3b21211dbb64fc965bc32b4a8140bbd model=anthropic/claude-opus-4 agent=146/200";

    fn model() -> Model {
        Model {
            provider: "anthropic".into(),
            id: "claude-opus-4".into(),
        }
    }

    fn sample() -> Attribution {
        Attribution::new(SAMPLE_SESSION.parse().unwrap(), model(), 146, 200).unwrap()
    }

    // --- Konstruktor & Invarianten -------------------------------------------

    #[test]
    fn new_accepts_valid_attribution() {
        let a = sample();
        assert_eq!(a.agent_lines(), 146);
        assert_eq!(a.total_lines(), 200);
        assert_eq!(a.model().id, "claude-opus-4");
        assert_eq!(a.session().to_string(), SAMPLE_SESSION);
    }

    #[test]
    fn new_rejects_agent_over_total() {
        let err = Attribution::new(SAMPLE_SESSION.parse().unwrap(), model(), 201, 200).unwrap_err();
        assert_eq!(
            err,
            AttributionError::AgentExceedsTotal {
                agent: 201,
                total: 200
            }
        );
    }

    #[test]
    fn new_allows_equal_agent_and_total() {
        // 100% Agent ist gültig.
        let a = Attribution::new(SAMPLE_SESSION.parse().unwrap(), model(), 200, 200).unwrap();
        assert_eq!(a.agent_percent(), 100);
        assert_eq!(a.human_lines(), 0);
    }

    #[test]
    fn new_rejects_provider_with_slash() {
        let m = Model {
            provider: "anthro/pic".into(),
            id: "x".into(),
        };
        let err = Attribution::new(SAMPLE_SESSION.parse().unwrap(), m, 1, 2).unwrap_err();
        assert!(matches!(err, AttributionError::ProviderHasSlash(_)));
    }

    #[test]
    fn new_rejects_whitespace_in_model() {
        let m = Model {
            provider: "anthropic".into(),
            id: "claude opus 4".into(),
        };
        let err = Attribution::new(SAMPLE_SESSION.parse().unwrap(), m, 1, 2).unwrap_err();
        assert!(matches!(err, AttributionError::ModelHasWhitespace(_)));
    }

    #[test]
    fn new_rejects_empty_model_fields() {
        let empty_provider = Model {
            provider: String::new(),
            id: "x".into(),
        };
        assert_eq!(
            Attribution::new(SAMPLE_SESSION.parse().unwrap(), empty_provider, 1, 2).unwrap_err(),
            AttributionError::EmptyModelProvider
        );
        let empty_id = Model {
            provider: "anthropic".into(),
            id: String::new(),
        };
        assert_eq!(
            Attribution::new(SAMPLE_SESSION.parse().unwrap(), empty_id, 1, 2).unwrap_err(),
            AttributionError::EmptyModelId
        );
    }

    // --- Abgeleitete Werte ----------------------------------------------------

    #[test]
    fn human_lines_is_total_minus_agent() {
        assert_eq!(sample().human_lines(), 54);
    }

    #[test]
    fn agent_percent_rounds_half_up() {
        let cases = [
            (146u32, 200u32, 73u32), // exakt 73.0
            (1, 3, 33),              // 33.33 → 33
            (2, 3, 67),              // 66.67 → 67
            (1, 8, 13),              // 12.5  → 13 (half-up)
            (3, 8, 38),              // 37.5  → 38
            (0, 0, 0),               // leere Änderung, kein Div-by-zero
            (0, 200, 0),
            (200, 200, 100),
        ];
        for (agent, total, expected) in cases {
            let a =
                Attribution::new(SAMPLE_SESSION.parse().unwrap(), model(), agent, total).unwrap();
            assert_eq!(a.agent_percent(), expected, "{agent}/{total}");
        }
    }

    // --- Golden: kanonische Textform ------------------------------------------

    #[test]
    fn display_is_frozen_canonical_value() {
        assert_eq!(sample().to_string(), GOLDEN_VALUE);
    }

    #[test]
    fn from_str_roundtrips_display() {
        let a = sample();
        let parsed: Attribution = a.to_string().parse().unwrap();
        assert_eq!(a, parsed);
    }

    #[test]
    fn from_str_parses_frozen_value() {
        assert_eq!(GOLDEN_VALUE.parse::<Attribution>().unwrap(), sample());
    }

    // --- Toleranz beim Lesen --------------------------------------------------

    #[test]
    fn from_str_is_field_order_tolerant() {
        let reordered =
            format!("agent=146/200 session={SAMPLE_SESSION} model=anthropic/claude-opus-4");
        assert_eq!(reordered.parse::<Attribution>().unwrap(), sample());
    }

    #[test]
    fn from_str_tolerates_extra_whitespace() {
        let spaced =
            format!("  session={SAMPLE_SESSION}    model=anthropic/claude-opus-4\tagent=146/200  ");
        assert_eq!(spaced.parse::<Attribution>().unwrap(), sample());
    }

    #[test]
    fn from_str_inherits_uppercase_hex_tolerance() {
        // SessionId liest Groß-Hex; die Attribution reicht das durch.
        let upper_hex = SAMPLE_SESSION[3..].to_uppercase();
        let line = format!("session=b3-{upper_hex} model=anthropic/claude-opus-4 agent=146/200");
        assert_eq!(line.parse::<Attribution>().unwrap(), sample());
    }

    #[test]
    fn from_str_preserves_slash_in_model_id() {
        // HF-artige Id mit `/`: am ersten `/` getrennt ⇒ provider=hf, id=org/model.
        let line = format!("session={SAMPLE_SESSION} model=hf/org/model agent=1/2");
        let a = line.parse::<Attribution>().unwrap();
        assert_eq!(a.model().provider, "hf");
        assert_eq!(a.model().id, "org/model");
        // Round-trip: die kanonische Ausgabe parst wieder auf dasselbe.
        assert_eq!(a.to_string().parse::<Attribution>().unwrap(), a);
    }

    // --- Ablehnungen ----------------------------------------------------------

    #[test]
    fn from_str_rejects_missing_field() {
        let line = format!("session={SAMPLE_SESSION} agent=1/2");
        assert_eq!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::MissingField(FIELD_MODEL)
        );
    }

    #[test]
    fn from_str_rejects_duplicate_field() {
        let line = format!("session={SAMPLE_SESSION} session={SAMPLE_SESSION} model=a/b agent=1/2");
        assert_eq!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::DuplicateField(FIELD_SESSION)
        );
    }

    #[test]
    fn from_str_rejects_unknown_field() {
        let line = format!("session={SAMPLE_SESSION} model=a/b agent=1/2 extra=x");
        assert_eq!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::UnknownField("extra".into())
        );
    }

    #[test]
    fn from_str_rejects_token_without_equals() {
        let line = format!("session={SAMPLE_SESSION} model=a/b agent=1/2 lonely");
        assert_eq!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::MalformedToken("lonely".into())
        );
    }

    #[test]
    fn from_str_rejects_bad_session() {
        let line = "session=nope model=a/b agent=1/2";
        assert!(matches!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::Session(_)
        ));
    }

    #[test]
    fn from_str_rejects_agent_over_total() {
        let line = format!("session={SAMPLE_SESSION} model=a/b agent=3/2");
        assert_eq!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::AgentExceedsTotal { agent: 3, total: 2 }
        );
    }

    #[test]
    fn from_str_rejects_malformed_counts() {
        let line = format!("session={SAMPLE_SESSION} model=a/b agent=1-2");
        assert!(matches!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::MalformedCounts(_)
        ));
    }

    #[test]
    fn from_str_rejects_non_numeric_count() {
        let line = format!("session={SAMPLE_SESSION} model=a/b agent=x/2");
        assert!(matches!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::InvalidCount(_)
        ));
    }

    #[test]
    fn from_str_rejects_model_without_slash() {
        let line = format!("session={SAMPLE_SESSION} model=noslash agent=1/2");
        assert!(matches!(
            line.parse::<Attribution>().unwrap_err(),
            AttributionError::MalformedModel(_)
        ));
    }

    #[test]
    fn debug_is_available() {
        // Debug bleibt strukturell (abgeleitet) — nützlich in Test-Panics.
        let s = format!("{:?}", sample());
        assert!(s.contains("Attribution"));
        assert!(s.contains("146"));
    }
}
