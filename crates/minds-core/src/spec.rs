//! Das Soll: ein vom Menschen signiertes Regelwerk, gegen das `minds verify`
//! einen Commit auswertet (Conformance).
//!
//! # Die Form
//!
//! Im Repository liegt `.minds/spec.json`, geschrieben von Hand:
//!
//! ```json
//! {
//!   "version": "minds-spec-v1",
//!   "rules": [
//!     {"id": "core-is-leaf", "kind": "no-dependency",
//!      "crate": "minds-core", "on": ["minds-cli", "minds-store"]},
//!     {"id": "capture-not-reader", "kind": "no-import",
//!      "paths": ["crates/minds-capture/src/**"], "import": ["minds_reader"]},
//!     {"id": "replay-policy", "kind": "json-schema",
//!      "schema": "schemas/replay.json", "files": [".minds/replay.json"]},
//!     {"id": "agent-commits", "kind": "graph", "expect": "intent",
//!      "paths": ["crates/**"]}
//!   ]
//! }
//! ```
//!
//! Gelesen wird tolerant (Leerraum, Schlüsselreihenfolge, unbekannte Felder
//! wie `description`), identifiziert und signiert wird die **kanonische**
//! Form nach RFC 8785 ([`Spec::text`]): Umformatieren ändert die Id nicht,
//! jede inhaltliche Änderung schon. `id = derive_key("minds/spec/v1", text)`.
//!
//! # Was das Soll **nicht** ist
//!
//! Kein Beleg und keine Bewertung. Belegt ist es erst mit einer
//! `ssh-sig`-Signatur im Namensraum `minds-spec` eines vertrauten Signers;
//! die Bewertung PASS/DRIFT/UNKNOWN rechnet der Reader zur Lesezeit
//! (`minds_reader::conformance`) — eine Ableitung, jederzeit nachrechenbar.
//!
//! Eine Regel, deren Art oder Form dieser Version unbekannt ist, macht das
//! Soll nicht ungültig: Sie wird als [`RuleKind::Unsupported`] gelesen und
//! ergibt UNKNOWN — nie still PASS.

use serde_json::Value;

use crate::ContentHash;

/// Die Versionskennung im Feld `version`.
pub const SPEC_VERSION: &str = "minds-spec-v1";

/// Wo das Soll im Repository liegt.
pub const SPEC_PATH: &str = ".minds/spec.json";

/// Höchstgröße des Solls — roh wie kanonisch.
pub const MAX_SPEC: usize = 256 * 1024;

/// Höchstzahl der Regeln.
pub const MAX_RULES: usize = 256;

/// Höchstzahl der Einträge einer Liste in einer Regel (Globs, Crates,
/// Importe).
pub const MAX_LIST: usize = 64;

/// Höchstlänge eines Werts in einer Regel.
pub const MAX_VALUE: usize = 512;

/// Hash-Kontext der Id.
const CTX_SPEC: &str = "minds/spec/v1";

/// Die Versionskennung des signierten Umschlags.
pub const SIGNED_VERSION: &str = "minds-spec-signed-v1";

/// Hash-Kontext der Pins (Inhalte, die das Soll nur per Pfad nennt).
const CTX_PIN: &str = "minds/spec/v1/pin";

/// Höchstzahl der Pins.
pub const MAX_PINS: usize = MAX_RULES;

/// Warum ein Dokument kein Soll ist. Nennt nie Inhalte des Dokuments.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    /// Größer als [`MAX_SPEC`].
    #[error("the spec is larger than {MAX_SPEC} bytes")]
    TooLarge,
    /// Kein JSON-Objekt.
    #[error("the spec is not a JSON object")]
    NotJson,
    /// `version` fehlt oder ist nicht [`SPEC_VERSION`].
    #[error("the spec is not {SPEC_VERSION}")]
    Version,
    /// `rules` fehlt, ist keine Liste oder zu lang.
    #[error("`rules` must be a list of at most {MAX_RULES} rules")]
    Rules,
    /// Eine Regel ist kein Objekt oder hat keine gültige `id`.
    #[error("rule {0} has no valid `id` (letters, digits, `.`, `_`, `-`; at most 64)")]
    RuleId(usize),
    /// Zwei Regeln tragen dieselbe `id`.
    #[error("rule {0} repeats an earlier `id`")]
    DuplicateId(usize),
    /// Die kanonische Form ist nicht darstellbar (Zahl außerhalb von JCS).
    #[error("the spec has no canonical form: {0}")]
    Canonical(String),
    /// Lesbar, aber nicht in kanonischer Form (der Store legt nur
    /// kanonische Formen ab).
    #[error("the spec is not in canonical form")]
    NotCanonical,
    /// Kein gültiger signierter Umschlag (`minds-spec-signed-v1`).
    #[error("not a {SIGNED_VERSION} document")]
    Signed,
}

/// Die Ebene eines Graph-Erwartung: was jede Session eines Commits mitbringen
/// muss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Der Commit trägt mindestens eine minds-Session.
    Session,
    /// Jede Session des Commits ist an einen Intent-Anker gebunden.
    Intent,
}

impl Expect {
    /// Der Name im Soll.
    pub fn as_str(self) -> &'static str {
        match self {
            Expect::Session => "session",
            Expect::Intent => "intent",
        }
    }
}

/// Was eine Regel verlangt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKind {
    /// Architektur: Die Crate `krate` hängt (auch über andere Crates des
    /// Workspaces) von keiner Crate aus `on` ab. Gezählt werden
    /// `[dependencies]`, `[build-dependencies]` und ihre `target`-Formen,
    /// nicht `[dev-dependencies]`.
    NoDependency {
        /// Paketname der Crate.
        krate: String,
        /// Verbotene Paketnamen.
        on: Vec<String>,
    },
    /// Architektur auf Modulebene: Keine Datei unter `paths` nennt einen
    /// Pfad aus `import` (`minds_reader`, `crate::evidence`).
    NoImport {
        /// Globs der geprüften Dateien.
        paths: Vec<String>,
        /// Verbotene Pfade.
        import: Vec<String>,
    },
    /// Datenvertrag: Jede Datei unter `files` erfüllt das JSON Schema in
    /// `schema` (beides aus demselben Commit).
    JsonSchema {
        /// Repo-relativer Pfad des Schemas.
        schema: String,
        /// Globs der geprüften Dateien.
        files: Vec<String>,
    },
    /// Erwartete Beziehung im minds-Graphen für Commits, die etwas unter
    /// `paths` ändern (leer: jeder Commit).
    Graph {
        /// Was erwartet wird.
        expect: Expect,
        /// Globs, die die Regel auslösen.
        paths: Vec<String>,
    },
    /// Art oder Form unbekannt — ergibt immer UNKNOWN.
    Unsupported {
        /// Die Art, wie sie im Soll steht (geprüft einzeilig, gekürzt).
        kind: String,
        /// Was fehlt oder nicht passt.
        reason: &'static str,
    },
}

impl RuleKind {
    /// Der Name der Art im Soll.
    pub fn name(&self) -> &str {
        match self {
            RuleKind::NoDependency { .. } => "no-dependency",
            RuleKind::NoImport { .. } => "no-import",
            RuleKind::JsonSchema { .. } => "json-schema",
            RuleKind::Graph { .. } => "graph",
            RuleKind::Unsupported { kind, .. } => kind,
        }
    }
}

/// Eine Regel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Eindeutig im Soll.
    pub id: String,
    /// Was sie verlangt.
    pub kind: RuleKind,
}

/// Ein gelesenes Soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    text: String,
    id: ContentHash,
    rules: Vec<Rule>,
}

impl Spec {
    /// Liest ein Soll tolerant und bildet seine kanonische Form.
    pub fn parse(bytes: &[u8]) -> Result<Self, SpecError> {
        if bytes.len() > MAX_SPEC {
            return Err(SpecError::TooLarge);
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| SpecError::NotJson)?;
        let Value::Object(object) = &value else {
            return Err(SpecError::NotJson);
        };
        if object.get("version").and_then(Value::as_str) != Some(SPEC_VERSION) {
            return Err(SpecError::Version);
        }
        let Some(Value::Array(list)) = object.get("rules") else {
            return Err(SpecError::Rules);
        };
        if list.len() > MAX_RULES {
            return Err(SpecError::Rules);
        }
        let mut rules: Vec<Rule> = Vec::with_capacity(list.len());
        for (index, rule) in list.iter().enumerate() {
            let rule = read_rule(index, rule)?;
            if rules.iter().any(|seen| seen.id == rule.id) {
                return Err(SpecError::DuplicateId(index));
            }
            rules.push(rule);
        }
        let text = crate::to_canonical_string(&value)
            .map_err(|err| SpecError::Canonical(err.to_string()))?;
        if text.len() > MAX_SPEC {
            return Err(SpecError::TooLarge);
        }
        let id = Self::id_of_text(&text);
        Ok(Self { text, id, rules })
    }

    /// Liest eine kanonische Form zurück — strikt: Was nicht schon kanonisch
    /// ist, ist es nicht (der Store legt nur kanonische Formen ab).
    pub fn from_canonical(text: &str) -> Result<Self, SpecError> {
        let spec = Self::parse(text.as_bytes())?;
        if spec.text != text {
            return Err(SpecError::NotCanonical);
        }
        Ok(spec)
    }

    /// Die Identität einer kanonischen Form.
    pub fn id_of_text(text: &str) -> ContentHash {
        ContentHash::from_bytes(blake3::derive_key(CTX_SPEC, text.as_bytes()))
    }

    /// Die kanonische Form — genau diese Bytes werden signiert.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `derive_key("minds/spec/v1", text)`.
    pub fn id(&self) -> &ContentHash {
        &self.id
    }

    /// Die Regeln in der Reihenfolge des Solls.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Jeder Schlüssel und jeder String-Wert der kanonischen Form, unescapt —
    /// für die Redaction-Prüfung Feld für Feld (ein mehrzeiliger Wert steht
    /// in der kanonischen Form nur escapt).
    pub fn strings(&self) -> Vec<String> {
        fn walk(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::String(text) => out.push(text.clone()),
                Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
                Value::Object(fields) => {
                    for (key, item) in fields {
                        out.push(key.clone());
                        walk(item, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Ok(value) = serde_json::from_str::<Value>(&self.text) {
            walk(&value, &mut out);
        }
        out
    }
}

/// Der Hash, mit dem ein Pin eine Datei festhält:
/// `b3-` + `derive_key("minds/spec/v1/pin", bytes)` in Hex.
pub fn pin_of(bytes: &[u8]) -> String {
    let hash = blake3::derive_key(CTX_PIN, bytes);
    let mut out = String::with_capacity(67);
    out.push_str("b3-");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Der Verzeichnisname eines Umschlags im Soll-Ref (`envelopes/<name>`):
/// 64 Hex-Zeichen über seine kanonische Form.
pub fn envelope_name(text: &str) -> String {
    blake3::derive_key("minds/spec/v1/envelope", text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Der Dateiname einer Signatur im Soll-Ref (`sigs/<name>`): 64 Hex-Zeichen
/// über ihre Bytes — so verdrängt keine Signatur eine andere.
pub fn signature_name(signature: &str) -> String {
    blake3::derive_key("minds/spec/v1/signature", signature.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Die Dateien, die ein Soll nur per Pfad nennt, deren **Inhalt** aber zum
/// Soll gehört: die Schemas der `json-schema`-Regeln. Ein Schema, das der
/// Agent im Commit lockern könnte, wäre keine Regel.
pub fn pinned_paths(spec: &Spec) -> Vec<String> {
    let mut paths: Vec<String> = spec
        .rules()
        .iter()
        .filter_map(|rule| match &rule.kind {
            RuleKind::JsonSchema { schema, .. } => Some(schema.clone()),
            _ => None,
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Das signierte Soll: das Soll **und** die Hashes der Dateien, die es nur
/// per Pfad nennt ([`pinned_paths`]) — signiert wird dieser Umschlag, nicht
/// die Datei allein.
///
/// ```text
/// {"at":"<commit>","pins":{"schemas/p.json":"b3-…"},"spec":{…},
///  "version":"minds-spec-signed-v1"}
/// ```
///
/// `at` ist der Commit, dessen Fassung signiert wurde (HEAD beim Signieren).
/// Er bindet die Signatur an **diese** Historie und an einen Zeitpunkt in
/// ihr: Ein Soll, das anderswo signiert wurde, oder ein älteres Soll, das
/// nach einem neueren wieder auftaucht, ist kein gültiger Übergang
/// (`minds_reader::conformance`).
///
/// Die Id bleibt die des Solls ([`Spec::id`]): Der Store findet den
/// Umschlag über das Soll im Commit. Wird ein Schema geändert oder soll ein
/// älteres Soll wieder gelten, signiert der Mensch es neu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedSpec {
    spec: Spec,
    pins: std::collections::BTreeMap<String, String>,
    at: String,
    text: String,
}

impl SignedSpec {
    /// Baut den Umschlag. Jeder Pin-Pfad ist ein Repository-Pfad, jeder
    /// Wert die Form von [`pin_of`].
    pub fn new(
        spec: Spec,
        pins: std::collections::BTreeMap<String, String>,
        at: &str,
    ) -> Result<Self, SpecError> {
        let hex = |s: &str| {
            matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        if !hex(at) {
            return Err(SpecError::Signed);
        }
        // Genau die Dateien, die das Soll nur per Pfad nennt — ein
        // fehlender Pin fiele sonst erst beim Prüfen auf.
        if !pins.keys().eq(pinned_paths(&spec).iter()) {
            return Err(SpecError::Signed);
        }
        if pins.len() > MAX_PINS
            || pins
                .iter()
                .any(|(path, pin)| !repo_path(path) || path.len() > MAX_VALUE || !valid_pin(pin))
        {
            return Err(SpecError::Signed);
        }
        let inner: Value = serde_json::from_str(spec.text()).map_err(|_| SpecError::Signed)?;
        let value = serde_json::json!({
            "at": at,
            "pins": pins,
            "spec": inner,
            "version": SIGNED_VERSION,
        });
        let text = crate::to_canonical_string(&value)
            .map_err(|err| SpecError::Canonical(err.to_string()))?;
        if text.len() > MAX_SPEC * 2 {
            return Err(SpecError::TooLarge);
        }
        Ok(Self {
            spec,
            pins,
            at: at.to_owned(),
            text,
        })
    }

    /// Liest einen Umschlag zurück — strikt kanonisch.
    pub fn parse(text: &str) -> Result<Self, SpecError> {
        if text.len() > MAX_SPEC * 2 {
            return Err(SpecError::TooLarge);
        }
        let value: Value = serde_json::from_str(text).map_err(|_| SpecError::Signed)?;
        let Value::Object(fields) = value else {
            return Err(SpecError::Signed);
        };
        if fields.len() != 4
            || fields.get("version").and_then(Value::as_str) != Some(SIGNED_VERSION)
        {
            return Err(SpecError::Signed);
        }
        let inner = fields.get("spec").ok_or(SpecError::Signed)?;
        let inner = crate::to_canonical_string(inner)
            .map_err(|err| SpecError::Canonical(err.to_string()))?;
        let spec = Spec::from_canonical(&inner)?;
        let Some(Value::Object(raw)) = fields.get("pins") else {
            return Err(SpecError::Signed);
        };
        let mut pins = std::collections::BTreeMap::new();
        for (path, pin) in raw {
            pins.insert(
                path.clone(),
                pin.as_str().ok_or(SpecError::Signed)?.to_owned(),
            );
        }
        let at = fields
            .get("at")
            .and_then(Value::as_str)
            .ok_or(SpecError::Signed)?;
        let signed = Self::new(spec, pins, at)?;
        if signed.text != text {
            return Err(SpecError::Signed);
        }
        Ok(signed)
    }

    /// Das Soll.
    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Pfad → [`pin_of`] der Fassung, die signiert wurde.
    pub fn pins(&self) -> &std::collections::BTreeMap<String, String> {
        &self.pins
    }

    /// Der Commit (Hex), dessen Fassung signiert wurde.
    pub fn at(&self) -> &str {
        &self.at
    }

    /// Die kanonische Form des Umschlags — genau diese Bytes werden signiert.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Die Id des Solls darin.
    pub fn id(&self) -> &ContentHash {
        self.spec.id()
    }

    /// Jeder Schlüssel und String-Wert des Umschlags (Soll und Pins) — für
    /// die Redaction-Prüfung.
    pub fn strings(&self) -> Vec<String> {
        let mut out = self.spec.strings();
        for (path, pin) in &self.pins {
            out.push(path.clone());
            out.push(pin.clone());
        }
        out
    }
}

/// `b3-` und 64 Hex-Zeichen (klein).
fn valid_pin(pin: &str) -> bool {
    pin.strip_prefix("b3-").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Liest eine Regel. Nur eine fehlende oder doppelte `id` macht das ganze
/// Soll ungültig — alles andere wird zu [`RuleKind::Unsupported`].
fn read_rule(index: usize, rule: &Value) -> Result<Rule, SpecError> {
    let Value::Object(fields) = rule else {
        return Err(SpecError::RuleId(index));
    };
    let id = fields
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or(SpecError::RuleId(index))?
        .to_owned();
    let kind = match fields.get("kind").and_then(Value::as_str) {
        Some("no-dependency") => no_dependency(fields),
        Some("no-import") => no_import(fields),
        Some("json-schema") => json_schema(fields),
        Some("graph") => graph(fields),
        Some(other) => Err(("unknown rule kind", other.to_owned())),
        None => Err(("missing `kind`", String::new())),
    };
    let kind = kind.unwrap_or_else(|(reason, kind)| RuleKind::Unsupported {
        kind: shown_kind(&kind, fields),
        reason,
    });
    Ok(Rule { id, kind })
}

type Read<T> = Result<T, (&'static str, String)>;

fn no_dependency(fields: &serde_json::Map<String, Value>) -> Read<RuleKind> {
    let krate = string(fields, "crate", "no-dependency")?;
    let on = list(fields, "on", "no-dependency", false)?;
    Ok(RuleKind::NoDependency { krate, on })
}

fn no_import(fields: &serde_json::Map<String, Value>) -> Read<RuleKind> {
    let paths = globs(fields, "paths", "no-import", false)?;
    let import = list(fields, "import", "no-import", false)?;
    if import.iter().any(|path| !valid_import(path)) {
        return Err((
            "`import` must be Rust paths like `crate::a`",
            "no-import".into(),
        ));
    }
    Ok(RuleKind::NoImport { paths, import })
}

fn json_schema(fields: &serde_json::Map<String, Value>) -> Read<RuleKind> {
    let schema = string(fields, "schema", "json-schema")?;
    if !repo_path(&schema) {
        return Err(("`schema` must be a repository path", "json-schema".into()));
    }
    let files = globs(fields, "files", "json-schema", false)?;
    Ok(RuleKind::JsonSchema { schema, files })
}

fn graph(fields: &serde_json::Map<String, Value>) -> Read<RuleKind> {
    let expect = match fields.get("expect").and_then(Value::as_str) {
        Some("session") => Expect::Session,
        Some("intent") => Expect::Intent,
        _ => return Err(("`expect` must be `session` or `intent`", "graph".into())),
    };
    let paths = globs(fields, "paths", "graph", true)?;
    Ok(RuleKind::Graph { expect, paths })
}

fn string(fields: &serde_json::Map<String, Value>, key: &str, kind: &str) -> Read<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| valid_value(value))
        .map(str::to_owned)
        .ok_or_else(|| (missing(key), kind.to_owned()))
}

/// Eine Liste von Werten; ein einzelner String gilt als Liste mit einem
/// Eintrag.
fn list(
    fields: &serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
    optional: bool,
) -> Read<Vec<String>> {
    let values: Vec<&Value> = match fields.get(key) {
        None if optional => return Ok(Vec::new()),
        Some(Value::String(_)) => vec![&fields[key]],
        Some(Value::Array(items)) if items.is_empty() && optional => return Ok(Vec::new()),
        Some(Value::Array(items)) if !items.is_empty() && items.len() <= MAX_LIST => {
            items.iter().collect()
        }
        _ => return Err((missing(key), kind.to_owned())),
    };
    values
        .into_iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| valid_value(value))
                .map(str::to_owned)
                .ok_or_else(|| (missing(key), kind.to_owned()))
        })
        .collect()
}

fn globs(
    fields: &serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
    optional: bool,
) -> Read<Vec<String>> {
    let globs = list(fields, key, kind, optional)?;
    if globs
        .iter()
        .any(|glob| glob.starts_with('/') || glob.split('/').any(|part| part == ".."))
    {
        return Err(("globs must stay inside the repository", kind.to_owned()));
    }
    Ok(globs)
}

/// Die feste Meldung zu einem fehlenden oder ungültigen Feld.
fn missing(key: &str) -> &'static str {
    match key {
        "crate" => "missing or invalid `crate`",
        "on" => "missing or invalid `on`",
        "paths" => "missing or invalid `paths`",
        "import" => "missing or invalid `import`",
        "schema" => "missing or invalid `schema`",
        "files" => "missing or invalid `files`",
        _ => "missing or invalid field",
    }
}

/// Die Art für die Anzeige einer nicht lesbaren Regel: nur, wenn sie eine
/// harmlose Kennung ist.
fn shown_kind(kind: &str, fields: &serde_json::Map<String, Value>) -> String {
    let kind = if kind.is_empty() {
        fields.get("kind").and_then(Value::as_str).unwrap_or("")
    } else {
        kind
    };
    if valid_id(kind) {
        kind.to_owned()
    } else {
        "?".to_owned()
    }
}

/// `id` und angezeigte Kennungen: `[A-Za-z0-9._-]{1,64}`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Ein Wert einer Regel: nicht leer, begrenzt, einzeilig, ohne Steuer- und
/// Formatzeichen (die Ausgabe von `verify` zeigt ihn).
fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_VALUE
        && crate::attest::check_single_line("spec", value).is_ok()
}

/// Ein Rust-Pfad: Segmente aus Bezeichnern, getrennt durch `::`.
fn valid_import(path: &str) -> bool {
    path.split("::").all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// Ein repo-relativer Pfad ohne `..`, ohne führendes `/`, ohne leere
/// Segmente.
fn repo_path(path: &str) -> bool {
    !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"{
        "version": "minds-spec-v1",
        "description": "Architektur von minds",
        "rules": [
            {"id": "core-is-leaf", "kind": "no-dependency", "crate": "minds-core", "on": "minds-cli"},
            {"id": "no-reader", "kind": "no-import", "paths": ["crates/a/**"], "import": ["minds_reader"]},
            {"id": "policy", "kind": "json-schema", "schema": "schemas/p.json", "files": [".minds/p.json"]},
            {"id": "agents", "kind": "graph", "expect": "intent"}
        ]
    }"#;

    #[test]
    fn reads_all_rule_kinds() {
        let spec = Spec::parse(SPEC.as_bytes()).unwrap();
        let kinds: Vec<&str> = spec.rules().iter().map(|r| r.kind.name()).collect();
        assert_eq!(
            kinds,
            ["no-dependency", "no-import", "json-schema", "graph"]
        );
        assert_eq!(
            spec.rules()[0].kind,
            RuleKind::NoDependency {
                krate: "minds-core".into(),
                on: vec!["minds-cli".into()],
            }
        );
        assert_eq!(
            spec.rules()[3].kind,
            RuleKind::Graph {
                expect: Expect::Intent,
                paths: vec![],
            }
        );
    }

    /// Tolerant lesen, kanonisch identifizieren: Umformatieren und
    /// Umsortieren ändert die Id nicht, ein Wert schon.
    #[test]
    fn id_follows_content_not_formatting() {
        let a = Spec::parse(SPEC.as_bytes()).unwrap();
        let compact: Value = serde_json::from_str(SPEC).unwrap();
        let b = Spec::parse(serde_json::to_string(&compact).unwrap().as_bytes()).unwrap();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.text(), b.text());
        let changed = SPEC.replace("minds-cli", "minds-store");
        assert_ne!(Spec::parse(changed.as_bytes()).unwrap().id(), a.id());
        // Die kanonische Form liest sich strikt zurück, die rohe nicht.
        assert_eq!(Spec::from_canonical(a.text()).unwrap(), a);
        assert!(Spec::from_canonical(SPEC).is_err());
    }

    /// Eingefrorene kanonische Form und Id.
    #[test]
    fn golden_id() {
        let spec = Spec::parse(
            br#"{"rules":[{"expect":"session","id":"s","kind":"graph"}],"version":"minds-spec-v1"}"#,
        )
        .unwrap();
        assert_eq!(
            spec.text(),
            r#"{"rules":[{"expect":"session","id":"s","kind":"graph"}],"version":"minds-spec-v1"}"#
        );
        assert_eq!(
            spec.id().hex(),
            "440fb09cb71db56b4984d30896d585d3d5936d1a77f06b1fe138cffee26d460b"
        );
    }

    /// Unbekannt oder unvollständig: Die Regel bleibt, als UNKNOWN-Fall.
    #[test]
    fn unknown_and_malformed_rules_stay_unsupported() {
        let spec = Spec::parse(
            br#"{"version":"minds-spec-v1","rules":[
                {"id":"a","kind":"story"},
                {"id":"b","kind":"no-dependency","crate":"x"},
                {"id":"c","kind":"json-schema","schema":"../etc/passwd","files":"*.json"},
                {"id":"d","kind":"no-import","paths":"src/**","import":"use x"},
                {"id":"e","kind":"graph","expect":"review"},
                {"id":"f","kind":"x\u001b[31m"}
            ]}"#,
        )
        .unwrap();
        for rule in spec.rules() {
            assert!(
                matches!(rule.kind, RuleKind::Unsupported { .. }),
                "{}",
                rule.id
            );
        }
        assert_eq!(spec.rules()[0].kind.name(), "story");
        assert_eq!(spec.rules()[5].kind.name(), "?");
    }

    const AT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Der signierte Umschlag: kanonisch, strikt zurückgelesen, Pins geprüft.
    #[test]
    fn signed_envelope_roundtrips_and_pins_schemas() {
        let spec = Spec::parse(SPEC.as_bytes()).unwrap();
        assert_eq!(pinned_paths(&spec), ["schemas/p.json"]);
        let pins = std::collections::BTreeMap::from([("schemas/p.json".to_owned(), pin_of(b"{}"))]);
        let signed = SignedSpec::new(spec.clone(), pins.clone(), AT).unwrap();
        assert_eq!(signed.id(), spec.id());
        assert_eq!(signed.at(), AT);
        assert!(
            signed
                .text()
                .starts_with(&format!(r#"{{"at":"{AT}","pins":{{"schemas/p.json":"b3-"#))
        );
        assert_eq!(SignedSpec::parse(signed.text()).unwrap(), signed);
        // Andere Pins: anderer Text, dieselbe Id.
        let other = SignedSpec::new(
            spec.clone(),
            std::collections::BTreeMap::from([("schemas/p.json".to_owned(), pin_of(b"true"))]),
            AT,
        )
        .unwrap();
        assert_ne!(other.text(), signed.text());
        assert_eq!(other.id(), signed.id());
        // Nicht kanonisch, falsche Pins, fremde Felder: kein Umschlag.
        let spaced = signed.text().replacen(':', ": ", 1);
        assert!(SignedSpec::parse(&spaced).is_err());
        for bad in ["b3-xyz", "sha1-00", ""] {
            let pins = std::collections::BTreeMap::from([("a".to_owned(), bad.to_owned())]);
            assert!(SignedSpec::new(spec.clone(), pins, AT).is_err());
        }
        let pins = std::collections::BTreeMap::from([("../a".to_owned(), pin_of(b""))]);
        assert!(SignedSpec::new(spec.clone(), pins, AT).is_err());
        assert!(SignedSpec::new(spec.clone(), Default::default(), "HEAD").is_err());
        let extra = signed.text().replacen("{", r#"{"extra":1,"#, 1);
        assert!(SignedSpec::parse(&extra).is_err());
    }

    #[test]
    fn optional_empty_paths_and_absolute_globs() {
        let spec = Spec::parse(
            br#"{"version":"minds-spec-v1","rules":[
                {"id":"a","kind":"graph","expect":"session","paths":[]},
                {"id":"b","kind":"graph","expect":"session","paths":"/crates/**"}
            ]}"#,
        )
        .unwrap();
        assert!(matches!(spec.rules()[0].kind, RuleKind::Graph { .. }));
        assert!(matches!(spec.rules()[1].kind, RuleKind::Unsupported { .. }));
    }

    #[test]
    fn rejects_what_is_not_a_spec() {
        let cases: [(&[u8], SpecError); 6] = [
            (b"[]", SpecError::NotJson),
            (b"{", SpecError::NotJson),
            (
                br#"{"version":"minds-spec-v2","rules":[]}"#,
                SpecError::Version,
            ),
            (br#"{"version":"minds-spec-v1"}"#, SpecError::Rules),
            (
                br#"{"version":"minds-spec-v1","rules":[{"id":"a b"}]}"#,
                SpecError::RuleId(0),
            ),
            (
                br#"{"version":"minds-spec-v1","rules":[{"id":"a"},{"id":"a"}]}"#,
                SpecError::DuplicateId(1),
            ),
        ];
        for (bytes, error) in cases {
            assert_eq!(Spec::parse(bytes).unwrap_err(), error);
        }
        let big = vec![b' '; MAX_SPEC + 1];
        assert_eq!(Spec::parse(&big).unwrap_err(), SpecError::TooLarge);
    }
}
