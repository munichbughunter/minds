//! Korpus-Tests: die Redaction gegen realistischen Text, in beide Richtungen.
//!
//! Die Unit-Tests in den Detektor-Modulen prüfen je *ein* Muster. Dieser Korpus
//! prüft die **zusammengesetzte Default-Policy** gegen Text, wie er in
//! Agent-Transkripten tatsächlich vorkommt — und er prüft beide Fehlerrichtungen,
//! weil nur eine davon offensichtlich ist:
//!
//! - **[`MUST_REDACT`] — False Negatives.** Was hier durchrutscht, ist ein Leck.
//!   Der teure Fehler, und der, an den alle denken.
//! - **[`MUST_SURVIVE`] — False Positives.** Was hier fälschlich verschwindet,
//!   zerstört den Record. Ein Reader, der statt des Prompts eine Reihe von
//!   Platzhaltern zeigt, ist wertlos — dann kann man Minds auch weglassen. Diese
//!   Hälfte ist die schwierigere und deshalb die längere Tabelle.
//!
//! Dazu zwei Tabellen, die keine Prüfung, sondern eine **Festschreibung** sind:
//! [`ACCEPTED_OVER_REDACTION`] hält fest, wo wir bewusst zu viel schwärzen, und
//! [`DOCUMENTED_GAPS`], wo bewusst zu wenig. Beide Tabellen schlagen an, sobald
//! sich das Verhalten ändert — auch zum Besseren. Genau das ist der Zweck: Eine
//! stillschweigende Verschiebung der Trennlinie soll es nicht geben.
//!
//! # Zwei Regeln für dieses Verzeichnis
//!
//! 1. **Nie ein echtes Geheimnis als Fixture.** Alle Token hier sind
//!    synthetisch, alle Adressen liegen in `example.*`/`.test`. Die Datei liegt
//!    im Repo — dieselbe Regel wie für die Allowlist.
//! 2. **Ein Fixture, das rot wird, ist erst einmal eine Frage, keine Antwort.**
//!    Sie lautet: Ist die neue Trennlinie besser? Wenn ja, wandert der Eintrag
//!    in die andere Tabelle, statt gelöscht zu werden.
//!
//! Der Korpus prüft die *Detektoren* (Schicht 2). Die Mauer davor
//! (Zugangsdaten-Dateien, Schicht 1) und die Garantie dahinter
//! (`redact_session`, Schicht 3) haben je einen eigenen Test am Ende.

use minds_core::{Agent, Intent, Model, Role, Session, ToolCall, Turn};
use minds_redact::{
    HighEntropyConfig, KnownTokenRedactor, RedactionConfig, RedactionPipeline, is_secret_file,
};

// ---------------------------------------------------------------------------
// Synthetische Token
// ---------------------------------------------------------------------------
//
// Erfundene Werte in der jeweils gültigen Form (Präfix + exakte Länge). Wer eine
// Form ergänzt, ergänzt hier einen Wert und unten ein Fixture — sonst schlägt
// `every_known_token_format_has_a_fixture` an.

/// Der in AWS' eigener Dokumentation veröffentlichte Beispiel-Key.
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
const GITHUB_TOKEN: &str = "ghp_u8jzPde0IgxLd6GncfBAepfJBd0Kh8oOOL8d";
const GITHUB_FINE_GRAINED: &str = concat!(
    "github_pat_KLzdocJ2isAjIhKtJ0RlgL_",
    "KOmxgJTeKdNnFRIBXuDL7DxtpYlSXpfKtHF4vUCsMehGAkWvj7FAc9QeWJK"
);
const GITLAB_PAT: &str = "glpat-ORS-6ilI8ihN5KXSc7Tv";
const SLACK_TOKEN: &str = "xoxb-NhFdnXsiVpzz63FfkCzJr";
const GOOGLE_API_KEY: &str = "AIzar3J1TWDtkwtDDb_xHKas1VOqg6YYZYn9Zhy";
const STRIPE_KEY: &str = "sk_live_enCkhvMdgaKjIg8xNbe3nNyj";
const NPM_TOKEN: &str = "npm_Oq9wMxEhh2FDEEtfjgVvVqE1SkHbn88HxjSI";
const JWT: &str = concat!(
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.",
    "eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkFubmEifQ.",
    "dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk"
);
const PEM_KEY: &str = concat!(
    "-----BEGIN RSA PRIVATE KEY-----\n",
    "MIIEowIBAAKCAQEAx7Vn9pQmKtLbYcHZfWjRuEoNsAiPdGqTvXlMzBhKrCyWnUeF\n",
    "gJdSaOvTpQiXmZbNlHrKcYtEuAfWdGxPjRoLsVnBqMzCyIeUkTaFhDpWvNgXlOrJ\n",
    "-----END RSA PRIVATE KEY-----"
);
/// Prefixloser base64-Blob, hoch genug für das Entropie-Netz (~4.6 bit/Zeichen).
const ENTROPY_BLOB: &str = "dMlHUvTCQCyEZDz/TddJ8HyS5SUkCnD8zRA9a9SkpXz9";
const JSON_API_KEY: &str = "s3rv1ce-4cc0unt-k3y-2024";

/// Die `.env`, an der form-basierte Erkennung scheitert — der Grund, warum es
/// [`KeyValueRedactor`](minds_redact::KeyValueRedactor) gibt.
const DOTENV: &str = concat!(
    "DB_USER=admin\n",
    "DB_PASSWORD=hunter2\n",
    "SMTP_PASSWORD=Sommer2024!\n",
    "JWT_SECRET=abc123\n",
    "DATABASE_URL=postgres://admin:s3cr3t@db.internal:5432/prod"
);

// ---------------------------------------------------------------------------
// False Negatives: muss verschwinden
// ---------------------------------------------------------------------------

/// Ein Fixture, das etwas enthält, das verschwinden muss.
struct MustRedact {
    /// Stabiler Bezeichner; bei den Token-Formen **identisch** mit dem
    /// Regelnamen aus [`KnownTokenRedactor::covered_formats`].
    id: &'static str,
    /// Der Eingabetext.
    text: String,
    /// Zeichenketten, die in der Ausgabe **nicht** mehr vorkommen dürfen.
    gone: &'static [&'static str],
    /// Zeichenketten, die überleben müssen — Redaction darf den Kontext nicht
    /// mitnehmen. Ohne diese Spalte wäre „alles schwärzen" eine bestandene
    /// Prüfung.
    kept: &'static [&'static str],
}

fn must_redact() -> Vec<MustRedact> {
    vec![
        MustRedact {
            id: "aws-access-key-id",
            text: format!("aws_access_key_id = {AWS_KEY}"),
            gone: &[AWS_KEY],
            kept: &["aws_access_key_id"],
        },
        MustRedact {
            id: "aws-secret-access-key",
            text: "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            gone: &["wJalrXUtnFEMI"],
            kept: &["export AWS_SECRET_ACCESS_KEY="],
        },
        MustRedact {
            // Der Klassiker: Zugangsdaten im Git-Remote. Hier greifen drei
            // Detektoren übereinander — Token-Form, URL-Struktur und der
            // `oauth`-Schlüssel. Die Pipeline führt sie zu einem Fund zusammen.
            id: "github-token",
            text: format!(
                "git remote set-url origin https://oauth2:{GITHUB_TOKEN}@github.com/acme/app.git"
            ),
            gone: &[GITHUB_TOKEN, "oauth2"],
            kept: &["git remote set-url origin"],
        },
        MustRedact {
            id: "github-fine-grained-pat",
            text: format!("GH_TOKEN={GITHUB_FINE_GRAINED}"),
            gone: &[GITHUB_FINE_GRAINED],
            kept: &["GH_TOKEN="],
        },
        MustRedact {
            id: "gitlab-pat",
            text: format!("PRIVATE-TOKEN: {GITLAB_PAT}"),
            gone: &[GITLAB_PAT],
            kept: &["PRIVATE-TOKEN:"],
        },
        MustRedact {
            id: "slack-token",
            text: format!("slack_bot_token = {SLACK_TOKEN}"),
            gone: &[SLACK_TOKEN],
            kept: &["slack_bot_token"],
        },
        MustRedact {
            id: "google-api-key",
            text: format!("gmaps.setKey('{GOOGLE_API_KEY}')"),
            gone: &[GOOGLE_API_KEY],
            kept: &["gmaps.setKey"],
        },
        MustRedact {
            id: "stripe-live-key",
            text: format!("STRIPE_KEY={STRIPE_KEY}"),
            gone: &[STRIPE_KEY],
            kept: &["STRIPE_KEY="],
        },
        MustRedact {
            id: "npm-token",
            text: format!("//registry.npmjs.org/:_authToken={NPM_TOKEN}"),
            gone: &[NPM_TOKEN],
            kept: &["registry.npmjs.org"],
        },
        MustRedact {
            // `Bearer` bleibt stehen, der Token geht: Aus dem Record soll
            // hervorgehen, *dass* authentifiziert wurde.
            id: "jwt",
            text: format!("Authorization: Bearer {JWT}"),
            gone: &[JWT, "eyJhbGci"],
            kept: &["Authorization:", "Bearer"],
        },
        MustRedact {
            id: "private-key-pem",
            text: format!("cat deploy.key\n{PEM_KEY}"),
            gone: &["MIIEowIBAAKCAQEA", "BEGIN RSA PRIVATE KEY"],
            kept: &["cat deploy.key"],
        },
        MustRedact {
            // Fünf Zeilen, fünf Funde — keiner davon form-erkennbar.
            id: "dotenv-block",
            text: DOTENV.into(),
            gone: &["hunter2", "Sommer2024!", "abc123", "s3cr3t", "admin"],
            kept: &["DB_PASSWORD=", "postgres://", ":5432/prod"],
        },
        MustRedact {
            id: "cli-password-flag",
            text: "psql --password hunter2 --host db.internal".into(),
            gone: &["hunter2"],
            kept: &["--password", "--host db.internal"],
        },
        MustRedact {
            id: "cli-password-equals",
            text: "mysql -u root --password=Sommer2024 -h db".into(),
            gone: &["Sommer2024"],
            kept: &["mysql -u root", "-h db"],
        },
        MustRedact {
            id: "url-credentials",
            text: "clone von https://buildbot:Sommer2024@git.internal.test/infra.git".into(),
            gone: &["Sommer2024", "buildbot"],
            kept: &["clone von", "https://", "/infra.git"],
        },
        MustRedact {
            id: "email-address",
            text: "Bug gemeldet von anna.mueller@example.org, siehe #4711".into(),
            gone: &["anna.mueller@example.org", "anna.mueller"],
            kept: &["Bug gemeldet von", "#4711"],
        },
        MustRedact {
            id: "two-emails",
            text: "cc: anna@example.org, bob@example.net".into(),
            gone: &["anna@example.org", "bob@example.net"],
            kept: &["cc:"],
        },
        MustRedact {
            // Ohne Präfix und ohne Schlüsselnamen — nur das Entropie-Netz.
            id: "high-entropy-blob",
            text: format!("Antwort-Body: {ENTROPY_BLOB}"),
            gone: &[ENTROPY_BLOB],
            kept: &["Antwort-Body:"],
        },
        MustRedact {
            id: "json-api-key",
            text: format!(r#"{{"api_key": "{JSON_API_KEY}", "region": "eu-central-1"}}"#),
            gone: &[JSON_API_KEY],
            kept: &[r#""api_key""#, "eu-central-1"],
        },
        MustRedact {
            id: "basic-auth-header",
            text: "curl -H 'Authorization: Basic YWRtaW46aHVudGVyMg=='".into(),
            gone: &["YWRtaW46aHVudGVyMg"],
            kept: &["Authorization:", "Basic"],
        },
        MustRedact {
            id: "k8s-secret-yaml",
            text: "apiVersion: v1\nkind: Secret\ndata:\n  password: aHVudGVyMg==".into(),
            gone: &["aHVudGVyMg"],
            kept: &["kind: Secret", "password:"],
        },
    ]
}

// ---------------------------------------------------------------------------
// False Positives: muss unverändert bleiben
// ---------------------------------------------------------------------------

/// Text aus echten Transkripten, der **Zeichen für Zeichen** überleben muss.
///
/// Jeder Eintrag ist ein Fehlalarm, der einmal plausibel war: ein Wort, das auch
/// ein Schlüsselname ist; eine Hex-Kette, die auch ein Secret sein könnte; eine
/// URL, die auch Zugangsdaten tragen könnte. Die Kommentare nennen den Detektor,
/// der hier zu Recht schweigt.
const MUST_SURVIVE: &[(&str, &str)] = &[
    // --- `token`/`secret` als Wort, nicht als Schlüssel ---------------------
    ("prose-token", "Der Token-Verbrauch war erstaunlich hoch."),
    (
        "bearer-prose",
        "Das Bearer-Token-Konzept muss noch erklaert werden.",
    ),
    (
        // Der Grund für den Shaped-Tier: Agent-Transkripte sind voll davon.
        "token-counters",
        "input_tokens: 45123, output_tokens: 890, Token-Limit: 4096",
    ),
    ("unset-key", "SECRET_KEY_BASE ist nicht gesetzt"),
    ("header-name", "x-api-key header fehlt im Request"),
    (
        "login-prose",
        "Login fehlgeschlagen (401) nach drei Versuchen",
    ),
    (
        "session-prose",
        "Die Session lief 12 Minuten und kostete 3 Cent.",
    ),
    ("stoplist-value", "session: localhost"),
    // --- Hex-Ketten: lang genug, aber zu entropiearm ------------------------
    (
        "git-sha",
        "Siehe commit 356a192b7913b04c54574d18c28d46e6395428ab",
    ),
    (
        // `hash` steht bewusst nicht in der Schlüsseltabelle.
        "hash-is-not-a-key",
        "hash: 356a192b7913b04c54574d18c28d46e6395428ab",
    ),
    ("md5", "md5 e2fc714c4727ee9395f324cd2e7f331f"),
    (
        "docker-digest",
        "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    ),
    ("uuid", "request_id: 550e8400-e29b-41d4-a716-446655440000"),
    // --- Rollenmarken und Metadaten: der teuerste Fehlalarm ------------------
    (
        // `User:` markiert in jedem zweiten Transkript eine Nachricht. Würde die
        // Identity-Regel hier greifen, wäre der Record unlesbar.
        "role-marker",
        "User: Kann ich das Retry-Timeout anpassen?",
    ),
    (
        // `auth` allein ist kein Schlüssel — sonst stürbe jede Git-Log-Zeile.
        "author-line",
        "Author: Patrick Doering",
    ),
    // --- Werte, die nur wie Geheimnisse aussehen ----------------------------
    (
        // `key` allein steht nicht in der Tabelle.
        "key-alone",
        "let hotkey = monkey_keyboard.get(&id);",
    ),
    (
        // Ein Pfad benennt einen Ort, kein Geheimnis.
        "secret-path-not-value",
        "TOKEN_FILE=/run/secrets/api-token",
    ),
    (
        // Eine Referenz auf ein Geheimnis ist nicht das Geheimnis.
        "variable-reference",
        "API_KEY=$MY_API_KEY",
    ),
    ("percent-reference", "set PASSWORD=%DEPLOY_PW%"),
    // --- URLs ohne Zugangsdaten ---------------------------------------------
    ("plain-repo-url", "https://gitlab.com/pdoering-it/minds"),
    ("at-after-path", "https://example.com/a@b"),
    ("socket-addr", "lauscht auf 127.0.0.1:8080"),
    // --- Knapp verfehlte Token-Formen ---------------------------------------
    (
        // Die Längen in den Token-Mustern sind exakt, nicht ungefähr.
        "near-miss-aws",
        "AKIA1234 ist zu kurz fuer einen echten Key",
    ),
    ("near-miss-github", "ghp_zutokurz taucht im Log auf"),
    (
        // Der Vorfilter trifft `-----BEGIN `, die Regex verlangt PRIVATE KEY.
        "certificate-header",
        "-----BEGIN CERTIFICATE-----",
    ),
    // --- Gewöhnlicher Werkzeug-Output ---------------------------------------
    ("content-type", "Content-Type: application/json"),
    ("timestamp", "2026-07-22T09:14:33Z INFO minds::capture"),
    (
        "cargo-dependency",
        r#"serde = { version = "1", features = ["derive"] }"#,
    ),
    ("toml-table", "[high_entropy]\nenabled = true\nmin_len = 32"),
    (
        "env-example",
        ".env.example:\nDB_HOST=localhost\nDB_PORT=5432",
    ),
    (
        "cargo-invocation",
        "cargo test -p minds-redact -- --nocapture",
    ),
    (
        "panic-location",
        "thread 'main' panicked at crates/minds-redact/src/pipeline.rs:142",
    ),
    (
        "rust-code",
        "let cfg = RedactionConfig::default().pipeline()?;",
    ),
    (
        "german-prose",
        "Der Retry-Test flackert seit dem Umstieg auf tokio 1.35 sporadisch.",
    ),
    ("semver", "minds 0.1.0 (build 2026-07-22)"),
];

// ---------------------------------------------------------------------------
// Festgeschriebene Kompromisse
// ---------------------------------------------------------------------------

/// Stellen, an denen die Policy **bewusst zu viel** schwärzt: `(id, ein, aus)`.
///
/// Der Preis des Strict-Tiers, offen benannt. Ein verlorenes Wort im Record ist
/// ein Schönheitsfehler, ein durchgerutschtes Passwort ein Incident — deshalb
/// gewinnt hier die grobe Regel. Wo es im Alltag stört, ist die Allowlist die
/// Antwort, nicht ein weicherer Detektor.
const ACCEPTED_OVER_REDACTION: &[(&str, &str, &str)] = &[
    (
        "strict-tier-eats-prose",
        "Secret: der Reviewer liest hier mit",
        "Secret: [redacted:secret] Reviewer liest hier mit",
    ),
    (
        // Ein Login-Name ist keine Zugangsdatei, aber er benennt eine Person.
        "identity-key-is-pii",
        "DB_USER=admin",
        "DB_USER=[redacted:pii]",
    ),
];

/// Stellen, an denen die Policy **bewusst zu wenig** fängt: `(id, Text, Wert)`.
///
/// Beide Lücken sind strukturell, nicht Nachlässigkeit: In Prosa fehlt der
/// Zuweisungs-Kontext, und ein rein alphabetischer Wert ist von einem Wort nicht
/// zu unterscheiden. Die Antwort darauf steht nicht in einem besseren Detektor,
/// sondern in den anderen Schichten: die Mauer (`.env` wird gar nicht erst
/// gescannt) und die Config (`secret_keys` hebt `token` in den Strict-Tier).
///
/// Wird ein Eintrag hier grün, weil jemand die Lücke geschlossen hat: Eintrag
/// löschen und in [`MUST_REDACT`] aufnehmen.
const DOCUMENTED_GAPS: &[(&str, &str, &str)] = &[
    (
        "prose-password",
        "das Passwort war uebrigens hunter2",
        "hunter2",
    ),
    (
        "alphabetic-shaped-token",
        "TOKEN=supersecrettoken",
        "supersecrettoken",
    ),
];

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

/// Die Policy, die ein Repo ohne eigene Config bekommt.
fn policy() -> RedactionPipeline {
    RedactionConfig::default()
        .pipeline()
        .expect("Default-Policy muss bauen")
}

/// Sammelt alle Beanstandungen und meldet sie **gemeinsam**.
///
/// Ein Korpus, der beim ersten Treffer abbricht, verwandelt „drei Fixtures sind
/// rot" in drei Testläufe. Der Bericht am Ende zeigt das ganze Bild.
#[derive(Default)]
struct Report {
    problems: Vec<String>,
}

impl Report {
    fn note(&mut self, problem: String) {
        self.problems.push(problem);
    }

    fn finish(self, headline: &str) {
        assert!(
            self.problems.is_empty(),
            "{headline} — {} Beanstandung(en):\n\n{}\n",
            self.problems.len(),
            self.problems.join("\n")
        );
    }
}

/// Der Text eines [`MUST_SURVIVE`]-Eintrags, über seinen Bezeichner.
fn survivor(id: &str) -> &'static str {
    MUST_SURVIVE
        .iter()
        .find(|(fixture, _)| *fixture == id)
        .map(|(_, text)| *text)
        .unwrap_or_else(|| panic!("kein MUST_SURVIVE-Fixture mit id {id:?}"))
}

/// Setzt einen Text in eine Mehrbyte-Umgebung. Die Detektoren rechnen in Bytes;
/// ein Fund, dessen Grenzen nicht auf einer Zeichengrenze liegen, würde beim
/// Ersetzen paniken oder als Vertragsverstoß gezählt.
fn in_multibyte_context(text: &str) -> String {
    format!("Präfix äöü 🦀\n{text}\n🦀 Süffix")
}

// ---------------------------------------------------------------------------
// Die beiden Fehlerrichtungen
// ---------------------------------------------------------------------------

#[test]
fn no_corpus_fixture_leaks() {
    let pipeline = policy();
    let mut report = Report::default();

    for case in must_redact() {
        let out = pipeline.redact(&case.text);

        for needle in case.gone {
            if out.text.contains(needle) {
                report.note(format!(
                    "{}: {needle:?} steht noch im Ergebnis:\n    {}",
                    case.id, out.text
                ));
            }
        }
        if out.counts == Default::default() {
            report.note(format!("{}: kein einziger Fund gezählt", case.id));
        }
    }

    report.finish("Der Korpus leckt");
}

#[test]
fn redaction_keeps_the_surrounding_context() {
    let pipeline = policy();
    let mut report = Report::default();

    for case in must_redact() {
        let out = pipeline.redact(&case.text);
        for needle in case.kept {
            if !out.text.contains(needle) {
                report.note(format!(
                    "{}: {needle:?} wurde mit weggeschwärzt:\n    {}",
                    case.id, out.text
                ));
            }
        }
    }

    report.finish("Die Redaction nimmt zu viel Kontext mit");
}

#[test]
fn harmless_text_is_never_touched() {
    let pipeline = policy();
    let mut report = Report::default();

    for (id, text) in MUST_SURVIVE {
        let out = pipeline.redact(text);
        if out.text != *text {
            report.note(format!(
                "{id}: verändert\n    ein: {text}\n    aus: {}",
                out.text
            ));
        } else if out.counts != Default::default() {
            report.note(format!(
                "{id}: unverändert, aber gezählt ({} secret, {} pii)",
                out.counts.secrets, out.counts.pii
            ));
        }
    }

    report.finish("Fehlalarme im Korpus");
}

#[test]
fn accepted_over_redaction_is_pinned() {
    let pipeline = policy();
    let mut report = Report::default();

    for (id, input, expected) in ACCEPTED_OVER_REDACTION {
        let out = pipeline.redact(input);
        if out.text != *expected {
            report.note(format!(
                "{id}: die festgeschriebene Über-Redaction hat sich verschoben\n    \
                 erwartet: {expected}\n    tatsächlich: {}",
                out.text
            ));
        }
    }

    report.finish(
        "Ein bewusster Kompromiss hat sich geändert. Wenn die neue Trennlinie \
         besser ist: Eintrag anpassen oder in MUST_SURVIVE verschieben",
    );
}

#[test]
fn documented_gaps_are_pinned() {
    let pipeline = policy();
    let mut report = Report::default();

    for (id, text, value) in DOCUMENTED_GAPS {
        let out = pipeline.redact(text);
        if !out.text.contains(value) {
            report.note(format!(
                "{id}: die Lücke ist geschlossen — Ergebnis: {}",
                out.text
            ));
        }
    }

    report.finish(
        "Eine dokumentierte Lücke ist geschlossen — gute Nachricht. Eintrag aus \
         DOCUMENTED_GAPS entfernen und in MUST_REDACT aufnehmen",
    );
}

// ---------------------------------------------------------------------------
// Eigenschaften über den gesamten Korpus
// ---------------------------------------------------------------------------

#[test]
fn the_whole_corpus_reaches_a_fixpoint() {
    // Bereinigter Text darf sich beim zweiten Durchlauf nicht weiter verändern.
    // Täte er es, hätte der erste Lauf etwas stehen lassen — und `redact_session`
    // würde die Session mit `RedactionError::Unstable` verwerfen. Dieser Test ist
    // die Absicherung, dass die Prüfung dort nicht auf realistischem Text
    // fehlalarmiert.
    //
    // Verglichen wird der Text, nicht der Zähler: Aus `DB_PASSWORD=hunter2` wird
    // `DB_PASSWORD=[redacted:secret]`, und der Platzhalter ist im zweiten Lauf
    // erneut ein Wert hinter `PASSWORD=`. Er wird durch sich selbst ersetzt —
    // Zähler +1, Text unverändert.
    let pipeline = policy();
    let mut report = Report::default();

    let texts: Vec<String> = must_redact()
        .into_iter()
        .map(|c| c.text)
        .chain(MUST_SURVIVE.iter().map(|(_, t)| (*t).to_string()))
        .collect();

    for text in &texts {
        let once = pipeline.redact(text);
        let twice = pipeline.redact(&once.text);
        if twice.text != once.text {
            report.note(format!(
                "kein Fixpunkt:\n    ein:  {text}\n    1x:   {}\n    2x:   {}",
                once.text, twice.text
            ));
        }
    }

    report.finish("Redaction erreicht auf dem Korpus keinen Fixpunkt");
}

#[test]
fn no_detector_violates_the_span_contract() {
    // Ein vertragswidriger Span wird verworfen — der Text bliebe ungeschwärzt.
    // Über den ganzen Korpus, roh und in Mehrbyte-Umgebung, darf das nie
    // passieren.
    let pipeline = policy();
    let mut report = Report::default();

    let texts: Vec<String> = must_redact()
        .into_iter()
        .map(|c| c.text)
        .chain(MUST_SURVIVE.iter().map(|(_, t)| (*t).to_string()))
        .collect();

    for text in &texts {
        for variant in [text.clone(), in_multibyte_context(text)] {
            let out = pipeline.redact(&variant);
            if out.invalid_findings > 0 {
                report.note(format!(
                    "{} ungültige Span(s) auf:\n    {variant}",
                    out.invalid_findings
                ));
            }
        }
    }

    report.finish("Detektoren verletzen den Finding-Vertrag");
}

#[test]
fn corpus_holds_inside_multibyte_context() {
    let pipeline = policy();
    let mut report = Report::default();

    for case in must_redact() {
        let out = pipeline.redact(&in_multibyte_context(&case.text));
        for needle in case.gone {
            if out.text.contains(needle) {
                report.note(format!(
                    "{}: {needle:?} überlebt zwischen Umlauten",
                    case.id
                ));
            }
        }
    }

    for (id, text) in MUST_SURVIVE {
        let wrapped = in_multibyte_context(text);
        let out = pipeline.redact(&wrapped);
        if !out.text.contains(*text) {
            report.note(format!(
                "{id}: wird zwischen Umlauten redigiert:\n    {}",
                out.text
            ));
        }
    }

    report.finish("Der Korpus verhält sich in Mehrbyte-Umgebung anders");
}

#[test]
fn every_known_token_format_has_a_fixture() {
    // Eine neue Token-Form ohne Fixture ist eine ungetestete Form. Die Fixture-Ids
    // sind deshalb identisch mit den Regelnamen.
    let ids: Vec<&'static str> = must_redact().iter().map(|c| c.id).collect();
    let mut report = Report::default();

    for format in KnownTokenRedactor::new().covered_formats() {
        if !ids.contains(&format) {
            report.note(format!("Token-Form {format:?} hat kein Korpus-Fixture"));
        }
    }

    report.finish("Token-Formen ohne Korpus-Abdeckung");
}

// ---------------------------------------------------------------------------
// Der Korpus durch die anderen beiden Schichten
// ---------------------------------------------------------------------------

#[test]
fn the_whole_corpus_as_one_session_is_fail_closed() {
    // Derselbe Korpus, aber durch das Envelope: jedes Fixture einmal als
    // Turn-Text und einmal als Tool-Argument. Damit ist zugleich geprüft, dass
    // `redact_session` auf realistischem Text weder abbricht noch etwas übersieht.
    let cases = must_redact();

    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "1.0.0".into(),
        },
        Model {
            provider: "anthropic".into(),
            id: "claude-opus-4".into(),
        },
        Intent {
            request: "Deploy-Pipeline reparieren".into(),
            ..Intent::default()
        },
    );
    for case in &cases {
        session.turns.push(Turn {
            role: Role::Assistant,
            text: case.text.clone(),
            tool_calls: vec![ToolCall {
                name: "bash".into(),
                arguments: case.text.clone(),
                effect: None,
            }],
            parent: None,
            at: None,
        });
    }

    let redacted = policy()
        .redact_session(session)
        .expect("der Korpus muss sich als Session redigieren lassen");

    let mut haystack = String::new();
    for turn in &redacted.session().turns {
        haystack.push_str(&turn.text);
        for call in &turn.tool_calls {
            haystack.push_str(&call.arguments);
        }
    }

    let mut report = Report::default();
    for case in &cases {
        for needle in case.gone {
            if haystack.contains(needle) {
                report.note(format!("{}: {needle:?} steht noch im Envelope", case.id));
            }
        }
    }
    report.finish("Der Korpus leckt durch das Session-Envelope");

    assert!(redacted.session().redaction.applied);
    assert_eq!(
        redacted.session().redaction.counts,
        redacted.audit().counts()
    );
    // Zwei Felder je Fixture (Turn-Text und Tool-Argument) müssen Funde tragen.
    assert!(redacted.audit().fields_changed() >= cases.len() * 2);
}

#[test]
fn the_dotenv_fixture_would_not_even_reach_the_net() {
    // `dotenv-block` prüft das Netz (Schicht 2) an genau dem Inhalt, für den in
    // der echten Capture schon die Mauer (Schicht 1) greift: Der Inhalt einer
    // `.env` wird gar nicht erst gescannt, sondern komplett ausgelassen. Die
    // eingecheckte Vorlage daneben bleibt lesbar — und läuft durchs Netz.
    assert!(is_secret_file("/home/p/projekt/.env"));
    assert!(!is_secret_file("/home/p/projekt/.env.example"));
}

// ---------------------------------------------------------------------------
// Policy-Stellschrauben am Korpus
// ---------------------------------------------------------------------------

#[test]
fn allowlist_releases_a_single_corpus_finding() {
    // Der AWS-Beispielschlüssel steht in AWS' eigener Dokumentation. Wer ihn im
    // Transkript behalten will, trägt ihn ein — und nur ihn.
    let pipeline = RedactionConfig {
        allow: vec![AWS_KEY.into()],
        ..RedactionConfig::default()
    }
    .pipeline()
    .expect("Policy muss bauen");

    let text = format!("aws_access_key_id = {AWS_KEY}");
    let out = pipeline.redact(&text);
    assert_eq!(out.text, text);
    assert_eq!(out.counts, Default::default());

    // Ein anderer Schlüssel derselben Form bleibt geschwärzt.
    let other = "aws_access_key_id = AKIA2E0A8F3B244C9986";
    assert!(!policy().redact(other).text.contains("AKIA2E0A8F3B244C9986"));
}

#[test]
fn the_entropy_threshold_has_measurable_headroom() {
    // Der versprochene Feinschliff aus `secret.rs`: Wie weit ist die Schwelle von
    // den harmlosen Fällen entfernt? Ein Git-SHA liegt bei ~3.8 bit/Zeichen, die
    // Schwelle bei 4.5 — er überlebt mit Luft. Senkt man sie unter seinen Wert,
    // fällt er. Der Abstand ist also gemessen, nicht geraten.
    let sha_line = survivor("git-sha");
    assert_eq!(policy().redact(sha_line).text, sha_line);

    let jumpy = RedactionConfig {
        high_entropy: HighEntropyConfig {
            min_entropy_bits: 3.5,
            ..HighEntropyConfig::default()
        },
        ..RedactionConfig::default()
    }
    .pipeline()
    .expect("Policy muss bauen");
    assert_ne!(
        jumpy.redact(sha_line).text,
        sha_line,
        "unter 3.5 bit/Zeichen müsste der Git-SHA fallen — sonst stimmt die \
         Annahme über den Abstand nicht mehr"
    );
}
