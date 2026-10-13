//! Datenvertrag `json-schema`: Dateien eines Commits gegen ein JSON Schema —
//! ohne neue Abhängigkeit, mit einem bewusst kleinen Teil von Draft 2020-12.
//!
//! Das Schema liegt im Commit, gilt aber nur in der Fassung, die der Mensch
//! signiert hat: Der signierte Umschlag trägt seinen Hash (Pin). Weicht das
//! Schema im Commit davon ab, ist das DRIFT — ein gelockertes Schema wäre
//! sonst ein gelockerter Vertrag ohne Signatur.
//!
//! Verstanden werden `type` (auch als Liste), `enum`, `const`,
//! `properties`, `required`, `additionalProperties` (bool oder Schema),
//! `items` (ein Schema), `minItems`/`maxItems`, `minLength`/`maxLength`,
//! `minimum`/`maximum`, `exclusiveMinimum`/`exclusiveMaximum`,
//! `allOf`/`anyOf`/`oneOf`/`not`, lokale `$ref` (`#/$defs/…`,
//! `#/definitions/…`) und die reinen Anmerkungen. Jedes andere
//! Schlüsselwort — und ein verstandenes mit unerwarteter Wertform — macht
//! das Ergebnis UNKNOWN: Ein Validator, der Unverstandenes übergeht,
//! meldete PASS für Verträge, die er nie geprüft hat. Tiefe und Schritte
//! sind gedeckelt, die Schritte über alle Dateien und Regeln eines Berichts.

use std::cell::Cell;

use serde_json::Value;

use super::{Judged, MAX_JSON, Outcome, Snapshot};
use crate::text::sanitize_path;
use minds_core::spec::pin_of as pin;

/// Höchste Verschachtelung von Schema-Anwendungen (auch über `$ref`).
const MAX_DEPTH: usize = 64;

/// Was eine Prüfung ergibt.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Check {
    Valid,
    /// Erste Verletzung: JSON-Pointer und Schlüsselwort.
    Invalid(String),
    /// Nicht entscheidbar, mit Grund.
    Unsupported(String),
}

pub(super) fn json_schema(
    snapshot: &Snapshot<'_>,
    schema_path: &str,
    pinned: Option<&str>,
    files: &[String],
) -> Judged {
    let shown = sanitize_path(schema_path);
    let schema = match snapshot.read_path(schema_path, MAX_JSON) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (Outcome::Drift, format!("{shown} is not in this commit")),
        Err(reason) => return (Outcome::Unknown, reason),
    };
    match pinned {
        None => {
            return (
                Outcome::Unknown,
                format!("{shown} is not pinned by a checked signature"),
            );
        }
        Some(expected) if pin(&schema) != expected => {
            return (
                Outcome::Drift,
                format!("{shown} changed since the spec was signed"),
            );
        }
        Some(_) => {}
    }
    let Ok(schema) = serde_json::from_slice::<Value>(&schema) else {
        return (Outcome::Unknown, format!("{shown} is not JSON"));
    };
    let targets = match snapshot.matching(files) {
        Ok(targets) => targets,
        Err(reason) => return (Outcome::Unknown, reason),
    };
    if targets.is_empty() {
        return (Outcome::Unknown, "no file matches".into());
    }
    let mut unsupported: Option<String> = None;
    for (path, blob) in &targets {
        let bytes = match snapshot.read(path, *blob, MAX_JSON) {
            Ok(bytes) => bytes,
            Err(reason) => return (Outcome::Unknown, reason),
        };
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            return (Outcome::Drift, format!("{}: not JSON", sanitize_path(path)));
        };
        match validate_with(&schema, &document, &snapshot.steps) {
            Check::Valid => {}
            Check::Invalid(at) => {
                return (Outcome::Drift, format!("{}: {at}", sanitize_path(path)));
            }
            Check::Unsupported(reason) => {
                if snapshot.steps.get() == 0 {
                    return (Outcome::Unknown, reason);
                }
                unsupported.get_or_insert(reason);
            }
        }
    }
    match unsupported {
        Some(reason) => (Outcome::Unknown, reason),
        None => (Outcome::Pass, format!("{} file(s) valid", targets.len())),
    }
}

/// Prüft `document` gegen `schema` mit eigenem Schrittbudget (Tests).
#[cfg(test)]
pub(crate) fn validate(schema: &Value, document: &Value) -> Check {
    validate_with(schema, document, &Cell::new(1_000_000))
}

/// Prüft `document` gegen `schema`; `steps` wird verbraucht.
pub(crate) fn validate_with(schema: &Value, document: &Value, steps: &Cell<u64>) -> Check {
    let mut validator = Validator {
        root: schema,
        steps,
        unsupported: None,
        tainted: false,
        combinator: 0,
    };
    let verdict = validator.check(schema, document, "", 0);
    match (verdict, validator.unsupported) {
        (Ok(()), None) => Check::Valid,
        (Ok(()), Some(reason)) => Check::Unsupported(reason),
        // Eine Verletzung aus verstandenen Schlüsselwörtern bleibt eine —
        // außer Unverstandenes lag in einem Kombinator oder ein Deckel riss
        // (dann ist nichts sicher).
        (Err(at), Some(reason)) if validator.tainted => {
            let _ = at;
            Check::Unsupported(reason)
        }
        (Err(at), _) => Check::Invalid(at),
    }
}

struct Validator<'s, 'b> {
    root: &'s Value,
    steps: &'b Cell<u64>,
    unsupported: Option<String>,
    /// Das Gesamturteil ist unsicher: Unverstandenes in einem Kombinator,
    /// oder ein Deckel ist gerissen.
    tainted: bool,
    /// Wie tief in `anyOf`/`oneOf`/`not` die Prüfung gerade steht.
    combinator: usize,
}

/// Anmerkungen ohne Prüfwirkung.
const ANNOTATIONS: [&str; 10] = [
    "$schema",
    "$id",
    "title",
    "description",
    "$comment",
    "default",
    "examples",
    "$defs",
    "definitions",
    "deprecated",
];

/// Was der Validator versteht.
const KEYWORDS: [&str; 20] = [
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "$ref",
];

impl<'s> Validator<'s, '_> {
    fn unsupported(&mut self, reason: String) {
        if self.combinator > 0 {
            self.tainted = true;
        }
        self.unsupported.get_or_insert(reason);
    }

    fn capped(&mut self, reason: String) {
        self.tainted = true;
        self.unsupported.get_or_insert(reason);
    }

    /// Eine nichtnegative ganze Zahl (auch `2.0`); sonst Unsupported.
    fn count(&mut self, fields: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
        let value = fields.get(key)?;
        let count = value.as_u64().or_else(|| {
            value
                .as_f64()
                .filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n <= u64::MAX as f64)
                .map(|n| n as u64)
        });
        if count.is_none() {
            self.unsupported(format!("`{key}` is not a non-negative integer"));
        }
        count
    }

    fn number(&mut self, fields: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
        let value = fields.get(key)?;
        if value.as_f64().is_none() {
            self.unsupported(format!("`{key}` is not a number"));
        }
        value.as_f64()
    }

    /// `Err(pointer: keyword)` bei der ersten Verletzung.
    fn check(
        &mut self,
        schema: &'s Value,
        value: &Value,
        at: &str,
        depth: usize,
    ) -> Result<(), String> {
        if self.steps.get() == 0 {
            self.capped("validation budget exhausted".into());
            return Ok(());
        }
        self.steps.set(self.steps.get() - 1);
        if depth > MAX_DEPTH {
            self.capped(format!("schema too deep (more than {MAX_DEPTH} levels)"));
            return Ok(());
        }
        let fields = match schema {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => return Err(fail(at, "false")),
            Value::Object(fields) => fields,
            _ => {
                self.unsupported("a schema is neither an object nor a boolean".into());
                return Ok(());
            }
        };
        for key in fields.keys() {
            if !KEYWORDS.contains(&key.as_str()) && !ANNOTATIONS.contains(&key.as_str()) {
                let shown: String = key.chars().take(40).collect();
                self.unsupported(format!(
                    "unsupported schema keyword `{}`",
                    crate::text::sanitize(&shown)
                ));
            }
        }
        if let Some(reference) = fields.get("$ref") {
            let target = reference.as_str().and_then(|r| self.resolve(r));
            match target {
                Some(target) => self.check(target, value, at, depth + 1)?,
                None => self.unsupported("a `$ref` that is not local (`#/$defs/…`)".into()),
            }
        }
        if let Some(types) = fields.get("type") {
            let allowed: Option<Vec<&str>> = match types {
                Value::String(one) => Some(vec![one.as_str()]),
                Value::Array(many) => many.iter().map(Value::as_str).collect(),
                _ => None,
            };
            match allowed {
                Some(allowed)
                    if !allowed.is_empty() && allowed.iter().all(|t| TYPES.contains(t)) =>
                {
                    if !allowed.iter().any(|t| has_type(value, t)) {
                        return Err(fail(at, "type"));
                    }
                }
                _ => self.unsupported("an unreadable `type`".into()),
            }
        }
        match fields.get("enum") {
            Some(Value::Array(options)) => {
                if !options.iter().any(|option| same(option, value)) {
                    return Err(fail(at, "enum"));
                }
            }
            Some(_) => self.unsupported("`enum` is not a list".into()),
            None => {}
        }
        if let Some(expected) = fields.get("const")
            && !same(expected, value)
        {
            return Err(fail(at, "const"));
        }
        let required: Vec<&str> = match fields.get("required") {
            Some(Value::Array(names)) => {
                let names: Option<Vec<&str>> = names.iter().map(Value::as_str).collect();
                names.unwrap_or_else(|| {
                    self.unsupported("`required` holds a non-string".into());
                    Vec::new()
                })
            }
            Some(_) => {
                self.unsupported("`required` is not a list".into());
                Vec::new()
            }
            None => Vec::new(),
        };
        let properties = match fields.get("properties") {
            Some(Value::Object(properties)) => Some(properties),
            Some(_) => {
                self.unsupported("`properties` is not an object".into());
                None
            }
            None => None,
        };
        let additional = fields.get("additionalProperties");
        if additional.is_some_and(|a| !matches!(a, Value::Bool(_) | Value::Object(_))) {
            self.unsupported("`additionalProperties` is neither a boolean nor a schema".into());
        }
        let (min_items, max_items) = (
            self.count(fields, "minItems"),
            self.count(fields, "maxItems"),
        );
        let (min_length, max_length) = (
            self.count(fields, "minLength"),
            self.count(fields, "maxLength"),
        );
        let bounds = [
            ("minimum", self.number(fields, "minimum")),
            ("maximum", self.number(fields, "maximum")),
            ("exclusiveMinimum", self.number(fields, "exclusiveMinimum")),
            ("exclusiveMaximum", self.number(fields, "exclusiveMaximum")),
        ];
        match value {
            Value::Object(object) => {
                for name in &required {
                    if !object.contains_key(*name) {
                        return Err(fail(&child(at, name), "required"));
                    }
                }
                for (name, item) in object {
                    let path = child(at, name);
                    match properties.and_then(|p| p.get(name)) {
                        Some(sub) => self.check(sub, item, &path, depth + 1)?,
                        None => match additional {
                            Some(Value::Bool(false)) => {
                                return Err(fail(&path, "additionalProperties"));
                            }
                            Some(sub @ Value::Object(_)) => {
                                self.check(sub, item, &path, depth + 1)?
                            }
                            _ => {}
                        },
                    }
                }
            }
            Value::Array(items) => {
                let length = items.len() as u64;
                if min_items.is_some_and(|min| length < min) {
                    return Err(fail(at, "minItems"));
                }
                if max_items.is_some_and(|max| length > max) {
                    return Err(fail(at, "maxItems"));
                }
                match fields.get("items") {
                    Some(sub @ (Value::Object(_) | Value::Bool(_))) => {
                        for (index, item) in items.iter().enumerate() {
                            self.check(sub, item, &child(at, &index.to_string()), depth + 1)?;
                        }
                    }
                    Some(_) => self.unsupported("`items` as a list (use `prefixItems`)".into()),
                    None => {}
                }
            }
            Value::String(text) => {
                let length = text.chars().count() as u64;
                if min_length.is_some_and(|min| length < min) {
                    return Err(fail(at, "minLength"));
                }
                if max_length.is_some_and(|max| length > max) {
                    return Err(fail(at, "maxLength"));
                }
            }
            Value::Number(_) => {
                for (keyword, bound) in bounds {
                    let Some(bound) = bound else { continue };
                    let broken = match keyword {
                        "minimum" => below(value, bound, false),
                        "maximum" => above(value, bound, false),
                        "exclusiveMinimum" => below(value, bound, true),
                        _ => above(value, bound, true),
                    };
                    match broken {
                        Some(true) => return Err(fail(at, keyword)),
                        Some(false) => {}
                        None => self.unsupported(format!(
                            "a number too large to compare exactly with `{keyword}`"
                        )),
                    }
                }
            }
            _ => {}
        }
        match fields.get("allOf") {
            Some(Value::Array(all)) => {
                for sub in all {
                    self.check(sub, value, at, depth + 1)?;
                }
            }
            Some(_) => self.unsupported("`allOf` is not a list".into()),
            None => {}
        }
        match fields.get("anyOf") {
            Some(Value::Array(any)) => {
                if self.branches(any, value, at, depth) == 0 {
                    return Err(fail(at, "anyOf"));
                }
            }
            Some(_) => self.unsupported("`anyOf` is not a list".into()),
            None => {}
        }
        match fields.get("oneOf") {
            Some(Value::Array(one)) => {
                if self.branches(one, value, at, depth) != 1 {
                    return Err(fail(at, "oneOf"));
                }
            }
            Some(_) => self.unsupported("`oneOf` is not a list".into()),
            None => {}
        }
        if let Some(sub) = fields.get("not")
            && self.branches(std::slice::from_ref(sub), value, at, depth) == 1
        {
            return Err(fail(at, "not"));
        }
        Ok(())
    }

    /// Wie viele Zweige gelten. Unverstandenes in einem Zweig macht das
    /// Gesamturteil unsicher ([`Validator::tainted`]).
    fn branches(&mut self, branches: &'s [Value], value: &Value, at: &str, depth: usize) -> usize {
        self.combinator += 1;
        let passed = branches
            .iter()
            .filter(|sub| self.check(sub, value, at, depth + 1).is_ok())
            .count();
        self.combinator -= 1;
        passed
    }

    /// `#/$defs/name` oder `#/definitions/name` (auch tiefer, JSON Pointer).
    fn resolve(&self, reference: &str) -> Option<&'s Value> {
        let pointer = reference.strip_prefix('#')?;
        if pointer.is_empty() {
            return Some(self.root);
        }
        self.root.pointer(pointer)
    }
}

/// Die Typnamen von Draft 2020-12.
const TYPES: [&str; 7] = [
    "object", "array", "string", "boolean", "null", "number", "integer",
];

fn has_type(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        _ => false,
    }
}

/// Gleichheit nach JSON Schema: Zahlen nach ihrem Wert (`1` == `1.0`),
/// ganze Zahlen exakt.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (exact(x), exact(y)) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, value)| y.get(key).is_some_and(|other| same(value, other)))
        }
        _ => a == b,
    }
}

/// Eine Zahl als exakte ganze Zahl, wenn sie eine ist.
fn exact(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
        .or_else(|| {
            number
                .as_f64()
                .filter(|n| n.fract() == 0.0 && n.abs() < 9.0e15)
                .map(|n| n as i128)
        })
}

/// `value < bound` (bzw. `<=` bei `strict`) — `None`, wenn eine ganze Zahl
/// jenseits von 2^53 nicht exakt vergleichbar ist.
fn below(value: &Value, bound: f64, strict: bool) -> Option<bool> {
    let n = comparable(value)?;
    Some(if strict { n <= bound } else { n < bound })
}

fn above(value: &Value, bound: f64, strict: bool) -> Option<bool> {
    let n = comparable(value)?;
    Some(if strict { n >= bound } else { n > bound })
}

fn comparable(value: &Value) -> Option<f64> {
    let number = value.as_number()?;
    if let Some(i) = number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
        && i.unsigned_abs() > 1 << 53
    {
        return None;
    }
    number.as_f64()
}

/// Höchstlänge eines JSON-Pointers in einer Meldung. Länger wächst er nicht
/// (`…`): Ein riesiger Schlüssel über einem großen Array kostete sonst je
/// Schritt eine Kopie seiner ganzen Länge.
const MAX_POINTER: usize = 256;

/// Ein JSON-Pointer-Segment (`~` und `/` escapt), für die Anzeige entschärft
/// und gedeckelt.
fn child(at: &str, name: &str) -> String {
    if at.len() >= MAX_POINTER {
        return at.to_owned();
    }
    let escaped: String = name
        .chars()
        .take(64)
        .collect::<String>()
        .replace('~', "~0")
        .replace('/', "~1");
    let mut out = format!("{at}/{escaped}");
    if out.len() >= MAX_POINTER {
        let mut end = MAX_POINTER - 3;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

fn fail(at: &str, keyword: &str) -> String {
    let at = if at.is_empty() { "/" } else { at };
    format!("{} violates `{keyword}`", crate::text::sanitize(at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["name", "tags"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "tags": {"type": "array", "items": {"$ref": "#/$defs/tag"}, "maxItems": 2},
                "level": {"enum": ["low", "high"]},
                "count": {"type": "integer", "minimum": 0}
            },
            "$defs": {"tag": {"type": "string", "maxLength": 3}}
        })
    }

    #[test]
    fn valid_documents_pass() {
        assert_eq!(
            validate(&schema(), &json!({"name": "a", "tags": ["x"], "count": 2})),
            Check::Valid
        );
    }

    #[test]
    fn violations_name_pointer_and_keyword() {
        let cases = [
            (json!({"tags": []}), "/name violates `required`"),
            (
                json!({"name": "", "tags": []}),
                "/name violates `minLength`",
            ),
            (
                json!({"name": "a", "tags": ["toolong"]}),
                "/tags/0 violates `maxLength`",
            ),
            (
                json!({"name": "a", "tags": [], "x": 1}),
                "/x violates `additionalProperties`",
            ),
            (
                json!({"name": "a", "tags": [], "level": "mid"}),
                "/level violates `enum`",
            ),
            (
                json!({"name": "a", "tags": [], "count": -1}),
                "/count violates `minimum`",
            ),
            (
                json!({"name": "a", "tags": [], "count": 1.5}),
                "/count violates `type`",
            ),
            (json!([]), "/ violates `type`"),
        ];
        for (document, expected) in cases {
            assert_eq!(
                validate(&schema(), &document),
                Check::Invalid(expected.into()),
                "{document}"
            );
        }
    }

    /// Unverstanden heißt UNKNOWN — nie still PASS.
    #[test]
    fn unknown_keywords_are_unsupported() {
        let schema = json!({"type": "string", "pattern": "^a"});
        assert_eq!(
            validate(&schema, &json!("b")),
            Check::Unsupported("unsupported schema keyword `pattern`".into())
        );
        let remote = json!({"$ref": "https://example.com/s.json"});
        assert!(matches!(
            validate(&remote, &json!(1)),
            Check::Unsupported(_)
        ));
        // Eine sichere Verletzung bleibt eine.
        let mixed = json!({"type": "string", "format": "email"});
        assert_eq!(
            validate(&mixed, &json!(1)),
            Check::Invalid("/ violates `type`".into())
        );
        // In einem Kombinator ist nichts sicher — auch nicht, wenn schon
        // vorher etwas unverstanden war.
        let combined = json!({"not": {"pattern": "^a"}});
        assert!(matches!(
            validate(&combined, &json!("a")),
            Check::Unsupported(_)
        ));
        let both = json!({"format": "email", "not": {"pattern": "^a"}});
        assert!(matches!(
            validate(&both, &json!("b")),
            Check::Unsupported(_)
        ));
    }

    /// Verstandene Schlüsselwörter in unerwarteter Form: UNKNOWN statt
    /// still übergangen; ganzzahlige Floats zählen als ganze Zahlen.
    #[test]
    fn malformed_keyword_values_are_unsupported() {
        for schema in [
            json!({"required": "name"}),
            json!({"enum": "x"}),
            json!({"minimum": "5"}),
            json!({"allOf": {}}),
            json!({"maxLength": -1}),
            json!({"additionalProperties": 1}),
            json!({"type": "text"}),
        ] {
            assert!(
                matches!(validate(&schema, &json!({})), Check::Unsupported(_)),
                "{schema}"
            );
        }
        assert_eq!(
            validate(&json!({"maxLength": 2.0}), &json!("toolong")),
            Check::Invalid("/ violates `maxLength`".into())
        );
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(validate(&json!({"const": 1}), &json!(1.0)), Check::Valid);
        assert_eq!(validate(&json!({"enum": [2.0]}), &json!(2)), Check::Valid);
        let big = json!({"maximum": 9007199254740992u64});
        assert!(matches!(
            validate(&big, &json!(9007199254740993u64)),
            Check::Unsupported(_)
        ));
    }

    /// Ein riesiger Schlüssel: Der Pointer bleibt kurz.
    #[test]
    fn pointers_stay_short() {
        let key = "k".repeat(100_000);
        let schema =
            json!({"additionalProperties": {"type": "array", "items": {"type": "integer"}}});
        let mut document = serde_json::Map::new();
        document.insert(key, json!([0, 1, "x"]));
        let Check::Invalid(at) = validate(&schema, &Value::Object(document)) else {
            panic!();
        };
        assert!(at.len() < 400, "{}", at.len());
    }

    #[test]
    fn combinators_and_recursion_are_bounded() {
        let one = json!({"oneOf": [{"type": "integer"}, {"type": "number"}]});
        assert_eq!(validate(&one, &json!(1.5)), Check::Valid);
        assert_eq!(
            validate(&one, &json!(1)),
            Check::Invalid("/ violates `oneOf`".into())
        );
        let looping = json!({"$ref": "#"});
        assert!(matches!(
            validate(&looping, &json!(1)),
            Check::Unsupported(_)
        ));
        // Das Schrittbudget ist geteilt: Ist es leer, ist nichts entschieden.
        let steps = Cell::new(3);
        let deep = json!({"allOf": [{"allOf": [{"allOf": [{"type": "string"}]}]}]});
        assert!(matches!(
            validate_with(&deep, &json!(1), &steps),
            Check::Unsupported(_)
        ));
        assert_eq!(steps.get(), 0);
    }
}
