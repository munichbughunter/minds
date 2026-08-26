//!
//! Ein [`Session`] ist der dauerhafte, versionierte Record einer einzelnen
//! Agent-Session: was verlangt wurde, was das Modell getan hat und was dabei
//! entstanden ist. Dieses Modul enthält **ausschließlich** das Datenmodell und
//! seine serde-Anbindung — kein I/O, kein Hashing, keine Kanonisierung.
//!
//! Bewusste Design-Entscheidungen für Schema v1:
//! - `schema_version` wird treu gespeichert, nicht erzwungen: ein Reader kann
//!   so erkennen, mit welcher Version die Daten geschrieben wurden. Neue
//!   Sessions bekommen über [`Session::new`] automatisch [`SCHEMA_VERSION`].
//! - Kein `deny_unknown_fields`: unbekannte Felder werden ignoriert
//!   (Vorwärts-Toleranz, Architektur-Prinzip 4 im Plan).
//! - `Redaction` speichert **nur Zähler, niemals Werte**.
//! - Keine Fließkommazahlen im Envelope (Token-Zähler sind `u64`) — das hält
//!   die spätere Kanonisierung frei von Float-Formatierungsfragen.
//!
//! # M5: Ordnung und Herkunft, ohne Bruch
//!
//! Mit dem Umbau auf Hook-basiertes Capture kommen vier Felder dazu:
//! [`Session::lineage`], [`Session::edges`], [`Turn::parent`]/[`Turn::at`] und
//! [`ToolCall::effect`]. Alle sind additiv und tragen `skip_serializing_if` —
//! eine Session, die keines davon belegt, serialisiert **byte-identisch** wie
//! vorher und behält ihre `SessionId`. Deshalb blieb [`SCHEMA_VERSION`] damals
//! bei 1; ein Bump wäre eine Aussage über Inkompatibilität gewesen, die dort
//! nicht zutraf.
//!
//! Das Warum steht in [`crate::lineage`]; hier steht nur, wo es hängt.
//!
//! # Schema 2: Evidence in zwei Dimensionen (ADR-0011)
//!
//! `Edge.evidence` ist ab Schema 2 ein
//! [`EvidenceMark`](crate::lineage::EvidenceMark) (Objektform
//! `{"source":…,"status":…}`) statt des Legacy-Strings. Das ist die erste
//! Änderung, bei der der Bump auch real Lesbarkeit trennt: Ein neueres Binary
//! liest beide Formen (der Deserializer ist tolerant), ein Schema-1-Binary
//! liest Schema-2-Sessions **nicht**. Bestand wird nicht migriert —
//! content-adressierte Sessions sind unveränderlich und behalten ihre Bytes.
//!
//! # Ein Turn ist ein Knoten, keine Zeile
//!
//! [`Turn::parent`] ist der Preis dafür, dass Claude Codes Transkript kein
//! flaches Log ist, sondern ein Baum: Jedes Event verweist über `parentUuid`
//! auf sein Elternteil. Ein `/resume`, ein Rewind oder eine Sidechain erzeugt
//! eine Verzweigung. Plättet man die zu einer Liste, wird aus einer
//! Verzweigung ein Widerspruch — die Erzählung enthält dann zwei einander
//! widersprechende Fortsetzungen ohne Hinweis darauf, dass es Alternativen
//! waren. Ein `Option<u32>` je Turn kauft das ab.

use serde::{Deserialize, Serialize};

use crate::lineage::{Edge, Effect, Lineage};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub schema_version: u32,
    pub agent: Agent,
    pub model: Model,
    pub intent: Intent,
    #[serde(default)]
    pub turns: Vec<Turn>,
    pub usage: Usage,
    pub produced: Produced,
    pub redaction: Redaction,

    /// Woher diese Session stammt (agent-eigene Kennung, Zeitfenster, `cwd`).
    ///
    /// `None` heißt „unbekannt", nicht „keine". Sessions, die vor M5 erfasst
    /// wurden oder aus einem Adapter ohne Herkunftsinformation stammen, haben
    /// hier nichts — und serialisieren dadurch wie vorher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Lineage>,

    /// Beziehungen zu anderen Sessions und Commits, jede mit ihrer Herkunft.
    ///
    /// Leer ist der Normalfall beim Capture: Der Adapter sieht immer nur *eine*
    /// Session und kann deshalb höchstens die Kanten eintragen, die im eigenen
    /// Transkript beobachtbar sind (Sub-Agenten). Alles Übergreifende wird
    /// später aus dem Store-Index abgeleitet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
}

impl Session {
    /// Erzeugt eine neue Session mit aktueller [`SCHEMA_VERSION`] und leeren
    /// Sammelfeldern.
    ///
    /// `redaction.applied` startet bewusst als `false` (fail-closed): Erst
    /// nachdem die Redaction-Pipeline gelaufen ist, darf das Flag gesetzt und
    /// die Session gespeichert werden. Der Store weist ungeredigierte Sessions
    /// ab.
    pub fn new(agent: Agent, model: Model, intent: Intent) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            agent,
            model,
            intent,
            turns: Vec::new(),
            usage: Usage::default(),
            produced: Produced::default(),
            redaction: Redaction::default(),
            lineage: None,
            edges: Vec::new(),
        }
    }
}

/// Der Agent, der die Session gefahren hat (z. B. claude-code, codex).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
}

/// Das Modell hinter der Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub provider: String,
    pub id: String,
}

/// Die extrahierte Absicht: was verlangt wurde, unter welchen Constraints und
/// welche Pfade verworfen wurden. In v0.1 rein deterministisch befüllt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Intent {
    pub request: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub discarded: Vec<String>,
}

/// Ein einzelner Zug im Gesprächsverlauf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,

    /// Index des Vorgänger-Turns in [`Session::turns`]; `None` = Wurzel.
    ///
    /// Bei einem linearen Verlauf ist das schlicht `i - 1` und damit redundant
    /// — deshalb darf ein Adapter es weglassen, und die kanonische Form bleibt
    /// klein. Belegt wird es dort, wo es *nicht* redundant ist: nach einem
    /// Rewind, nach `/resume`, an einer Sidechain.
    ///
    /// Der Index zeigt immer nach **hinten** (`parent < eigener Index`). Ein
    /// Vorwärtsverweis wäre ein Zyklus im Envelope; das zu prüfen ist Sache des
    /// Validators, nicht des Typs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,

    /// Zeitpunkt, RFC 3339 in UTC — aus dem Transkript bzw. Hook-Event,
    /// niemals `now()` im Adapter. Zur Sortierung siehe [`crate::lineage`]:
    /// **nicht** per String-Vergleich.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// Rolle eines Zugs. Adapter normalisieren agent-spezifische Rollennamen auf
/// diese kanonische Menge.
///
/// Bewusst geschlossen für v1: eine neue Rolle wäre eine Schema-Änderung mit
/// Versions-Bump. Falls sich das als zu eng erweist, ist der Wechsel auf einen
/// `#[serde(other)] Unknown`-Fallback ein Einzeiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Ein Tool-Aufruf innerhalb eines Zugs.
///
/// Die Argumente werden als roher, bereits serialisierter String übernommen —
/// so hängt der spätere Hash nicht von der JSON-Formatierung des jeweiligen
/// Agents ab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: String,

    /// Ob und durch wen dieser Aufruf gedeutet wurde (ADR-0011).
    ///
    /// `None` nur bei Bestand aus der Zeit vor der Evidence-Chain — dieselbe
    /// Additiv-Regel wie bei [`Session::lineage`]. Ein beobachteter, aber
    /// nicht gedeuteter Aufruf trägt [`CaptureStatus::Uninterpreted`] statt
    /// still zu verschwinden: „Ich habe gesehen, dass ein Tool lief; seine
    /// Wirkung konnte ich nicht deuten."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<Capture>,

    /// Was der Aufruf in der Welt getan hat, **normalisiert**.
    ///
    /// Der Pfad steckt zwar auch in `arguments` — aber dort in der Sprache des
    /// jeweiligen Agents. Ihn später im Reader wieder herauszuparsen hieße,
    /// Agent-Spezifika außerhalb des Adapters wiederzubeleben; genau das soll
    /// der Adapter verhindern. Und die Secretfile-Mauer braucht ihn ohnehin,
    /// bevor irgendetwas gespeichert wird.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
}

/// Ob und wie ein Tool-Aufruf gedeutet wurde — die Deutungsgrenze als
/// Aussage statt als Schweigen (ADR-0011).
///
/// `adapter`/`adapter_version` machen die Deutung **wiederholbar**: Evidence
/// ist unveränderlich, Interpretation ist rekonstruierbar — ein späterer
/// Adapter v2 kann denselben erhaltenen Aufruf neu deuten, und ein Leser
/// sieht, mit welchem Stand die vorliegende Deutung entstand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    /// Gedeutet oder nur beobachtet.
    pub status: CaptureStatus,

    /// Wer gedeutet hat (`claude-code`, `generic`).
    pub adapter: String,

    /// Versionsstand dieser Deutung; Bump bei jeder Deutungsänderung.
    pub adapter_version: u32,
}

/// Der Deutungszustand eines beobachteten Tool-Aufrufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Der Adapter kennt das Tool und hat seine Wirkung gedeutet.
    Interpreted,

    /// Beobachtet, aber nicht gedeutet — Name und Roh-Argumente bleiben
    /// erhalten, die Wirkung ist unbekannt.
    Uninterpreted,
}

/// Token-Verbrauch der Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Was die Session hervorgebracht hat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Produced {
    /// Optionaler Hinweis auf den erzeugten Commit. Zum Capture-Zeitpunkt
    /// existiert der Commit oft noch nicht — die verbindliche Verlinkung
    /// erfolgt über den Trailer im Production-Commit, nicht über dieses Feld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hint: Option<String>,
    /// Betroffene Dateien (repo-relative Pfade).
    #[serde(default)]
    pub files: Vec<String>,
}

/// Nachweis der Redaction. Enthält **nur Zähler, niemals die entfernten Werte**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Redaction {
    /// `true`, sobald die Redaction-Pipeline erfolgreich lief. Startet als
    /// `false` (fail-closed).
    pub applied: bool,
    pub counts: RedactionCounts,
}

/// Zähler der entfernten Treffer, nach Kategorie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RedactionCounts {
    pub secrets: u32,
    pub pii: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lineage::{
        ContentHash, EdgeKind, Effect, EffectKind, Endpoint, EvidenceMark, EvidenceSource,
    };

    fn sample() -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.0.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: "Flaky Retry-Test reparieren".into(),
                constraints: vec!["keine neuen Dependencies".into()],
                discarded: vec!["Timeout einfach hochsetzen".into()],
            },
        );
        s.turns.push(Turn {
            role: Role::User,
            text: "Der Retry-Test flackert, bitte fixen.".into(),
            tool_calls: Vec::new(),
            parent: None,
            at: None,
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "Ich schaue mir die Backoff-Logik an.".into(),
            tool_calls: vec![ToolCall {
                capture: None,
                name: "read_file".into(),
                arguments: r#"{"path":"src/retry.rs"}"#.into(),
                effect: None,
            }],
            parent: None,
            at: None,
        });
        s.usage = Usage {
            input_tokens: 1234,
            output_tokens: 567,
        };
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s.redaction = Redaction {
            applied: true,
            counts: RedactionCounts { secrets: 0, pii: 1 },
        };
        s
    }

    #[test]
    fn new_sets_current_schema_version() {
        assert_eq!(SCHEMA_VERSION, 2);
        assert_eq!(sample().schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn new_defaults_are_fail_closed() {
        let s = Session::new(
            Agent {
                name: "a".into(),
                version: "1".into(),
            },
            Model {
                provider: "p".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        assert!(!s.redaction.applied, "Redaction muss un-applied starten");
        assert!(s.turns.is_empty());
        assert!(s.lineage.is_none());
        assert!(s.edges.is_empty());
    }

    #[test]
    fn a_schema_1_session_with_legacy_evidence_still_reads() {
        // Rueckwaerts-Zusage aus ADR-0011: Ein neueres Binary liest alle
        // aelteren Schema-Versionen. Der Legacy-String an der Kante wird auf
        // `(source, Unknown)` gemappt — keine Alt-Kante war je nachgerechnet.
        let json = r#"{
            "schema_version": 1,
            "agent": {"name": "a", "version": "1"},
            "model": {"provider": "p", "id": "m"},
            "intent": {"request": "mach x"},
            "usage": {"input_tokens": 0, "output_tokens": 0},
            "produced": {"files": []},
            "redaction": {"applied": true, "counts": {"secrets": 0, "pii": 0}},
            "edges": [{
                "kind": "produced",
                "to": {"type": "commit", "id": "deadbeef"},
                "evidence": "observed"
            }]
        }"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.schema_version, 1);
        assert_eq!(
            s.edges[0].evidence,
            EvidenceMark::of(EvidenceSource::Observed)
        );
    }

    #[test]
    fn json_roundtrip_is_lossless() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // Vorwärts-Toleranz: ein in einer künftigen Schema-Version ergänztes
        // Feld darf einen älteren Reader nicht brechen. Zugleich prüft dieser
        // Test die `#[serde(default)]`-Felder (fehlende constraints/commit_hint).
        let json = r#"{
            "schema_version": 1,
            "agent": {"name": "a", "version": "1"},
            "model": {"provider": "p", "id": "m"},
            "intent": {"request": "mach x"},
            "usage": {"input_tokens": 0, "output_tokens": 0},
            "produced": {"files": []},
            "redaction": {"applied": true, "counts": {"secrets": 0, "pii": 0}},
            "future_field": {"whatever": 42}
        }"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.intent.request, "mach x");
        assert!(s.intent.constraints.is_empty());
        assert!(s.turns.is_empty());
        assert!(s.produced.commit_hint.is_none());
        // Und rueckwaerts: die M5-Felder fehlen in altem JSON und muessen
        // schweigend auf ihren Default fallen.
        assert!(s.lineage.is_none());
        assert!(s.edges.is_empty());
    }

    #[test]
    fn role_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
    }

    // --- M5: Hash-Stabilitaet ------------------------------------------------

    #[test]
    fn additive_fields_do_not_change_canonical_form() {
        // Der wichtigste Test dieses Commits. Die `SessionId` ist der Hash der
        // kanonischen Form; taucht auch nur ein neuer Schluessel auf, aendern
        // sich saemtliche bereits vergebenen IDs — und jeder Trailer in der
        // Historie zeigt ins Leere.
        let s = sample();
        let json = crate::to_canonical_string(&s).unwrap();

        for key in ["lineage", "edges", "parent", "at", "effect"] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "unbelegtes M5-Feld {key:?} darf nicht serialisiert werden"
            );
        }
    }

    #[test]
    fn belegte_felder_aendern_den_hash_sehr_wohl() {
        // Die Kehrseite: Wo Herkunft *da* ist, gehoert sie in die Identitaet.
        // Zwei Sessions mit gleichem Verlauf, aber verschiedenen Sub-Agenten,
        // sind nicht dieselbe Session und duerfen nicht dedupliziert werden.
        let plain = crate::to_canonical_json(&sample()).unwrap();

        let mut with_lineage = sample();
        with_lineage.lineage = Some(Lineage::new("31f3f224-f440-41ac"));
        let other = crate::to_canonical_json(&with_lineage).unwrap();

        assert_ne!(plain, other);
    }

    #[test]
    fn lineage_and_edges_roundtrip() {
        let mut s = sample();
        s.lineage = Some(Lineage {
            local_id: "31f3f224-f440-41ac".into(),
            started_at: Some("2026-07-23T09:12:04.512Z".into()),
            ended_at: Some("2026-07-23T09:31:57.004Z".into()),
            cwd: Some("/home/anna/projects/minds".into()),
        });
        s.edges.push(Edge {
            kind: EdgeKind::SpawnedBy,
            to: Endpoint::Session {
                agent: "claude-code".into(),
                local_id: "a53626b".into(),
            },
            evidence: EvidenceMark::of(EvidenceSource::Observed),
        });
        s.turns[1].parent = Some(0);
        s.turns[1].at = Some("2026-07-23T09:12:09.001Z".into());
        s.turns[1].tool_calls[0].effect = Some(Effect {
            kind: EffectKind::Read,
            path: Some("src/retry.rs".into()),
            content: Some(ContentHash::from_bytes([7u8; 32])),
        });

        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
