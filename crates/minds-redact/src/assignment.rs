//! Kontextbasierte Detektoren: wenn nicht der *Wert* verrät, dass er geheim
//! ist, sondern seine *Umgebung*.
//!
//! [`crate::secret`] erkennt Geheimnisse an ihrer **Form** — `AKIA…` ist ein
//! AWS-Key, ein 44-Zeichen-base64-Blob ist hochentropisch. Diese Detektoren
//! haben eine harte Grenze, und sie heißt `.env`:
//!
//! ```text
//! DB_USER=admin
//! DB_PASSWORD=hunter2
//! SMTP_PASSWORD=Sommer2024!
//! JWT_SECRET=abc123
//! ```
//!
//! Kein Präfix, keine Entropie (`hunter2` hat 7 Zeichen — die Schwelle liegt
//! bei 32), kein Muster. **Form-basierte Erkennung kann das prinzipiell nicht
//! fangen.** Ein von Menschen gewähltes Passwort sieht aus wie ein Wort, weil
//! es eines ist.
//!
//! Der einzige verlässliche Hinweis ist der **Schlüsselname**: Was hinter
//! `PASSWORD=` steht, ist ein Passwort — unabhängig davon, wie es aussieht.
//! Genau darauf setzt [`KeyValueRedactor`]. Redigiert wird dabei **nur der
//! Wert**, nie der Schlüssel: Dass `DB_PASSWORD` gesetzt wurde, ist für den
//! Reviewer nützliche Information; *welcher* Wert es war, geht ihn nichts an.
//!
//! [`UrlCredentialRedactor`] deckt die zweite Stelle ab, an der die Struktur
//! das Geheimnis benennt: `postgres://admin:s3cr3t@db.internal/prod`,
//! `https://oauth2:glpat-…@gitlab.com/…`.
//!
//! # Zwei Stufen, weil `password` und `token` nicht dasselbe sind
//!
//! - **[`Tier::Strict`]** — `password`, `secret`, `credential`, `private_key`.
//!   Diese Wörter bedeuten in einer Zuweisung nur eine Sache. Deshalb **kein
//!   Längen-Filter**: `hunter2` wird redigiert, und das ist der ganze Zweck.
//! - **[`Tier::Shaped`]** — `token`, `api_key`, `session`, `cookie`.
//!   Diese Wörter stehen auch in gewöhnlicher Prosa („Token-Limit: 4096",
//!   „input_tokens: 1234") — und agent-erzeugte Transkripte sind voll davon.
//!   Hier muss der Wert zusätzlich *credential-typisch aussehen*
//!   (siehe [`has_credential_shape`]).
//!
//! # Der Preis, offen benannt
//!
//! Der Strict-Tier ist absichtlich grob. `Secret: der Reviewer liest` verliert
//! ein Wort an den Platzhalter. Das ist der Preis dafür, `DB_PASSWORD=hunter2`
//! zu fangen — und es ist der richtige Preis: Ein verlorenes Wort im Record ist
//! ein Schönheitsfehler, ein durchgerutschtes Passwort im GitLab ein Incident.
//! Wo es wirklich stört, ist die [`AllowList`](crate::AllowList) die Antwort.
//!
//! # Was diese Detektoren *nicht* fangen
//!
//! Prosa. Schreibt ein Agent „das Passwort war übrigens hunter2", steht kein
//! `=` und kein `:` dazwischen — kein Treffer. Dagegen hilft nur, dass die
//! Datei gar nicht erst im Transkript landet: siehe
//! [`crate::secretfile`].

use regex::{Captures, Match, Regex};

use crate::redactor::{Category, Finding, Redactor};

// ---------------------------------------------------------------------------
// Bausteine der Muster
// ---------------------------------------------------------------------------

/// Optionaler Rest des Schlüssels nach dem Stichwort: `PASSWORD_2`,
/// `token.file`, `api_key-alt`. Gedeckelt.
const KEY_SUFFIX: &str = r"[A-Za-z0-9_.\-]{0,32}";

/// Trennzeichen zwischen Schlüssel und Wert. Das optionale Anführungszeichen
/// davor schließt den JSON-Fall `"password": "x"` mit ein.
///
/// # Warum `[^\S\r\n]` und nicht `\s`
///
/// `\s` matcht **auch Zeilenumbrüche**. Mit `\s*` würde
///
/// ```text
/// PASSWORD=
/// DB_HOST=localhost
/// ```
///
/// den Wert der *nächsten* Zeile als Passwort lesen: erst `=`, dann `\n` als
/// Trenner, dann `DB_HOST=localhost` als Wert. Das leckt zwar nichts (es
/// schwärzt zu viel, nicht zu wenig), zerstört aber den Record und verfälscht
/// die Redaction-Zähler.
///
/// `[^\S\r\n]` ist „alles, was `\s` matcht, außer `\r` und `\n"` — horizontaler
/// Leerraum. Bewusst nicht `[ \t]`: ein exotischer Trenner (geschütztes
/// Leerzeichen aus einem kopierten Snippet) soll den Fund nicht verhindern.
/// Breiter beim Trenner heißt hier mehr Redaction, nicht weniger.
const SEPARATOR: &str = r#"["']?[^\S\r\n]*[:=][^\S\r\n]*"#;

/// Der Wert in drei Formen: doppelt gequotet, einfach gequotet, nackt.
///
/// Der nackte Wert läuft **bis zum nächsten Leerraum** — nicht bis zum nächsten
/// Komma oder Semikolon. Ein Passwort darf `,` und `;` enthalten; würde der Fund
/// dort enden, bliebe der Rest des Geheimnisses stehen. Lieber ein Komma zu viel
/// redigiert als ein Zeichen zu wenig.
///
/// `bearer `/`basic ` davor wird **mitgelesen, aber nicht mitredigiert**: Aus
/// `Authorization: Bearer eyJ…` soll `Authorization: Bearer [redacted:secret]`
/// werden, nicht `Authorization: [redacted:secret] eyJ…` — sonst bliebe der
/// Token stehen und nur das Wort „Bearer" verschwände.
///
/// # Warum hier `+` steht und keine Obergrenze
///
/// Naheliegend wäre `{1,4096}` — „gedeckelt ist sicherer". Das ist hier
/// **falsch und kostet den Build**: Die `regex`-Crate *entrollt* Zähl-
/// Wiederholungen in den Automaten. Bei einer ASCII-Klasse (wie den
/// Token-Mustern in [`crate::secret`]) ist das billig — ein Byte-Bereich pro
/// Kopie. Diese Klassen sind aber **negiert** (`[^"\r\n]`) und damit
/// Unicode-weit: Jede Kopie ist ein komplettes UTF-8-Teilautomat über 1–4
/// Bytes. Drei solcher Klassen à 4096 Kopien sprengen das Größenlimit des
/// Compilers (`CompiledTooBig`, 10 MiB) — die Regex baut schlicht nicht.
///
/// `+` kompiliert dagegen zu einer konstant großen Schleife. Ein Sicherheits-
/// verlust ist das nicht: Die Laufzeit bleibt linear (endlicher Automat, kein
/// Backtracking), und die Länge des Fundes ist ohnehin **durch die Zeile
/// begrenzt** — `\r` und `\n` sind aus allen drei Klassen ausgeschlossen.
const VALUE: &str = r#"(?:"(?P<dq>[^"\r\n]+)"|'(?P<sq>[^'\r\n]+)'|(?:bearer[^\S\r\n]+|basic[^\S\r\n]+)?(?P<bare>[^\s"'\r\n]+))"#;

/// Lazy-Präfix zwischen `--` und dem Stichwort: `--db-password`.
const CLI_PREFIX: &str = r"[A-Za-z0-9\-]{0,32}?";
/// Rest des Flag-Namens nach dem Stichwort.
const CLI_SUFFIX: &str = r"[A-Za-z0-9\-]{0,32}";

/// Wie streng der Wert geprüft wird, bevor er als Geheimnis gilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Der Schlüssel ist eindeutig — jeder nicht-ausgenommene Wert wird
    /// redigiert, egal wie kurz oder harmlos er aussieht.
    Strict,
    /// Der Schlüssel ist mehrdeutig — der Wert muss zusätzlich credential-typisch
    /// aussehen ([`has_credential_shape`]).
    Shaped,
}

/// Form der Zuweisung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `KEY=wert`, `KEY: wert`, `"key": "wert"`.
    Assignment,
    /// `--flag wert`, `--flag=wert`. Nur mit `--` davor, weil erst das
    /// Doppel-Minus erlaubt, Leerraum als Trenner zu akzeptieren, ohne dass
    /// jeder Prosa-Satz zum Treffer wird.
    CliFlag,
}

/// Eine Regel: welche Schlüsselwörter, in welcher Form, mit welcher Strenge.
struct KeyRule {
    name: &'static str,
    category: Category,
    tier: Tier,
    form: Form,
    /// Regex-Alternation der Stichwörter — **ohne** Wortgrenzen: `secret`
    /// trifft auch in `client_secret`, `password` auch in `DB_PASSWORD`.
    /// Fail-closed und spart die halbe Tabelle.
    keys: &'static str,
    /// Ob [`RedactionConfig::secret_keys`](crate::RedactionConfig::secret_keys)
    /// hier angehängt wird.
    accepts_extra_keys: bool,
}

/// Die Regeltabelle.
///
/// Bewusst **nicht** enthalten:
/// - `key` allein — träfe `monkey`, `keyboard`, `hotkey`.
/// - `auth` allein — träfe `author`, und `Author: …` steht in jedem zweiten
///   Git-Log-Ausschnitt. Deshalb nur `authorization`/`oauth`.
/// - `user` **allein** — in einem Agent-Transkript ist `User:` die *Rollenmarke*
///   jeder Nutzer-Nachricht; sie zu schwärzen würde den Record zerstören.
///   Erfasst wird stattdessen nur die **zusammengesetzte** Form
///   `[A-Za-z0-9]{1,32}[_-]user` — `DB_USER`, `MYSQL_USER`, `smtp-user`. Genau
///   das ist der Unterschied zwischen einem Feldnamen und einem Satzanfang.
/// - `hash` — träfe `hash: 356a192b…`, also jeden Git-SHA im Transkript.
/// - `url`/`dsn` — die Verbindungs-URL soll *nicht* als Ganzes verschwinden;
///   [`UrlCredentialRedactor`] schneidet chirurgisch nur die Zugangsdaten raus
///   und lässt Schema und Host stehen (nützlicher Kontext für den Reviewer).
const KEY_RULES: &[KeyRule] = &[
    KeyRule {
        name: "assignment-strict",
        category: Category::Secret,
        tier: Tier::Strict,
        form: Form::Assignment,
        keys: r"password|passwd|pwd|passphrase|secret|credential|private[_-]?key",
        accepts_extra_keys: true,
    },
    KeyRule {
        name: "assignment-shaped",
        category: Category::Secret,
        tier: Tier::Shaped,
        form: Form::Assignment,
        keys: r"token|api[_-]?key|apikey|access[_-]?key|ssh[_-]?key|signing[_-]?key|encryption[_-]?key|authorization|oauth|bearer|session|cookie|signature",
        accepts_extra_keys: false,
    },
    KeyRule {
        // Nutzernamen sind die andere Hälfte der Zugangsdaten. Kategorie `Pii`,
        // weil ein Login-Name eine Person benennt, kein Geheimnis ist.
        name: "assignment-identity",
        category: Category::Pii,
        tier: Tier::Strict,
        form: Form::Assignment,
        keys: r"user[_-]?name|login[_-]?name|login|[A-Za-z0-9]{1,32}[_-]user",
        accepts_extra_keys: false,
    },
    KeyRule {
        name: "cli-flag",
        category: Category::Secret,
        tier: Tier::Strict,
        form: Form::CliFlag,
        keys: r"password|passwd|secret|token|credential|api-key|apikey",
        accepts_extra_keys: false,
    },
];

// ---------------------------------------------------------------------------
// KeyValueRedactor
// ---------------------------------------------------------------------------

/// Kompilierte Regel.
struct CompiledKeyRule {
    name: &'static str,
    category: Category,
    tier: Tier,
    re: Regex,
}

/// Detektor für Zuweisungen mit sensiblem Schlüsselnamen — der `.env`-Fall.
///
/// Redigiert **nur den Wert**. Der Schlüssel bleibt lesbar, damit im Record
/// sichtbar bleibt, *dass* eine Zugangsdatei im Spiel war.
pub struct KeyValueRedactor {
    rules: Vec<CompiledKeyRule>,
}

impl KeyValueRedactor {
    /// Nur die eingebauten Regeln.
    ///
    /// Die Muster sind Konstanten und in den Tests abgedeckt — ein
    /// Kompilierfehler wäre ein Programmierfehler, deshalb `expect`.
    pub fn new() -> Self {
        Self::build(&[]).expect("konstante Schlüssel-Muster müssen kompilieren")
    }

    /// Eingebaute Regeln plus zusätzliche Schlüsselwörter aus der Config
    /// (z. B. `VAULT_ROLE_ID`, `DB_USER`). Die Wörter werden regex-escaped —
    /// ein Eintrag mit Sonderzeichen ist ein Literal, kein Muster.
    ///
    /// Sie landen im **Strict**-Tier: Wer einen Schlüssel bewusst einträgt,
    /// will seinen Wert weg, ohne Längen-Debatte.
    pub fn with_extra_keys<S: AsRef<str>>(keys: &[S]) -> Result<Self, regex::Error> {
        let extra: Vec<String> = keys
            .iter()
            .map(|k| k.as_ref().trim())
            .filter(|k| !k.is_empty())
            .map(regex::escape)
            .collect();
        Self::build(&extra)
    }

    fn build(extra: &[String]) -> Result<Self, regex::Error> {
        let mut rules = Vec::with_capacity(KEY_RULES.len());
        for rule in KEY_RULES {
            let mut keys = rule.keys.to_string();
            if rule.accepts_extra_keys {
                for key in extra {
                    keys.push('|');
                    keys.push_str(key);
                }
            }
            let pattern = match rule.form {
                Form::Assignment => {
                    format!("(?i)(?:{}){}{}{}", keys, KEY_SUFFIX, SEPARATOR, VALUE)
                }
                Form::CliFlag => format!(
                    r"(?i)--{}(?:{}){}(?:=|[^\S\r\n]+){}",
                    CLI_PREFIX, keys, CLI_SUFFIX, VALUE
                ),
            };
            rules.push(CompiledKeyRule {
                name: rule.name,
                category: rule.category,
                tier: rule.tier,
                re: Regex::new(&pattern)?,
            });
        }
        Ok(Self { rules })
    }

    /// Namen der aktiven Regeln (Doku, Diagnose, späterer Per-Regel-Audit).
    pub fn rule_names(&self) -> Vec<&'static str> {
        self.rules.iter().map(|r| r.name).collect()
    }
}

impl Default for KeyValueRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for KeyValueRedactor {
    fn name(&self) -> &str {
        "key-value"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        let mut out = Vec::new();
        for rule in &self.rules {
            for caps in rule.re.captures_iter(text) {
                let Some(value) = value_of(&caps) else {
                    continue;
                };
                if is_exempt(value.as_str(), rule.tier) {
                    continue;
                }
                // Capture-Spans liegen immer auf UTF-8-Grenzen und sind hier
                // nie leer (jede Wert-Alternative verlangt >= 1 Zeichen).
                out.push(Finding::new(rule.category, value.start(), value.end()));
            }
        }
        out
    }
}

/// Die Wert-Capture, egal in welcher der drei Formen sie steckt.
fn value_of<'t>(caps: &Captures<'t>) -> Option<Match<'t>> {
    caps.name("dq")
        .or_else(|| caps.name("sq"))
        .or_else(|| caps.name("bare"))
}

/// Ob ein Wert trotz sensiblem Schlüssel stehen bleiben darf.
fn is_exempt(value: &str, tier: Tier) -> bool {
    if is_variable_reference(value) || is_filesystem_path(value) {
        return true;
    }
    match tier {
        Tier::Strict => false,
        Tier::Shaped => !has_credential_shape(value),
    }
}

/// `$TOKEN`, `${VAULT_PW}`, `%SECRET%` — eine **Referenz** auf ein Geheimnis
/// ist nicht das Geheimnis. Sie zu redigieren würde Information vernichten,
/// ohne irgendetwas zu schützen.
fn is_variable_reference(value: &str) -> bool {
    value.starts_with('$') || (value.starts_with('%') && value.ends_with('%') && value.len() > 2)
}

/// Ein Dateipfad benennt einen Ort, kein Geheimnis: `TOKEN_FILE=/run/secrets/tok`,
/// `PWD=/home/claude`.
///
/// Bewusst eng: absoluter oder explizit relativer Pfad, kein Leerraum, mindestens
/// ein weiterer `/`. Ja, das ist ein Loch — ein Passwort der Form `/foo/bar` bliebe
/// stehen. Der Preis wäre sonst, in jedem Transkript jeden `PWD`- und
/// `*_PATH`-Wert zu schwärzen, und ein Record ohne Pfade ist für den Reviewer
/// wertlos.
fn is_filesystem_path(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    // Windows-Pfade: `C:\path`, `D:\...` etc. Char-grenzengerecht.
    let windows = {
        let mut chars = value.chars();
        if let (Some(first), Some(second), Some(third)) = (chars.next(), chars.next(), chars.next())
        {
            first.is_ascii_alphabetic() && second == ':' && third == '\\'
        } else {
            false
        }
    };
    windows
        || value.starts_with("~/")
        || value.starts_with("./")
        || value.starts_with("../")
        || (value.starts_with('/') && value[1..].contains('/'))
}

/// Sieht der Wert aus wie ein maschinell erzeugtes Credential?
///
/// Nur für [`Tier::Shaped`]. Drei Filter, jeder gegen eine konkrete Sorte
/// Fehlalarm aus echten Agent-Transkripten:
///
/// 1. **Mindestens 8 Zeichen** — gegen `tokens: 1234`.
/// 2. **Nicht rein numerisch** (inkl. `.`/`-`/`:`) — gegen Versionen, Ports,
///    Zeitstempel, Zähler: `Token-Limit: 4096`, `input_tokens: 45123`.
/// 3. **Nicht rein alphabetisch** — gegen deutsche und englische Prosa:
///    `Der Token-Verbrauch: erstaunlich`. Maschinell erzeugte Tokens sind
///    base64/hex und enthalten praktisch immer Ziffern oder `-_/+=`.
///
/// Filter 3 ist die bewusste Lücke dieses Tiers: ein rein alphabetisches
/// `TOKEN=supersecrettoken` entgeht ihm. Wem das zu knapp ist, trägt `token`
/// in `secret_keys` ein — dann gilt Strict.
pub fn has_credential_shape(value: &str) -> bool {
    /// Werte, die als Belegung eines `token`/`session`-Schlüssels vorkommen,
    /// aber nie ein Credential sind.
    const STOPLIST: &[&str] = &[
        "undefined",
        "localhost",
        "enabled",
        "disabled",
        "required",
        "optional",
        "default",
        "unlimited",
    ];

    if value.len() < 8 {
        return false;
    }
    if STOPLIST.iter().any(|s| value.eq_ignore_ascii_case(s)) {
        return false;
    }
    if value
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | ':' | ','))
    {
        return false;
    }
    if value.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// UrlCredentialRedactor
// ---------------------------------------------------------------------------

/// Zugangsdaten im Autoritätsteil einer URL: `schema://user:pass@host`.
///
/// Redigiert **nur den `userinfo`-Teil** — Schema und Host bleiben stehen. Aus
/// `postgres://admin:s3cr3t@db.internal:5432/prod` wird
/// `postgres://[redacted:secret]@db.internal:5432/prod`: Der Reviewer sieht
/// weiter, *wohin* verbunden wurde, nur nicht mehr *womit*.
///
/// Der Nutzername wird mitredigiert, auch ohne Passwort daneben — in einer
/// Credential-URL ist er die halbe Zugangsdatei.
///
/// Kein Treffer ohne `@` vor dem ersten `/`: `https://gitlab.com/pdoering-it/minds`
/// und `https://example.com/a@b` bleiben unberührt.
///
/// Die Wiederholungen sind aus demselben Grund unbegrenzt wie in [`VALUE`]:
/// negierte Klassen entrollen teuer. Begrenzt wird der Fund strukturell — `/`,
/// `@` und Leerraum sind ausgeschlossen.
const URL_CREDENTIAL_PATTERN: &str =
    r"(?i)[A-Za-z][A-Za-z0-9+.\-]{0,31}://(?P<userinfo>[^\s/@:]+(?::[^\s/@]*)?)@";

/// Detektor für Zugangsdaten in URLs.
pub struct UrlCredentialRedactor {
    re: Regex,
}

impl UrlCredentialRedactor {
    /// Baut den Detektor.
    pub fn new() -> Self {
        Self {
            re: Regex::new(URL_CREDENTIAL_PATTERN).expect("konstantes URL-Muster muss kompilieren"),
        }
    }
}

impl Default for UrlCredentialRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for UrlCredentialRedactor {
    fn name(&self) -> &str {
        "url-credential"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        self.re
            .captures_iter(text)
            .filter_map(|caps| caps.name("userinfo"))
            .map(|m| Finding::new(Category::Secret, m.start(), m.end()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::RedactionPipeline;

    fn redact(text: &str) -> crate::pipeline::RedactedText {
        RedactionPipeline::new()
            .with(KeyValueRedactor::new())
            .with(UrlCredentialRedactor::new())
            .redact(text)
    }

    // --- Der Kern: die .env-Datei --------------------------------------------

    #[test]
    fn env_file_leaks_nothing() {
        // Genau die Datei, an der die form-basierten Detektoren scheitern.
        let env = "DB_USER=admin\n\
                   DB_PASSWORD=hunter2\n\
                   SMTP_PASSWORD=Sommer2024!\n\
                   JWT_SECRET=abc123\n\
                   DATABASE_URL=postgres://admin:s3cr3t@db.internal:5432/prod\n";
        let out = redact(env);

        // `admin` steht zweimal drin: als `DB_USER`-Wert (Identity-Regel) und
        // im Autoritätsteil der URL (URL-Regel). Beide müssen weg.
        for leak in ["hunter2", "Sommer2024!", "abc123", "s3cr3t", "admin"] {
            assert!(
                !out.text.contains(leak),
                "{leak:?} steht noch im Record:\n{}",
                out.text
            );
        }
        // Die Schlüssel bleiben lesbar — der Reviewer sieht, *dass* eine
        // Zugangsdatei im Spiel war.
        assert!(out.text.contains("DB_PASSWORD="));
        assert!(out.text.contains("postgres://"));
        assert!(out.text.contains(":5432/prod"));
    }

    #[test]
    fn short_low_entropy_password_is_caught() {
        // Der Fall, den Entropie prinzipiell nicht fangen kann.
        let out = redact("DB_PASSWORD=hunter2");
        assert_eq!(out.text, "DB_PASSWORD=[redacted:secret]");
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn only_the_value_is_redacted_never_the_key() {
        let out = redact("SMTP_PASSWORD=Sommer2024!");
        assert!(out.text.starts_with("SMTP_PASSWORD="));
        assert!(!out.text.contains("Sommer2024"));
    }

    // --- Zuweisungs-Formen ----------------------------------------------------

    #[test]
    fn handles_json_yaml_ini_and_export_forms() {
        for (input, marker) in [
            (r#"{"client_secret": "xyz"}"#, "xyz"),
            ("password: geheim", "geheim"),
            ("password = 'geheim'", "geheim"),
            ("export API_PASSWORD=geheim", "geheim"),
            (r#"passphrase="mit space drin""#, "mit space drin"),
        ] {
            let out = redact(input);
            assert!(!out.text.contains(marker), "durchgerutscht: {input:?}");
        }
    }

    #[test]
    fn cli_flag_with_space_is_covered() {
        // Tool-Calls schießen Shell-Kommandos ab — dort steht kein `=`.
        let out = redact("curl --password hunter2 -X GET https://api.test");
        assert!(!out.text.contains("hunter2"));
        assert!(out.text.contains("--password "));
    }

    #[test]
    fn bearer_scheme_stays_the_token_goes() {
        let out = redact("Authorization: Bearer abc123def456");
        assert_eq!(out.text, "Authorization: Bearer [redacted:secret]");
    }

    #[test]
    fn empty_value_is_not_a_finding() {
        // `PASSWORD=` ohne Wert ist Information, kein Leck.
        let out = redact("PASSWORD=\nNEXT=1");
        assert_eq!(out.text, "PASSWORD=\nNEXT=1");
        assert_eq!(out.counts.secrets, 0);
    }

    #[test]
    fn separator_never_crosses_a_line_break() {
        // Der Trenner ist zeilenlokal. Sonst liest ein leerer Wert den Wert der
        // *Folgezeile* als Geheimnis — das leckt nichts, zerstört aber den
        // Record und verfälscht die Zähler.
        for text in [
            "PASSWORD=\nDB_HOST=localhost",
            "Das Secret:\nDer Reviewer liest die Absicht.",
            "curl --password\n--verbose",
        ] {
            let out = redact(text);
            assert_eq!(out.text, text, "Zeilenumbruch übersprungen: {text:?}");
            assert_eq!(out.counts, minds_core::RedactionCounts::default());
        }
    }

    #[test]
    fn multiple_assignments_on_one_line() {
        let out = redact("PASSWORD=aaa TOKEN=bbb111ccc");
        assert!(!out.text.contains("aaa"));
        assert!(!out.text.contains("bbb111ccc"));
        assert_eq!(out.counts.secrets, 2);
    }

    // --- Ausnahmen ------------------------------------------------------------

    #[test]
    fn variable_reference_is_kept() {
        let out = redact("PASSWORD=${VAULT_PW}");
        assert_eq!(out.text, "PASSWORD=${VAULT_PW}");
        assert_eq!(out.counts.secrets, 0);
    }

    #[test]
    fn filesystem_path_is_kept() {
        let out = redact("PWD=/home/claude\nTOKEN_FILE=/run/secrets/tok");
        assert!(out.text.contains("/home/claude"));
        assert!(out.text.contains("/run/secrets/tok"));
        assert_eq!(out.counts.secrets, 0);
    }

    #[test]
    fn windows_filesystem_path_is_kept() {
        // Windows-Pfade: `C:\path`, `D:\data`, etc. müssen erkannt werden.
        // Ohne Leerzeichen, da der VALUE-Regex keine Whitespace erlaubt.
        for path in &["D:\\data\\backup", "C:\\Windows\\app", "E:\\secrets\\file"] {
            let input = format!("DB_PASSWORD={}", path);
            let out = redact(&input);
            assert!(
                out.text.contains(path),
                "Windows-Pfad nicht erkannt: {}",
                path
            );
            assert_eq!(out.counts.secrets, 0, "Pfad wurde redigiert: {}", input);
        }
    }

    // --- Shaped-Tier: keine Fehlalarme in Prosa ------------------------------

    #[test]
    fn token_counters_in_prose_are_not_redacted() {
        // Diese Zeilen stehen in *jedem* Agent-Transkript.
        for prose in [
            "input_tokens: 1234",
            "Total tokens: 45123",
            "Token-Limit: 4096",
            "Der Token-Verbrauch: erstaunlich",
            "session: enabled",
        ] {
            let out = redact(prose);
            assert_eq!(out.text, prose, "Fehlalarm auf {prose:?}");
        }
    }

    #[test]
    fn author_line_is_not_mistaken_for_authorization() {
        let out = redact("Author: Patrick Doering <p@example.org>");
        assert!(out.text.contains("Patrick Doering"));
    }

    #[test]
    fn user_role_marker_is_not_redacted() {
        // `User:` ist in einem Transkript die Rollenmarke, kein Feldname.
        let out = redact("User: bitte den Retry-Test reparieren");
        assert_eq!(out.text, "User: bitte den Retry-Test reparieren");
    }

    #[test]
    fn shaped_token_with_digits_is_redacted() {
        let out = redact("GITLAB_TOKEN=abc123def456");
        assert_eq!(out.text, "GITLAB_TOKEN=[redacted:secret]");
    }

    #[test]
    fn credential_shape_predicate() {
        assert!(has_credential_shape("abc123def456"));
        assert!(has_credential_shape("aGVsbG8="));
        assert!(!has_credential_shape("1234"));
        assert!(!has_credential_shape("45123"));
        assert!(!has_credential_shape("1.2.3.4"));
        assert!(!has_credential_shape("2026-07-22"));
        assert!(!has_credential_shape("erstaunlich"));
        assert!(!has_credential_shape("enabled"));
        assert!(!has_credential_shape("UNDEFINED"));

        // Bewusst *kein* Freibrief: Sobald ein Buchstabe in der Versions-
        // nummer steckt, gilt sie als credential-typisch. `token: 1.2.3-rc1`
        // wird also geschwärzt. Der Filter verspricht „nicht rein numerisch",
        // nicht „erkennt Versionen" — und diese Über-Schwärzung ist nur hinter
        // einem token-artigen Schlüssel überhaupt erreichbar.
        assert!(has_credential_shape("1.2.3-rc1"));
    }

    // --- Identity -------------------------------------------------------------

    #[test]
    fn username_is_redacted_as_pii() {
        let out = redact("DB_USERNAME=p.doering");
        assert_eq!(out.text, "DB_USERNAME=[redacted:pii]");
        assert_eq!(out.counts.pii, 1);
    }

    // --- URL-Zugangsdaten -----------------------------------------------------

    #[test]
    fn url_credentials_are_cut_out_host_stays() {
        let out = RedactionPipeline::new()
            .with(UrlCredentialRedactor::new())
            .redact("git push https://oauth2:glpat-XYZ@gitlab.com/p/minds.git");
        assert_eq!(
            out.text,
            "git push https://[redacted:secret]@gitlab.com/p/minds.git"
        );
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn bare_username_in_url_is_redacted() {
        let out = RedactionPipeline::new()
            .with(UrlCredentialRedactor::new())
            .redact("https://patrick@example.com/x");
        assert_eq!(out.text, "https://[redacted:secret]@example.com/x");
    }

    #[test]
    fn plain_url_is_untouched() {
        for url in [
            "https://gitlab.com/pdoering-it/minds",
            "https://example.com/a@b",
            "siehe https://docs.gitlab.com/ci/",
        ] {
            let out = RedactionPipeline::new()
                .with(UrlCredentialRedactor::new())
                .redact(url);
            assert_eq!(out.text, url);
        }
    }

    #[test]
    fn multibyte_context_is_preserved() {
        let out = redact("🦀 PASSWORD=hünter2 🦀 café");
        assert_eq!(out.text, "🦀 PASSWORD=[redacted:secret] 🦀 café");
    }

    #[test]
    fn multibyte_password_does_not_panic() {
        // Issue #1: PASSWORD=hunter€2 sollte nicht panicked
        let out = redact("PASSWORD=hunter€2");
        // Der Wert ist kein Pfad, sollte also redigiert werden
        assert_eq!(out.text, "PASSWORD=[redacted:secret]");
        assert_eq!(out.counts.secrets, 1);
    }

    // --- Konfigurierbare Schlüssel -------------------------------------------

    #[test]
    fn extra_keys_are_treated_as_strict() {
        let r = KeyValueRedactor::with_extra_keys(&["VAULT_ROLE_ID", "DB_USER"]).unwrap();
        let out = RedactionPipeline::new()
            .with(r)
            .redact("VAULT_ROLE_ID=x1\nDB_USER=admin");
        assert!(!out.text.contains("x1"));
        assert!(!out.text.contains("admin"));
        assert_eq!(out.counts.secrets, 2);
    }

    #[test]
    fn extra_keys_are_escaped_not_interpreted() {
        // Ein Eintrag mit Regex-Metazeichen darf kein Muster werden.
        let r = KeyValueRedactor::with_extra_keys(&["a.c"]).unwrap();
        let out = RedactionPipeline::new().with(r).redact("abc=wert");
        assert_eq!(out.text, "abc=wert");
    }

    #[test]
    fn blank_extra_keys_are_ignored() {
        let r = KeyValueRedactor::with_extra_keys(&["", "  "]).unwrap();
        let out = RedactionPipeline::new().with(r).redact("beliebig=wert");
        assert_eq!(out.text, "beliebig=wert");
    }

    #[test]
    fn rule_names_are_stable() {
        let names = KeyValueRedactor::new().rule_names();
        assert!(names.contains(&"assignment-strict"));
        assert!(names.contains(&"assignment-shaped"));
        assert!(names.contains(&"cli-flag"));
        assert_eq!(names.len(), KEY_RULES.len());
        assert_eq!(KeyValueRedactor::new().name(), "key-value");
        assert_eq!(UrlCredentialRedactor::new().name(), "url-credential");
    }
}
