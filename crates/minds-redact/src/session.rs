//! Die fail-closed-Garantie auf Session-Ebene — und der Audit, der sie belegt.
//!
//! [`crate::pipeline`] bereinigt *einen Text* und ist dabei bewusst infallibel:
//! Ein String lässt sich immer bereinigen. Diese Ebene beantwortet die andere
//! Hälfte der Frage — **wann darf eine ganze [`Session`] als redigiert gelten?**
//!
//! Antwort in einem Satz: nur dann, wenn jedes Textfeld des Envelopes
//! nachweislich durch die Pipeline gelaufen ist und dabei nichts schiefging.
//! Alles andere ist ein Fehler, kein halbes Ergebnis.
//!
//! # Drei Bauformen, die fail-closed *erzwingen* statt darum zu bitten
//!
//! **1. Die Session wird verbraucht, nicht geliehen.**
//! [`RedactionPipeline::redact_session`] nimmt die [`Session`] *by value* und
//! gibt im Fehlerfall **nichts** zurück. Die naheliegende Signatur
//! `fn redact_session(&self, s: &mut Session) -> Result<..>` wäre die
//! fail-open-Variante: Bricht sie in der Mitte ab, hält der Aufrufer eine halb
//! bereinigte Session in der Hand — und genau die wird dann doch gespeichert.
//! Was es nicht gibt, kann nicht versehentlich benutzt werden.
//!
//! **2. Der Nachweis ist ein Typ.** Erfolg liefert [`RedactedSession`], und
//! dieser Typ hat keinen öffentlichen Konstruktor. Wer ihn hat, hat ihn von der
//! Pipeline bekommen. `minds-store` (M4) sollte deshalb `&RedactedSession`
//! entgegennehmen und nicht `&Session`: Eine ungeredactete Session zu speichern
//! ist dann kein Policy-Verstoß mehr, sondern ein Compile-Fehler. Das Flag
//! `redaction.applied` im Envelope bleibt trotzdem nötig — es überlebt die
//! Serialisierung, der Typ nicht.
//!
//! **3. Ein neues Envelope-Feld bricht den Build.**
//! [`RedactionPipeline::redact_session`] zerlegt [`Session`], [`Intent`],
//! [`Turn`], [`ToolCall`], [`Agent`], [`Model`] und [`Produced`] per
//! *exhaustivem* Destructuring. Wächst das Schema um ein Feld, kompiliert genau
//! diese Funktion nicht mehr — der gewollte Zwang, für jedes neue Feld zu
//! entscheiden, ob es gescannt werden muss. Die Alternative (Feld für Feld über
//! `&mut` anfassen) hätte still weitergebaut und das neue Feld ungeprüft
//! durchgelassen.
//!
//! # Welche Felder gescannt werden: alle
//!
//! Jeder [`String`] im Envelope geht durch die Pipeline — auch `agent.name`,
//! `model.id` und `produced.commit_hint`, bei denen „da kann nichts drinstehen"
//! naheliegt. Die Ausnahme-Liste ist bewusst leer, aus zwei Gründen: Sie wäre
//! eine Annahme über *fremde* Adapter (M5 schreibt diese Felder), und sie
//! veraltet lautlos. Der Preis ist praktisch null — kein eingebauter Detektor
//! feuert auf `claude-opus-4` oder einen 40-stelligen Hex-SHA (Hex trägt
//! höchstens 4 bit/Zeichen und bleibt damit unter der Entropieschwelle). Trifft
//! eine *Denylist* dort, war das eine bewusste Konfigurationsentscheidung.
//!
//! # Was als Fehler gilt
//!
//! - [`RedactionError::NoDetectors`] — eine Pipeline ohne Detektoren würde
//!   jeden Text unverändert durchreichen und ihn anschließend als „redigiert"
//!   ausweisen. Das ist fail-open im Erfolgs-Gewand und deshalb der wichtigste
//!   der vier Fehler.
//! - [`RedactionError::AlreadyRedacted`] — eine bereits markierte Session
//!   erneut zu redigieren fände nichts mehr und würde die echten Zähler mit
//!   Nullen überschreiben. Der Audit wäre danach eine Lüge.
//! - [`RedactionError::InvalidFinding`] — ein Detektor hat den
//!   [`Finding`](crate::Finding)-Vertrag verletzt, sein Fund musste verworfen
//!   werden, und der zugehörige Text steht möglicherweise noch da. Die
//!   Pipeline-Doku verweist für diesen Fall auf „den fail-closed-Commit" — hier
//!   ist er.
//! - [`RedactionError::Unstable`] — siehe unten.
//!
//! # Der Verifikationslauf
//!
//! Für jedes Feld, das sich verändert hat, läuft die Pipeline ein zweites Mal
//! über das *Ergebnis*. Verändert sie es erneut, hat der erste Lauf etwas
//! stehen lassen; die Session wird verworfen. Felder, die der erste Lauf nicht
//! angefasst hat, brauchen keinen zweiten — die Pipeline ist deterministisch,
//! gleiche Eingabe hieße gleiches Ergebnis.
//!
//! Verglichen wird der **Text, nicht der Zähler** — das ist keine Schlamperei,
//! sondern nötig: Aus `DB_PASSWORD=hunter2` wird
//! `DB_PASSWORD=[redacted:secret]`, und im zweiten Lauf trifft
//! [`KeyValueRedactor`](crate::KeyValueRedactor) den Platzhalter erneut als
//! Wert hinter `PASSWORD=`. Er ersetzt ihn durch sich selbst: Zähler +1, Text
//! unverändert. Ein Zähler-Vergleich würde hier falschen Alarm schlagen, der
//! Text-Vergleich nicht.
//!
//! # Der Audit trägt Zähler und Ortsangaben — nie Werte
//!
//! [`RedactionAudit`] kennt drei Sorten Information: wie viele Felder geprüft
//! wurden, wie viel je Kategorie entfernt wurde, und **wo** — als
//! [`Field`]-Pfad wie `turns[3].tool_calls[0].arguments`. Ein Ort ist keine
//! Information über den entfernten Wert; er ist genau das, was ein Reviewer
//! braucht („warum ist der Record hier löchrig?"), ohne dass irgendwo ein
//! Geheimnis zwischengespeichert wird. Der Typ enthält schlicht keine
//! Zeichenkette aus dem Eingabetext — die Zusage ist strukturell, nicht
//! disziplinarisch.
//!
//! Ins Envelope wandert davon nur die Summe ([`minds_core::Redaction`]); die
//! Ortsangaben bleiben im Prozess und sind für die CLI-Ausgabe von
//! `minds capture` gedacht.

use std::fmt;

use minds_core::{
    Agent, Effect, Intent, Lineage, Model, Produced, Redaction, RedactionCounts, Session, ToolCall,
    Turn,
};

use crate::pipeline::RedactionPipeline;

// ---------------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------------

/// Warum eine Session **nicht** als redigiert gelten darf.
///
/// Jede Variante bedeutet dasselbe für den Aufrufer: Es gibt keine Session zu
/// speichern. Capture bricht ab.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedactionError {
    /// Die Pipeline enthält keinen einzigen Detektor.
    #[error(
        "Redaction-Policy ohne Detektoren: eine leere Pipeline ließe jeden Text \
         unverändert durch und würde ihn trotzdem als redigiert ausweisen"
    )]
    NoDetectors,

    /// Die Session trägt bereits `redaction.applied == true`.
    #[error(
        "Session ist bereits als redigiert markiert; ein zweiter Lauf fände \
         nichts mehr und würde den Audit des ersten überschreiben"
    )]
    AlreadyRedacted,

    /// Ein Detektor hat einen vertragswidrigen Span geliefert (leer,
    /// out-of-bounds oder nicht auf einer UTF-8-Grenze). Der Fund wurde
    /// verworfen — der Text an dieser Stelle ist damit möglicherweise
    /// ungeschwärzt.
    #[error(
        "Detektor-Vertrag verletzt: {count} ungültige Fund-Span(s) in {field} \
         mussten verworfen werden — der zugehörige Text ist möglicherweise \
         ungeschwärzt geblieben"
    )]
    InvalidFinding {
        /// Wo im Envelope.
        field: Field,
        /// Wie viele Funde verworfen wurden.
        count: u32,
    },

    /// Der Verifikationslauf hat den bereits bereinigten Text erneut verändert
    /// — die Bereinigung erreicht keinen Fixpunkt, der erste Lauf hat also
    /// etwas stehen lassen.
    #[error(
        "Redaction in {field} erreicht keinen Fixpunkt: ein zweiter Durchlauf \
         verändert den bereits bereinigten Text erneut"
    )]
    Unstable {
        /// Wo im Envelope.
        field: Field,
    },
}

// ---------------------------------------------------------------------------
// Feld-Pfade
// ---------------------------------------------------------------------------

/// Ein Textfeld des Envelopes — die Ortsangabe im Audit.
///
/// Bewusst ein Enum und kein `String`: Die Varianten sind vollständig
/// aufgezählt, [`Display`](fmt::Display) erzeugt daraus die kanonische
/// Pfadschreibweise (`turns[2].tool_calls[0].arguments`), und ein neues
/// Envelope-Feld zwingt zu einer neuen Variante statt zu einem frei getippten
/// Pfad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    /// `agent.name`
    AgentName,
    /// `agent.version`
    AgentVersion,
    /// `model.provider`
    ModelProvider,
    /// `model.id`
    ModelId,
    /// `intent.request`
    IntentRequest,
    /// `intent.constraints[i]`
    IntentConstraint(usize),
    /// `intent.discarded[i]`
    IntentDiscarded(usize),
    /// `turns[t].text`
    TurnText(usize),
    /// `turns[t].tool_calls[c].name`
    ToolCallName {
        /// Index des Zugs.
        turn: usize,
        /// Index des Tool-Calls im Zug.
        call: usize,
    },
    /// `turns[t].tool_calls[c].arguments`
    ToolCallArguments {
        /// Index des Zugs.
        turn: usize,
        /// Index des Tool-Calls im Zug.
        call: usize,
    },
    /// `turns[t].tool_calls[c].effect.path`
    EffectPath {
        /// Index des Zugs.
        turn: usize,
        /// Index des Tool-Calls im Zug.
        call: usize,
    },
    /// `produced.commit_hint`
    ProducedCommitHint,
    /// `produced.files[i]`
    ProducedFile(usize),
    /// `lineage.cwd`
    LineageCwd,
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Field::AgentName => f.write_str("agent.name"),
            Field::AgentVersion => f.write_str("agent.version"),
            Field::ModelProvider => f.write_str("model.provider"),
            Field::ModelId => f.write_str("model.id"),
            Field::IntentRequest => f.write_str("intent.request"),
            Field::IntentConstraint(i) => write!(f, "intent.constraints[{i}]"),
            Field::IntentDiscarded(i) => write!(f, "intent.discarded[{i}]"),
            Field::TurnText(t) => write!(f, "turns[{t}].text"),
            Field::ToolCallName { turn, call } => {
                write!(f, "turns[{turn}].tool_calls[{call}].name")
            }
            Field::ToolCallArguments { turn, call } => {
                write!(f, "turns[{turn}].tool_calls[{call}].arguments")
            }
            Field::EffectPath { turn, call } => {
                write!(f, "turns[{turn}].tool_calls[{call}].effect.path")
            }
            Field::ProducedCommitHint => f.write_str("produced.commit_hint"),
            Field::ProducedFile(i) => write!(f, "produced.files[{i}]"),
            Field::LineageCwd => f.write_str("lineage.cwd"),
        }
    }
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Ein Feld, in dem tatsächlich etwas ersetzt wurde — Ort plus Zähler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditSite {
    /// Wo im Envelope.
    pub field: Field,
    /// Wie viele Funde je Kategorie hier ersetzt wurden.
    pub counts: RedactionCounts,
}

/// Nachweis eines Session-weiten Redaction-Laufs: **nur Zähler und
/// Ortsangaben, niemals Werte.**
///
/// Der Typ enthält keine einzige aus dem Eingabetext stammende Zeichenkette —
/// weder [`Debug`] noch [`Display`](fmt::Display) können deshalb ein Geheimnis
/// ausgeben, egal wie sie aufgerufen werden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionAudit {
    counts: RedactionCounts,
    fields_scanned: usize,
    sites: Vec<AuditSite>,
}

impl RedactionAudit {
    /// Verbucht ein geprüftes Feld.
    fn record(&mut self, field: Field, counts: RedactionCounts) {
        self.fields_scanned += 1;
        if counts == RedactionCounts::default() {
            return;
        }
        self.counts.secrets = self.counts.secrets.saturating_add(counts.secrets);
        self.counts.pii = self.counts.pii.saturating_add(counts.pii);
        self.sites.push(AuditSite { field, counts });
    }

    /// Summe über alle Felder — genau das, was als Zähler in
    /// [`minds_core::Redaction`] ins Envelope geht.
    pub fn counts(&self) -> RedactionCounts {
        self.counts
    }

    /// Wie viele Textfelder geprüft wurden. Die interessante Zahl ist nicht,
    /// wie viel gefunden wurde, sondern dass *nichts* ungeprüft blieb.
    pub fn fields_scanned(&self) -> usize {
        self.fields_scanned
    }

    /// Wie viele Felder tatsächlich verändert wurden.
    pub fn fields_changed(&self) -> usize {
        self.sites.len()
    }

    /// Die veränderten Felder mit ihren Zählern, in Envelope-Reihenfolge.
    pub fn sites(&self) -> &[AuditSite] {
        &self.sites
    }

    /// `true`, wenn nichts gefunden wurde. **Nicht** dasselbe wie „nicht
    /// gelaufen" — ein sauberer Lauf ist der Normalfall.
    pub fn is_clean(&self) -> bool {
        self.counts == RedactionCounts::default()
    }
}

/// Einzeiler für die CLI: `2 Secret, 0 PII in 2 von 14 Textfeldern entfernt`.
impl fmt::Display for RedactionAudit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Secret, {} PII in {} von {} Textfeldern entfernt",
            self.counts.secrets,
            self.counts.pii,
            self.sites.len(),
            self.fields_scanned
        )
    }
}

// ---------------------------------------------------------------------------
// RedactedSession
// ---------------------------------------------------------------------------

/// Eine [`Session`], die die Redaction **nachweislich** durchlaufen hat.
///
/// Der einzige Weg zu diesem Typ führt über
/// [`RedactionPipeline::redact_session`] — er hat keinen öffentlichen
/// Konstruktor und keine öffentlichen Felder. Eine Funktion, die
/// `&RedactedSession` verlangt, kann von einer ungeredacteten Session gar nicht
/// erst aufgerufen werden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedSession {
    session: Session,
    audit: RedactionAudit,
}

impl RedactedSession {
    /// Die bereinigte Session. `redaction.applied` ist gesetzt, die Zähler
    /// stimmen mit [`audit`](Self::audit) überein.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Der Nachweis des Laufs (Zähler und Ortsangaben, keine Werte).
    pub fn audit(&self) -> &RedactionAudit {
        &self.audit
    }

    /// Gibt die bereinigte Session heraus und verwirft den Audit.
    pub fn into_session(self) -> Session {
        self.session
    }

    /// Zerlegt in bereinigte Session und Audit.
    pub fn into_parts(self) -> (Session, RedactionAudit) {
        (self.session, self.audit)
    }
}

// ---------------------------------------------------------------------------
// Der Session-weite Lauf
// ---------------------------------------------------------------------------

impl RedactionPipeline {
    /// Bereinigt **jedes** Textfeld einer [`Session`] und liefert den Nachweis.
    ///
    /// Erfolg heißt: alle Felder gescannt, kein Detektor-Vertrag verletzt,
    /// jedes veränderte Feld verifiziert, `redaction.applied` gesetzt und die
    /// Zähler eingetragen. Jeder Fehler heißt: **es gibt keine Session** — die
    /// übergebene ist verbraucht, eine bereinigte entsteht nicht. Genau das ist
    /// die fail-closed-Garantie (Architektur-Prinzip 3 im Plan).
    ///
    /// Was diese Funktion *nicht* prüfen kann, ist die Qualität der Policy: Eine
    /// Pipeline, die nur E-Mail-Adressen kennt, ist nicht leer und läuft durch.
    /// Die überprüfbare Untergrenze ist „mindestens ein Detektor"; darüber
    /// entscheidet [`RedactionConfig`](crate::RedactionConfig), deren Default
    /// alle eingebauten Detektoren anschaltet.
    ///
    /// ```
    /// use minds_core::{Agent, Intent, Model, Role, Session, Turn};
    /// use minds_redact::RedactionConfig;
    ///
    /// let mut session = Session::new(
    ///     Agent { name: "claude-code".into(), version: "1.0.0".into() },
    ///     Model { provider: "anthropic".into(), id: "claude-opus-4".into() },
    ///     Intent::default(),
    /// );
    /// session.turns.push(Turn {
    ///     role: Role::User,
    ///     text: "setz DB_PASSWORD=hunter2".into(),
    ///     tool_calls: Vec::new(),
    ///     parent: None,
    ///     at: None,
    /// });
    ///
    /// let pipeline = RedactionConfig::default().pipeline().unwrap();
    /// let redacted = pipeline.redact_session(session).unwrap();
    ///
    /// assert!(redacted.session().redaction.applied);
    /// assert_eq!(redacted.audit().counts().secrets, 1);
    /// assert!(!redacted.session().turns[0].text.contains("hunter2"));
    /// ```
    pub fn redact_session(&self, session: Session) -> Result<RedactedSession, RedactionError> {
        if self.is_empty() {
            return Err(RedactionError::NoDetectors);
        }
        if session.redaction.applied {
            return Err(RedactionError::AlreadyRedacted);
        }

        // Exhaustives Destructuring: Wächst das Envelope um ein Feld, bricht
        // *diese* Zeile den Build. Gewollt — siehe Modul-Doku.
        let Session {
            schema_version,
            agent,
            model,
            intent,
            turns,
            usage,
            produced,
            // `lineage.cwd` und `effect.path` (unten) tragen fast immer einen
            // Home-Pfad und damit einen Benutzernamen — PII. Sie laufen deshalb
            // durch dieselbe Pipeline wie jedes andere Textfeld. `edges` dagegen
            // hält nur Agentnamen, UUIDs und Commit-Hashes: nichts Sensibles,
            // deshalb unverändert durch.
            lineage,
            edges,
            // Wird unten aus dem Audit neu gesetzt; der alte Wert ist per
            // `AlreadyRedacted`-Prüfung ohnehin der Default.
            redaction: _,
        } = session;

        let mut audit = RedactionAudit::default();

        let Agent { name, version } = agent;
        let agent = Agent {
            name: self.redact_field(Field::AgentName, name, &mut audit)?,
            version: self.redact_field(Field::AgentVersion, version, &mut audit)?,
        };

        let Model { provider, id } = model;
        let model = Model {
            provider: self.redact_field(Field::ModelProvider, provider, &mut audit)?,
            id: self.redact_field(Field::ModelId, id, &mut audit)?,
        };

        let Intent {
            request,
            constraints,
            discarded,
        } = intent;
        let intent = Intent {
            request: self.redact_field(Field::IntentRequest, request, &mut audit)?,
            constraints: self.redact_list(constraints, Field::IntentConstraint, &mut audit)?,
            discarded: self.redact_list(discarded, Field::IntentDiscarded, &mut audit)?,
        };

        let mut redacted_turns = Vec::with_capacity(turns.len());
        for (turn_index, turn) in turns.into_iter().enumerate() {
            let Turn {
                role,
                text,
                tool_calls,
                parent,
                at,
            } = turn;

            let text = self.redact_field(Field::TurnText(turn_index), text, &mut audit)?;

            let mut redacted_calls = Vec::with_capacity(tool_calls.len());
            for (call_index, call) in tool_calls.into_iter().enumerate() {
                let ToolCall {
                    name,
                    arguments,
                    effect,
                } = call;
                let effect = self.redact_effect(effect, turn_index, call_index, &mut audit)?;
                redacted_calls.push(ToolCall {
                    effect,
                    // Der Tool-*Name* ist Vokabular des Agents und trägt
                    // normalerweise nichts Sensibles — normalerweise. Er kostet
                    // einen Scan über zehn Zeichen; die Ausnahme wäre teurer.
                    name: self.redact_field(
                        Field::ToolCallName {
                            turn: turn_index,
                            call: call_index,
                        },
                        name,
                        &mut audit,
                    )?,
                    arguments: self.redact_field(
                        Field::ToolCallArguments {
                            turn: turn_index,
                            call: call_index,
                        },
                        arguments,
                        &mut audit,
                    )?,
                });
            }

            redacted_turns.push(Turn {
                role,
                text,
                tool_calls: redacted_calls,
                parent,
                at,
            });
        }

        let Produced { commit_hint, files } = produced;
        let commit_hint = commit_hint
            .map(|hint| self.redact_field(Field::ProducedCommitHint, hint, &mut audit))
            .transpose()?;
        let files = self.redact_list(files, Field::ProducedFile, &mut audit)?;
        let produced = Produced { commit_hint, files };

        let lineage = self.redact_lineage(lineage, &mut audit)?;

        let session = Session {
            schema_version,
            agent,
            model,
            intent,
            turns: redacted_turns,
            usage,
            produced,
            lineage,
            edges,
            redaction: Redaction {
                applied: true,
                counts: audit.counts(),
            },
        };

        Ok(RedactedSession { session, audit })
    }

    /// Bereinigt ein einzelnes Feld, verifiziert das Ergebnis und verbucht es.
    fn redact_field(
        &self,
        field: Field,
        text: String,
        audit: &mut RedactionAudit,
    ) -> Result<String, RedactionError> {
        let first = self.redact(&text);
        if first.invalid_findings > 0 {
            return Err(RedactionError::InvalidFinding {
                field,
                count: first.invalid_findings,
            });
        }

        // Verifikation nur für Felder, die sich verändert haben: Blieb der Text
        // gleich, liefert ein zweiter Lauf über denselben Text zwangsläufig
        // dasselbe Ergebnis (die Pipeline ist deterministisch).
        if first.text != text {
            let second = self.redact(&first.text);
            if second.invalid_findings > 0 {
                return Err(RedactionError::InvalidFinding {
                    field,
                    count: second.invalid_findings,
                });
            }
            // Verglichen wird der Text, nicht der Zähler — siehe Modul-Doku
            // (Platzhalter können erneut treffen, ohne etwas zu verändern).
            if second.text != first.text {
                return Err(RedactionError::Unstable { field });
            }
        }

        audit.record(field, first.counts);
        Ok(first.text)
    }

    /// [`redact_field`](Self::redact_field) über eine Liste, mit Index im
    /// Feld-Pfad.
    fn redact_list(
        &self,
        items: Vec<String>,
        field_at: impl Fn(usize) -> Field,
        audit: &mut RedactionAudit,
    ) -> Result<Vec<String>, RedactionError> {
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            out.push(self.redact_field(field_at(index), item, audit)?);
        }
        Ok(out)
    }

    /// Redigiert den Pfad eines [`Effect`]; `kind` und `content` bleiben.
    ///
    /// `content` ist ein blake3-Hash (Hex, geringe Entropie) und trägt nichts
    /// Sensibles — der [`HighEntropyRedactor`](crate::HighEntropyRedactor) fasst
    /// ihn ohnehin nicht an. Redigiert wird nur der Pfad, der bei absoluten
    /// Angaben einen Benutzernamen enthalten kann.
    fn redact_effect(
        &self,
        effect: Option<Effect>,
        turn: usize,
        call: usize,
        audit: &mut RedactionAudit,
    ) -> Result<Option<Effect>, RedactionError> {
        let Some(Effect {
            kind,
            path,
            content,
        }) = effect
        else {
            return Ok(None);
        };
        let path = match path {
            Some(path) => Some(self.redact_field(Field::EffectPath { turn, call }, path, audit)?),
            None => None,
        };
        Ok(Some(Effect {
            kind,
            path,
            content,
        }))
    }

    /// Redigiert `lineage.cwd`; Kennung und Zeitfenster bleiben.
    ///
    /// `local_id` ist die agent-eigene UUID, `started_at`/`ended_at` sind
    /// Zeitstempel — beides keine PII. Nur `cwd` trägt in aller Regel einen
    /// Home-Pfad und damit einen Benutzernamen.
    fn redact_lineage(
        &self,
        lineage: Option<Lineage>,
        audit: &mut RedactionAudit,
    ) -> Result<Option<Lineage>, RedactionError> {
        let Some(Lineage {
            local_id,
            started_at,
            ended_at,
            cwd,
        }) = lineage
        else {
            return Ok(None);
        };
        let cwd = match cwd {
            Some(cwd) => Some(self.redact_field(Field::LineageCwd, cwd, audit)?),
            None => None,
        };
        Ok(Some(Lineage {
            local_id,
            started_at,
            ended_at,
            cwd,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use minds_core::Role;

    use crate::config::RedactionConfig;
    use crate::redactor::{Category, Finding, Redactor};

    // --- Hilfen ---------------------------------------------------------------

    /// Eine Session ohne alles Sensible. Bewusst über `Session::new` gebaut und
    /// nicht per Struct-Literal: So bleibt der Test gültig, wenn das Envelope um
    /// ein Feld wächst — brechen soll dann `redact_session`, nicht der Test.
    fn session() -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.0.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent::default(),
        )
    }

    fn pipeline() -> RedactionPipeline {
        RedactionConfig::default()
            .pipeline()
            .expect("Default-Policy muss bauen")
    }

    fn user_turn(text: &str) -> Turn {
        Turn {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        }
    }

    /// Detektor, der einen out-of-bounds-Span liefert — Vertragsbruch.
    struct Broken;

    impl Redactor for Broken {
        fn name(&self) -> &str {
            "broken"
        }

        fn scan(&self, text: &str) -> Vec<Finding> {
            vec![Finding::new(Category::Secret, 0, text.len() + 1)]
        }
    }

    /// Detektor, dessen eigener Platzhalter ihn erneut auslöst — die Bereinigung
    /// erreicht damit keinen Fixpunkt.
    struct SelfTriggering;

    impl Redactor for SelfTriggering {
        fn name(&self) -> &str {
            "self-triggering"
        }

        fn scan(&self, text: &str) -> Vec<Finding> {
            const NEEDLE: &str = "redacted";
            let mut out = Vec::new();
            let mut from = 0;
            while let Some(rel) = text[from..].find(NEEDLE) {
                let start = from + rel;
                let end = start + NEEDLE.len();
                out.push(Finding::new(Category::Secret, start, end));
                from = end;
            }
            out
        }
    }

    // --- Die Garantie ---------------------------------------------------------

    #[test]
    fn empty_pipeline_is_rejected() {
        // Der wichtigste Test der Datei: Eine Pipeline ohne Detektoren würde
        // alles durchreichen und trotzdem `applied` setzen.
        let err = RedactionPipeline::new()
            .redact_session(session())
            .unwrap_err();
        assert_eq!(err, RedactionError::NoDetectors);
    }

    #[test]
    fn already_redacted_session_is_rejected() {
        let mut s = session();
        s.redaction = Redaction {
            applied: true,
            counts: RedactionCounts { secrets: 3, pii: 0 },
        };
        assert_eq!(
            pipeline().redact_session(s).unwrap_err(),
            RedactionError::AlreadyRedacted
        );
    }

    #[test]
    fn clean_session_is_marked_applied_with_zero_counts() {
        // Sauber heißt nicht „nicht gelaufen": Das Flag wird gesetzt, die Zähler
        // bleiben null, und geprüft wurde trotzdem jedes Feld.
        let out = pipeline().redact_session(session()).unwrap();
        assert!(out.session().redaction.applied);
        assert_eq!(out.session().redaction.counts, RedactionCounts::default());
        assert!(out.audit().is_clean());
        assert_eq!(out.audit().fields_changed(), 0);
        assert!(out.audit().fields_scanned() >= 5);
    }

    #[test]
    fn invalid_finding_aborts_instead_of_leaking() {
        let err = RedactionPipeline::new()
            .with(Broken)
            .redact_session(session())
            .unwrap_err();
        assert!(
            matches!(err, RedactionError::InvalidFinding { .. }),
            "erwartet InvalidFinding, war: {err:?}"
        );
    }

    #[test]
    fn non_converging_redaction_aborts() {
        let mut s = session();
        s.turns.push(user_turn("hier steht redacted im Text"));
        let err = RedactionPipeline::new()
            .with(SelfTriggering)
            .redact_session(s)
            .unwrap_err();
        assert_eq!(
            err,
            RedactionError::Unstable {
                field: Field::TurnText(0)
            }
        );
    }

    // --- Abdeckung ------------------------------------------------------------

    #[test]
    fn every_text_field_of_the_envelope_is_covered() {
        // Ein Denylist-Begriff trifft in *jedem* Feld, unabhängig von Form und
        // Kontext — damit prüft dieser Test die Abdeckung des Durchlaufs und
        // nicht die Trefferquote der Detektoren.
        const TERM: &str = "korrekt-pferd-batterie-klammer";

        let pipeline = RedactionConfig {
            deny_secrets: vec![TERM.into()],
            ..RedactionConfig::default()
        }
        .pipeline()
        .unwrap();

        let mut s = Session::new(
            Agent {
                name: TERM.into(),
                version: TERM.into(),
            },
            Model {
                provider: TERM.into(),
                id: TERM.into(),
            },
            Intent {
                request: TERM.into(),
                constraints: vec![TERM.into()],
                discarded: vec![TERM.into()],
            },
        );
        s.turns.push(Turn {
            role: Role::Assistant,
            text: TERM.into(),
            tool_calls: vec![ToolCall {
                name: TERM.into(),
                arguments: TERM.into(),
                effect: None,
            }],
            parent: None,
            at: None,
        });
        s.produced = Produced {
            commit_hint: Some(TERM.into()),
            files: vec![TERM.into()],
        };

        let out = pipeline.redact_session(s).unwrap();
        let r = out.session();

        for text in [
            &r.agent.name,
            &r.agent.version,
            &r.model.provider,
            &r.model.id,
            &r.intent.request,
            &r.intent.constraints[0],
            &r.intent.discarded[0],
            &r.turns[0].text,
            &r.turns[0].tool_calls[0].name,
            &r.turns[0].tool_calls[0].arguments,
            r.produced.commit_hint.as_ref().unwrap(),
            &r.produced.files[0],
        ] {
            assert!(!text.contains(TERM), "Leck: {text}");
        }

        // Zwölf Textfelder, zwölf Treffer. Wächst das Envelope, bricht zuerst
        // `redact_session` — und danach diese Zahl.
        assert_eq!(out.audit().fields_changed(), 12);
        assert_eq!(r.redaction.counts.secrets, 12);
        assert_eq!(r.redaction.counts.pii, 0);
    }

    #[test]
    fn secret_in_turn_text_is_removed_and_counted() {
        let mut s = session();
        s.turns.push(user_turn("setz DB_PASSWORD=hunter2"));

        let out = pipeline().redact_session(s).unwrap();
        assert!(!out.session().turns[0].text.contains("hunter2"));
        assert!(out.session().turns[0].text.contains("DB_PASSWORD="));
        assert_eq!(out.session().redaction.counts.secrets, 1);
    }

    #[test]
    fn secret_in_tool_arguments_is_removed() {
        let mut s = session();
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "deploye".into(),
            tool_calls: vec![ToolCall {
                name: "bash".into(),
                arguments: r#"{"cmd":"deploy --token=ghp_012345678901234567890123456789012345"}"#
                    .into(),
                effect: None,
            }],
            parent: None,
            at: None,
        });

        let out = pipeline().redact_session(s).unwrap();
        let args = &out.session().turns[0].tool_calls[0].arguments;
        assert!(!args.contains("ghp_"), "Token steht noch da: {args}");
        assert_eq!(out.session().redaction.counts.secrets, 1);
    }

    #[test]
    fn envelope_counts_match_the_audit() {
        let mut s = session();
        s.turns
            .push(user_turn("DB_PASSWORD=hunter2 an anna@example.org"));

        let out = pipeline().redact_session(s).unwrap();
        assert_eq!(out.session().redaction.counts, out.audit().counts());
        assert_eq!(out.audit().counts().secrets, 1);
        assert_eq!(out.audit().counts().pii, 1);
    }

    // --- Der Audit ------------------------------------------------------------

    #[test]
    fn audit_names_the_field_of_each_finding() {
        let mut s = session();
        s.turns.push(user_turn("nichts"));
        s.turns.push(user_turn("DB_PASSWORD=hunter2"));

        let out = pipeline().redact_session(s).unwrap();
        let sites = out.audit().sites();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].field, Field::TurnText(1));
        assert_eq!(sites[0].counts.secrets, 1);
        assert_eq!(sites[0].field.to_string(), "turns[1].text");
    }

    #[test]
    fn audit_never_carries_the_removed_value() {
        // Die Zusage des Commits: nur Zähler, keine Werte. Der Typ enthält gar
        // keine Zeichenkette aus dem Eingabetext — weder Debug noch Display
        // können sie also ausgeben.
        let mut s = session();
        s.turns.push(Turn {
            role: Role::User,
            text: "DB_PASSWORD=hunter2".into(),
            tool_calls: vec![ToolCall {
                name: "bash".into(),
                arguments: "psql postgres://admin:s3cr3t@db.internal/prod".into(),
                effect: None,
            }],
            parent: None,
            at: None,
        });

        let out = pipeline().redact_session(s).unwrap();
        let debug = format!("{:?}", out.audit());
        let display = out.audit().to_string();

        for value in ["hunter2", "s3cr3t", "admin"] {
            assert!(!debug.contains(value), "Wert im Debug-Output: {debug}");
            assert!(
                !display.contains(value),
                "Wert im Display-Output: {display}"
            );
        }
    }

    #[test]
    fn field_paths_are_canonical() {
        assert_eq!(Field::AgentName.to_string(), "agent.name");
        assert_eq!(
            Field::IntentConstraint(2).to_string(),
            "intent.constraints[2]"
        );
        assert_eq!(
            Field::ToolCallArguments { turn: 3, call: 0 }.to_string(),
            "turns[3].tool_calls[0].arguments"
        );
        assert_eq!(Field::ProducedFile(1).to_string(), "produced.files[1]");
        assert_eq!(
            Field::EffectPath { turn: 2, call: 1 }.to_string(),
            "turns[2].tool_calls[1].effect.path"
        );
        assert_eq!(Field::LineageCwd.to_string(), "lineage.cwd");
    }

    // --- M6: lineage.cwd und effect.path ------------------------------------

    #[test]
    fn pii_in_lineage_cwd_is_removed_and_located() {
        // Der reale Fall: Der Checkpoint füllt `cwd` mit dem Arbeitsverzeichnis,
        // und das trägt hier eine E-Mail — die einzige Sorte, die die Pipeline
        // im Pfad auch belastbar erkennt.
        let mut s = session();
        s.lineage = Some(Lineage {
            local_id: "31f3f224".into(),
            started_at: Some("2026-07-23T09:12:04.512Z".into()),
            ended_at: None,
            cwd: Some("/home/anna@example.com/projekt".into()),
        });

        let out = pipeline().redact_session(s).unwrap();
        let lineage = out.session().lineage.clone().unwrap();

        assert!(
            !lineage.cwd.as_deref().unwrap().contains("anna@example.com"),
            "PII blieb: {:?}",
            lineage.cwd
        );
        assert_eq!(out.audit().counts().pii, 1);
        assert!(
            out.audit()
                .sites()
                .iter()
                .any(|site| site.field == Field::LineageCwd),
            "cwd nicht im Audit lokalisiert"
        );
        // Kennung und Zeitfenster sind keine PII und bleiben.
        assert_eq!(lineage.local_id, "31f3f224");
        assert_eq!(
            lineage.started_at.as_deref(),
            Some("2026-07-23T09:12:04.512Z")
        );
    }

    #[test]
    fn a_secret_in_an_effect_path_is_removed_and_located() {
        use minds_core::EffectKind;

        let mut s = session();
        let mut turn = user_turn("schreib was");
        turn.tool_calls.push(ToolCall {
            name: "Write".into(),
            arguments: "{}".into(),
            effect: Some(Effect {
                kind: EffectKind::Write,
                path: Some("/tmp/ghp_012345678901234567890123456789012345/out.rs".into()),
                content: None,
            }),
        });
        s.turns.push(turn);

        let out = pipeline().redact_session(s).unwrap();
        let effect = out.session().turns[0].tool_calls[0].effect.clone().unwrap();

        assert!(
            !effect.path.as_deref().unwrap().contains("ghp_012345"),
            "Secret blieb: {:?}",
            effect.path
        );
        assert!(
            out.audit()
                .sites()
                .iter()
                .any(|site| site.field == Field::EffectPath { turn: 0, call: 0 }),
            "effect.path nicht im Audit lokalisiert"
        );
        // Art und (hier fehlender) Hash bleiben unberührt.
        assert_eq!(effect.kind, EffectKind::Write);
    }

    #[test]
    fn an_ordinary_cwd_is_not_over_redacted() {
        // Ein gewöhnlicher Home-Pfad trägt zwar einen Vornamen, aber kein
        // detektierbares Muster — er muss unverändert durchlaufen, sonst
        // kostete der Scan echten Kontext.
        let mut s = session();
        s.lineage = Some(Lineage {
            local_id: "x".into(),
            started_at: None,
            ended_at: None,
            cwd: Some("/home/dev/projekt/minds".into()),
        });

        let out = pipeline().redact_session(s).unwrap();
        assert_eq!(
            out.session().lineage.as_ref().unwrap().cwd.as_deref(),
            Some("/home/dev/projekt/minds")
        );
        assert_eq!(out.audit().counts().pii, 0);
    }

    #[test]
    fn audit_display_is_a_one_liner_for_the_cli() {
        let mut s = session();
        s.turns.push(user_turn("DB_PASSWORD=hunter2"));
        let out = pipeline().redact_session(s).unwrap();
        assert!(
            out.audit()
                .to_string()
                .starts_with("1 Secret, 0 PII in 1 von ")
        );
    }

    // --- Herausgabe -----------------------------------------------------------

    #[test]
    fn into_parts_yields_session_and_audit() {
        let mut s = session();
        s.turns.push(user_turn("DB_PASSWORD=hunter2"));

        let (session, audit) = pipeline().redact_session(s).unwrap().into_parts();
        assert!(session.redaction.applied);
        assert_eq!(session.redaction.counts, audit.counts());
    }
}
