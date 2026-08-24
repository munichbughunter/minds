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
// Ab hier gilt für **jede** Token-Konstante mit erkennbarem Präfix: Sie wird
// per `concat!` aufgebrochen, sodass im Quelltext kein zusammenhängendes
// Token-Literal steht.
//
// Das ist keine Kosmetik. Fremde Secret-Scanner (GitHubs Push-Protection,
// GitLabs Secret Detection) lesen den **Quelltext** und können synthetische
// Fixtures nicht von echten Zugangsdaten unterscheiden — ein Push wird dann
// blockiert. Für den Compiler ist `concat!` dasselbe Literal, die Fixtures
// verlieren also nichts. Bitte nicht zu einem Einzeiler „aufräumen".

/// GitLab-PAT in der **langen** Form, wie sie seit 16.x ausgegeben wird — der
/// alte Cap von exakt 20 Zeichen hätte nur den Anfang gedeckt.
const GITLAB_PAT: &str = concat!("glpat", "-ORS6ilI8ihN5KXSc7TvA1b2C3d4");
/// Die klassische 20-Zeichen-Form, **mit** eingebettetem Bindestrich. Bleibt im
/// Korpus, damit die Cap-Erweiterung die alte Variante nicht verdrängt.
const GITLAB_PAT_CLASSIC: &str = concat!("glpat", "-ORS-6ilI8ihN5KXSc7Tv");
/// Der Key, der für dieses Projekt am meisten zählt: Die Testgruppe arbeitet
/// mit Claude Code, ein `sk-ant-` steht in echten Sessions.
const ANTHROPIC_KEY: &str = concat!(
    "sk-ant",
    "-api03-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"
);
const OPENAI_KEY: &str = concat!("sk-proj", "-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0");
/// Legacy-Registrierungstoken für GitLab-Runner.
const RUNNER_REGISTRATION: &str = concat!("GR13489", "41K7pR2xQm9vTzL4nB6wYd");
const SENDGRID_KEY: &str = concat!(
    "SG",
    ".A1b2C3d4E5f6G7h8I9j0K1.A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0"
);
/// Slack-Bot-Token in realistischer **Länge** — über dem alten Cap von 48.
const SLACK_TOKEN: &str = concat!(
    "xoxb",
    "-1234567890123-1234567890123-NhFdnXsiVpzz63FfkCzJrA1b2"
);
const GOOGLE_API_KEY: &str = "AIzar3J1TWDtkwtDDb_xHKas1VOqg6YYZYn9Zhy";
/// Stripe-Key in der langen Variante — der alte Cap von 64 hätte den Schwanz
/// am Entropie-Netz hängen lassen.
const STRIPE_KEY: &str = concat!(
    "sk_live",
    "_enCkhvMdgaKjIg8xNbe3nNyjA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"
);
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
/// Derselbe Schlüssel, wie er im **Envelope wirklich steht**: als Inhalt eines
/// JSON-Strings, also mit literalem `\n` statt echtem Zeilenumbruch.
///
/// Tool-Argumente liegen immer JSON-serialisiert vor — das ist der Hauptkanal
/// des Systems, nicht ein Randfall. Die letzte Körperzeile ist **absichtlich
/// kurz** (unter der Entropie-Schwelle von 32 Zeichen): Fängt die PEM-Regel den
/// Block nicht als Ganzes, bleibt genau sie stehen, und das ist echtes
/// Schlüsselmaterial.
const PEM_KEY_IN_JSON: &str = concat!(
    "-----BEGIN RSA PRIVATE KEY-----\\n",
    "MIIEowIBAAKCAQEAx7Vn9pQmKtLbYcHZfWjRuEoNsAiPdGqTvXlMzBhKrCyWnUeF\\n",
    "gJdSaOvTpQiXmZbNlHrKcYtEuAfWdGxPjRoLsVnBqMzCyIeUkTaFhDpWvNgXlOrJ\\n",
    "qZ8Wn3xKtLmPvRc=\\n",
    "-----END RSA PRIVATE KEY-----"
);
/// Die kurze Schlusszeile aus [`PEM_KEY_IN_JSON`], einzeln benannt: Sie ist der
/// Teil, den das Entropie-Netz nicht auffängt.
const PEM_SHORT_TAIL: &str = "qZ8Wn3xKtLmPvRc=";

/// Der **verschlüsselte** PEM nach RFC 1421 — mit Kopfzeilen zwischen BEGIN und
/// Körper. Steht in [`DOCUMENTED_GAPS`], weil die PEM-Regel ihn nicht fängt.
const ENCRYPTED_PEM_KEY: &str = concat!(
    "-----BEGIN RSA PRIVATE KEY-----\n",
    "Proc-Type: 4,ENCRYPTED\n",
    "DEK-Info: AES-128-CBC,7B3A9C2E5F1D8046A2B4C6E8F0A1B3C5\n",
    "\n",
    "MIIEowIBAAKCAQEAx7Vn9pQmKtLbYcHZfWjRuEoNsAiPdGqTvXlMzBhKrCyWnUeF\n",
    "qZ8Wn3xKtLmPvRc=\n",
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
            // Der generische Fallback (ADR-0011) friert ganze
            // Fremd-Agent-Payloads als `arguments` ein — der
            // KeyValue-Detektor muss ein Credential auch in der
            // verschachtelten JSON-Form fangen.
            id: "generic-fallback-nested-basic-auth",
            text: r#"{"tool_name":"http_request","tool_input":{"headers":{"Authorization":"Basic dXNlcjpwdw=="}}}"#.to_string(),
            gone: &["dXNlcjpwdw=="],
            kept: &["http_request", "headers"],
        },
        MustRedact {
            // Der Blocker-Fall aus dem Audit-Bundle: eine Remote-URL mit
            // eingebetteten Zugangsdaten. Die Senke (`without_url_credentials`)
            // redigiert im Bundle; die Pipeline muss den Token auch fangen,
            // wenn der Text anderswo auftaucht (Prompt, Log-Zitat).
            id: "origin-remote-with-token",
            text: concat!(
                "origin  https://oauth2:glpat",
                "-AbCdEf123456789012@gitlab.example.com/group/repo.git (fetch)"
            )
            .to_string(),
            // Die Pipeline schwärzt die ganze Autorität samt Host mit —
            // Over-Redaction, hier akzeptabel: Hauptsache, der Token fällt.
            gone: &[concat!("glpat", "-AbCdEf123456789012")],
            kept: &["origin", "(fetch)"],
        },
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
            // Beide Formen in einem Fixture: die lange seit 16.x und die
            // klassische mit eingebettetem Bindestrich.
            id: "gitlab-pat",
            text: format!("PRIVATE-TOKEN: {GITLAB_PAT}\nalter Token: {GITLAB_PAT_CLASSIC}"),
            gone: &[GITLAB_PAT, GITLAB_PAT_CLASSIC],
            kept: &["PRIVATE-TOKEN:", "alter Token:"],
        },
        MustRedact {
            // Bewusst **ohne** Zuweisung: In einer Fehlermeldung gibt es keinen
            // Schlüsselnamen, an dem sich der Key-Value-Detektor festhalten
            // könnte. Was hier greift, ist die Token-Form selbst.
            id: "anthropic-api-key",
            text: format!("Fehler 401: der Key {ANTHROPIC_KEY} wurde abgelehnt"),
            gone: &[ANTHROPIC_KEY, "api03"],
            kept: &["Fehler 401:", "wurde abgelehnt"],
        },
        MustRedact {
            id: "openai-api-key",
            text: format!("Fehler 401: der Key {OPENAI_KEY} wurde abgelehnt"),
            gone: &[OPENAI_KEY],
            kept: &["Fehler 401:", "wurde abgelehnt"],
        },
        MustRedact {
            id: "sendgrid-key",
            text: format!("Fehler 403: {SENDGRID_KEY} hat keine Rechte"),
            gone: &[SENDGRID_KEY],
            kept: &["Fehler 403:", "hat keine Rechte"],
        },
        MustRedact {
            // Legacy-Registrierungstoken für Runner, fester Präfix.
            id: "gitlab-runner-registration",
            text: format!("Runner meldet {RUNNER_REGISTRATION} zurueck"),
            gone: &[RUNNER_REGISTRATION],
            kept: &["Runner meldet", "zurueck"],
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
            // Derselbe Schlüssel im Hauptkanal: als JSON-serialisiertes
            // Tool-Argument, mit literalem `\n` statt Zeilenumbruch.
            id: "private-key-pem-in-json",
            text: format!(r#"{{"command":"cat deploy.key","output":"{PEM_KEY_IN_JSON}"}}"#),
            gone: &[
                "MIIEowIBAAKCAQEA",
                "gJdSaOvTpQiXmZbN",
                // Die kurze Schlusszeile: unter der Entropie-Schwelle und
                // deshalb der Teil, der ohne PEM-Treffer stehen bliebe.
                PEM_SHORT_TAIL,
                // Beweist, dass die **PEM-Regel** gegriffen hat und nicht nur
                // das Entropie-Netz die langen Zeilen einzeln erwischte.
                "BEGIN RSA PRIVATE KEY",
            ],
            kept: &[r#""command""#],
        },
        MustRedact {
            // Ein Passwort mit `"` darin — im JSON-String steht davor ein
            // Backslash. Endet der Fund am escapten Quote, bleibt der Rest
            // des Passworts stehen.
            id: "json-escaped-quote-in-secret",
            text: r#"{"password": "hun\"ter2", "host": "db.internal", "port": 5432}"#.into(),
            gone: &["ter2", "hun"],
            kept: &[r#""password""#, "db.internal", "5432"],
        },
        MustRedact {
            // Die Normalform verschachtelter Tool-Argumente: Ein Bash-Aufruf
            // schreibt JSON, also ist im Envelope **jedes** Quote escapt — auch
            // das des Schlüssels (`\"password\":`). Scheitert der Trenner am
            // Backslash vor dem Doppelpunkt, matcht die ganze Regel nicht und
            // der Wert steht vollständig im Record.
            id: "double-escaped-json-argument",
            text: r#"{"command":"curl -d '{\"password\": \"hunter2\", \"host\": \"db\"}' https://api.test"}"#
                .into(),
            gone: &["hunter2"],
            kept: &["curl -d", "https://api.test"],
        },
        MustRedact {
            // Shaped-Tier plus Shell-Zeilenfortsetzung: `mysecretkey\` besteht
            // die Shape-Prüfung, `mysecretkey` nicht. Verliert der Fund den
            // End-Backslash, kippt der Wert von „redigiert" auf „exempt".
            id: "shell-continuation-shaped-token",
            text: "docker run -e API_KEY=mysecretkey\\\n  -e OTHER=1".into(),
            gone: &["mysecretkey"],
            kept: &["docker run", "-e OTHER=1"],
        },
        MustRedact {
            // Ein Passwort, das nach dem Mitlesen der Escapes wie ein Pfad
            // aussieht (`/…/…`). Die Pfad-Ausnahme darf darauf nicht
            // hereinfallen, sonst bleibt der Wert ganz stehen.
            id: "json-escaped-quote-in-path-like-secret",
            text: r#"{"password": "/pa\"ss/word", "host": "db.internal"}"#.into(),
            gone: &["ss/word"],
            kept: &["db.internal"],
        },
        MustRedact {
            // Derselbe Mechanismus über den Backslash statt das Quote: auch
            // `/pa\ss/word` sähe wie ein Pfad aus. Ein echter POSIX-Pfad trägt
            // keinen Backslash.
            id: "json-escaped-backslash-in-path-like-secret",
            text: r#"{"password": "/pa\\ss/word", "host": "db.internal"}"#.into(),
            gone: &["ss/word"],
            kept: &["db.internal"],
        },
        MustRedact {
            // Und über den zweiten Ausnahme-Zweig: Der verlängerte Wert beginnt
            // und endet auf `%` und sähe damit wie eine Variablenreferenz aus.
            // Eine echte Referenz enthält keinen Leerraum.
            id: "percent-shaped-value-with-escaped-space",
            text: r"PASSWORD=%hunterzwei\ x%".into(),
            gone: &["hunterzwei"],
            kept: &["PASSWORD="],
        },
        MustRedact {
            // Dreifach serialisiert: ein Transkript, das selbst wieder als
            // JSON-String weitergereicht wurde.
            id: "triple-escaped-json-argument",
            text: r#"{"log":"{\\\"password\\\": \\\"hunter2\\\"}"}"#.into(),
            gone: &["hunter2"],
            kept: &[r#""log""#],
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
        MustRedact {
            // Short-Flag-Authentifizierung (#2): Tool-Calls schießen
            // Shell-Kommandos ab, und `curl -u` ist die kürzeste Form.
            id: "curl-basic-auth",
            text: "curl -u admin:hunter2 https://api.example.test/v1".into(),
            gone: &["admin:hunter2"],
            kept: &["curl", "https://api.example.test/v1"],
        },
        MustRedact {
            // Dieselbe .env mit CRLF-Zeilenenden — Windows-Editoren und
            // manche Agents liefern genau das. Kein Detektor darf am `\r`
            // scheitern oder darüber hinweglesen.
            id: "dotenv-crlf",
            text: DOTENV.replace('\n', "\r\n"),
            gone: &["hunter2", "Sommer2024!", "abc123", "s3cr3t", "admin"],
            kept: &["DB_PASSWORD=", "postgres://"],
        },
        MustRedact {
            // Multibyte **im** Wert, nicht nur drumherum: Der Panic aus #1
            // (`is_filesystem_path` schnitt in ein Mehrbyte-Zeichen) blieb
            // unentdeckt, weil `in_multibyte_context` nur außen wickelt.
            id: "multibyte-inside-secret",
            text: "PASSWORD=hünter€2 und TOKEN=abc€123def456".into(),
            gone: &["hünter€2", "abc€123def456"],
            kept: &["PASSWORD=", "TOKEN="],
        },
        MustRedact {
            // Die Exemption stellt nur den Platzhalter *selbst* frei — ein
            // echtes Geheimnis daneben wird weiter redigiert.
            id: "placeholder-does-not-shield-neighbours",
            text: "PASSWORD=[redacted:secret] TOKEN=abc123def456".into(),
            gone: &["abc123def456"],
            kept: &["[redacted:secret]"],
        },
        MustRedact {
            // Ein Geheimnis, an das der Marker angeklebt ist, ist nicht der
            // Platzhalter — der Exakt-Vergleich lässt es nicht durch.
            id: "secret-with-appended-marker",
            text: "PASSWORD=hunter2[redacted:secret]".into(),
            gone: &["hunter2"],
            kept: &[],
        },
        MustRedact {
            // Ein Trennzeichen hinter dem Marker verhindert den Exakt-Match
            // (der Wert ist `[redacted:secret]-hunter2`, nicht der Platzhalter):
            // Das Geheimnis dahinter muss verschwinden.
            id: "marker-with-trailing-separator",
            text: "PASSWORD=[redacted:secret]-hunter2".into(),
            gone: &["hunter2"],
            kept: &[],
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
    // --- Das Evidence-Chain-Vokabular (ADR-0011) bleibt lesbar --------------
    // Ein voller Seal-Text, in Prosa zitiert (Log-Ausschnitt): Das
    // Beweismittel darf die Redaction nicht anfressen.
    (
        "seal-text-in-prose",
        "aus dem Log: minds-seal-v1 root=b3-1676980fced8f11c73cc9ed58294c90c9c141ad6fb0c1a8004c86c7dc666a685 agent=claude-code scope=agent-hooks/v1 outcome=storage_policy_rejected_payload session=- previous=-",
    ),
    // Die neue Kanten-Objektform: Vokabular, kein Geheimnis.
    (
        "evidence-mark-object",
        r#"{"source":"observed","status":"unknown"}"#,
    ),
    // Der Capture-Stempel des generischen Fallbacks.
    (
        "capture-stamp",
        r#"{"status":"uninterpreted","adapter":"generic","adapter_version":1}"#,
    ),
    // Eine Seal-Id in Freitext: Hex traegt hoechstens 4 bit je Zeichen und
    // muss unter der Entropieschwelle bleiben — wie SessionIds.
    (
        "seal-id-in-prose",
        "der Seal b3-4a6388a9ea6622b9e2dc77df88c85311890dc62b1fde9692b3211dc74e71e65f deckt Epoche 2",
    ),
    // --- Bereits redigierte Platzhalter bleiben ein Fixpunkt ----------------
    // Zwei benachbarte Platzhalter verschiedener Kategorie ohne Trenner: Keiner
    // wird umgeschrieben, der Ein-Pass-Fixpunkt hält (Zähler bleibt null).
    ("adjacent-placeholders", "[redacted:secret][redacted:pii]"),
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
    (
        // Gegenprobe zur Leerraum-Absage in `is_variable_reference`: Die
        // Ausnahme muss im JSON-Kanal erhalten bleiben.
        "percent-reference-in-json",
        r#"{"password": "%DEPLOY_PW%"}"#,
    ),
    (
        // Gegenprobe zum Escape-Guard vor den Ausnahmen: Ein Windows-Pfad
        // besteht aus Backslashes — er darf davon nicht mitgerissen werden.
        "windows-path-in-json-argument",
        r#"{"token_file": "C:\keys\deploy.tok"}"#,
    ),
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
        // `sk-` ist als Präfix bewusst nur mit voller Länge gültig, sonst
        // träfe es jede Abkürzung mit Bindestrich.
        "near-miss-openai",
        "sk-learn und sk-image sind Python-Pakete",
    ),
    (
        // Der Anthropic-Präfix ohne plausiblen Rest bleibt Prosa.
        "near-miss-anthropic",
        "das Praefix sk-ant- steht in der Doku",
    ),
    (
        // `SG.` verlangt zwei durch Punkt getrennte Abschnitte; ein Satzanfang
        // erfüllt das nicht.
        "near-miss-sendgrid",
        "SG. Mueller hat den Termin bestaetigt",
    ),
    (
        // Die neuen GitLab-Präfixe brauchen ebenfalls Mindestlänge.
        "near-miss-gitlab-runner",
        "glrt-kurz steht im Runner-Log",
    ),
    ("near-miss-gitlab-cbt", "glcbt-kurz steht im Job-Log"),
    (
        // Gegenprobe zu den vielen neuen `gl…`-Präfixen: Projektnamen bleiben.
        "gitlab-project-slug",
        "gruppe/gl-runner-config und gl-agent-doku bleiben Prosa",
    ),
    (
        // `sk-` steht am Ende gewöhnlicher Wörter. Ohne Wortanfang-Prüfung
        // schwärzt eine lange Build-Id hier die halbe Zeile.
        "near-miss-task-id",
        "Build task-0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071 fertig",
    ),
    (
        "near-miss-disk-id",
        "disk-aaaaaaaabbbbccccddddeeeeeeeeeeeeaaaaaaaabbbbcccc gemountet",
    ),
    (
        // `SG.` mitten im Wort.
        "near-miss-msg-prefix",
        "MSG.AbCdEfGhIjKlMnOpQr.AbCdEfGhIjKlMnOpQrStUv im Log",
    ),
    (
        // Über Keys wird geredet — in Claude-Code-Sessions ständig. Ohne die
        // Typ-Sektion im Muster wäre das ein Treffer.
        "prose-about-anthropic-keys",
        "Doku: https://docs.example.test/sk-ant-api-keys-rotieren-anleitung",
    ),
    (
        "prose-about-slack-app",
        "Job xapp-android-build-pipeline-nightly ist rot",
    ),
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
    // --- Envelope-Felder, die seit #35 mitgescannt werden -------------------
    //
    // Sie stehen dort **nackt**, ohne Prosa drumherum — deshalb genügen die
    // bestehenden Einträge mit Präfix (`hash: …`, `request_id: …`) nicht. Wird
    // hier etwas rot, verliert eine Session ihren Zeitstempel oder ihre
    // Agent-Identität, und zwar lautlos.
    ("rfc3339-utc", "2026-07-23T09:12:04.512Z"),
    ("rfc3339-offset", "2026-07-23T11:12:04+02:00"),
    ("session-uuid-bare", "31f3f224-f440-41ac-9bfa-0123456789ab"),
    (
        "commit-endpoint-sha",
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    ),
    ("agent-endpoint-name", "claude-code"),
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
    (
        // Der Preis der Escape-Alternative im nackten Wert: `\ ` gehört zum
        // Geheimnis (`hun\ ter2`), also muss der Fund darüber hinweglesen —
        // folgt dahinter Prosa, wandert genau ein Wort mit. Ein Wort zu viel
        // ist der Preis dafür, `ter2` nicht stehen zu lassen.
        //
        // Steht in diesem Wort ein **weiterer sensibler Schlüssel**, wird der
        // mitverschluckt und nicht mehr geprüft — das ist kein Schönheits-
        // fehler mehr, sondern die Lücke `escaped-space-swallows-next-key`
        // in [`DOCUMENTED_GAPS`].
        "escaped-space-costs-one-word",
        r"PASSWORD=abc\ und der Rest bleibt",
        "PASSWORD=[redacted:secret] der Rest bleibt",
    ),
    (
        // Seit #35 läuft auch `lineage.local_id` durch die Pipeline. Eine
        // UUID bleibt stehen (Entropie ~3.4 bit), eine 32-stellige
        // base62-Kennung nicht — ein Agent mit dieser Konvention verliert
        // damit seine Kennung im Record.
        //
        // Der Preis ist bewusst: Die Alternative wäre, dem fremden Adapter zu
        // glauben, dass dort nie etwas anderes steht. Sobald der Store
        // symbolische Endpunkte auflöst, darf eine geschwärzte Kennung
        // allerdings **nicht** als gültiger Schlüssel gelten.
        "base62-session-id",
        "aB3xZ9qLmN7pQrS2tUvW4yZ6cD8eF0gH",
        "[redacted:secret]",
    ),
    (
        // Unsauber escaptes JSON: Das Quote hinter `abc\` beendet den Wert
        // nicht mehr, also läuft der Fund bis zum nächsten. Der Nachbar
        // verliert sein öffnendes Quote — der Record wird an der Stelle
        // strukturell schief, aber es leckt nichts.
        "malformed-json-eats-separator",
        r#"{"password": "abc\", "host": "db.internal"}"#,
        r#"{"password": "[redacted:secret]"host": "db.internal"}"#,
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
    (
        // Ein nackter Wert, der auf `\ ` endet, liest über den Leerraum hinweg
        // — und verschluckt dabei die **nächste Zuweisung derselben Regel**.
        // `captures_iter` setzt hinter dem Fund wieder auf, der zweite
        // Schlüssel wird also nie geprüft.
        //
        // Der Preis ist bewusst: Ohne die Escape-Alternative bliebe bei
        // `PASSWORD=hun\ ter2` das `ter2` stehen — der häufigere Fall. Sauber
        // lösen ließe sich beides nur, indem die Suche je Regel hinter dem
        // *Wert-Anfang* fortsetzt statt hinter dem Fund; das ändert die
        // Laufzeit-Charakteristik und gehört in ein eigenes Issue.
        "escaped-space-swallows-next-key",
        r"PASSWORD=abc\ SECRET: hunter2",
        "hunter2",
    ),
    (
        // Abgeschnittene Tool-Ausgabe: Der Wert hat kein Schluss-Quote, also
        // greift weder `dq` noch `sq`, und `bare` darf nicht mit `"` beginnen.
        // Unverändert gegenüber dem Stand vor den Escape-Alternativen.
        "truncated-json-value",
        r#"{"password": "hunter2"#,
        "hunter2",
    ),
    (
        // Zitiert ein Assistant den Inhalt einer Mauer-Datei in **Prosa**,
        // sieht die pfadbasierte Mauer ihn nicht, und ein patternfreier Wert
        // entgeht auch jedem Detektor. Gilt auf beiden Eingangswegen (Hook wie
        // Import) gleichermaßen — seit #93 ist der Tool-Call-Weg dicht, dieser
        // Prosa-Weg bleibt die benannte Restlücke.
        "assistant-prose-echoes-walled-file",
        "Die Datei .vault_pass enthaelt: korrekt-pferd-batterie-heftklammer",
        "korrekt-pferd-batterie-heftklammer",
    ),
    (
        // Shaped-Tier: Werte unter 8 **Bytes** gelten nicht als
        // credential-typisch (`has_credential_shape`, Filter 1) — ein kurzes
        // echtes Token hinter `TOKEN=` bleibt also stehen. Die Schwelle auf 6
        // zu senken wäre eine Policy-Änderung mit eigener Fehlalarm-Abwägung
        // (`token: v1.2.3` würde dann verschwinden) und gehört in ein eigenes
        // Issue, nicht in ein Test-Issue. Wem die Lücke zu groß ist: `token`
        // in `secret_keys` hebt sie in den Strict-Tier.
        "short-shaped-token",
        "GITLAB_TOKEN=abc123",
        "abc123",
    ),
    (
        // Ein **ungeescaptes** Quote mitten im nackten Wert beendet den Fund:
        // `abc` wird redigiert, `'def` bleibt stehen. Das Quote schließt in
        // JSON einen String, deshalb steht der Wert dort escapt (`\'`, den die
        // Escape-Alternative fängt) — nackt, ohne JSON, ist es die Grenze der
        // bare-Klasse. Verwandt mit `truncated-json-value`, hier für den
        // Nicht-JSON-Fall festgehalten.
        "unescaped-quote-truncates-bare-value",
        "PASSWORD=abc'def",
        "def",
    ),
    (
        // Der **verschlüsselte** PEM nach RFC 1421 trägt zwischen BEGIN und
        // Körper zwei Kopfzeilen (`Proc-Type: 4,ENCRYPTED`, `DEK-Info: …`).
        // Deren `:` und `,` stehen nicht in der Körperklasse, also greift die
        // PEM-Regel nicht; das Entropie-Netz fängt nur die langen Zeilen, und
        // die kurze Schlusszeile bleibt stehen.
        //
        // Nicht durch Erweitern der Körperklasse geschlossen: `:` und `,`
        // dort aufzunehmen macht die Regel über mehrere Blöcke hinweg gierig.
        // Der saubere Weg ist eine eigene Regel für den Header — eigenes Issue.
        "encrypted-pem-headers",
        ENCRYPTED_PEM_KEY,
        PEM_SHORT_TAIL,
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

#[test]
fn every_token_fixture_is_caught_by_the_token_rule_alone() {
    // Der Namensabgleich oben beweist nur, *dass* ein Fixture existiert — nicht,
    // dass die **Token-Regel** es fängt. In der Default-Policy greifen daneben
    // Entropie-Netz und Key-Value-Detektor; ein Fixture kann also grün sein,
    // während seine Regel gar nichts tut.
    //
    // Deshalb hier ein Durchlauf mit **nur** dem Token-Detektor. Erst damit
    // ziehen die längeren Konstanten: Ein Rückbau der geweiteten Caps macht
    // diesen Test rot, den Rest des Korpus nicht.
    // Geprüft wird der **erste** `gone`-Eintrag: Bei den Token-Fixtures ist das
    // per Konvention der Token selbst. Weitere Einträge dürfen zu anderen
    // Schichten gehören — beim GitHub-Fixture etwa das `oauth2` aus der
    // Credential-URL, das der URL-Detektor entfernt.
    let token_only = RedactionPipeline::new().with(KnownTokenRedactor::new());
    let formats = KnownTokenRedactor::new().covered_formats();
    let mut report = Report::default();

    for case in must_redact() {
        if !formats.contains(&case.id) {
            continue; // Fixture einer anderen Schicht (dotenv, URL, …)
        }
        let Some(token) = case.gone.first() else {
            report.note(format!("{}: Fixture ohne gone-Eintrag", case.id));
            continue;
        };
        let out = token_only.redact(&case.text);
        if out.text.contains(token) {
            report.note(format!(
                "{}: {token:?} überlebt den reinen Token-Durchlauf:\n    {}",
                case.id, out.text
            ));
            continue;
        }
        // Der volle String allein genügt als Nachweis nicht: Ein Fund, der nur
        // bis zur ersten Wortgrenze reicht, lässt den **Schwanz** stehen — der
        // Token-String als Ganzes ist dann weg, das Geheimnis aber nicht. Genau
        // dieser Teilleck-Mechanismus ist der Grund für die geweiteten Caps,
        // also wird er hier geprüft.
        let tail = token
            .char_indices()
            .rev()
            .nth(11)
            .map(|(i, _)| &token[i..])
            .unwrap_or(token);
        if out.text.contains(tail) {
            report.note(format!(
                "{}: Ende {tail:?} überlebt — der Fund deckt den Token nur teilweise:\n    {}",
                case.id, out.text
            ));
        }
    }

    report.finish("Token-Formen, die ohne die anderen Schichten nicht greifen");
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
                capture: None,
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

/// Klartext-Werte, wie ein Agent sie als Tool-Argument übergibt — der
/// **rohe** Inhalt, bevor die Erfassung ihn in JSON serialisiert.
///
/// Diese Fälle sind bewusst getrennt von [`must_redact`]: Sie sind noch *kein*
/// JSON. Erst der Test unten packt jeden in `{"command": …}`, wodurch `"` zu
/// `\"` und Zeilenumbrüche zu `\n` mit literalem Backslash werden — genau die
/// Envelope-Form, an der die Redaktion vor #3 scheiterte. Fixtures aus
/// [`must_redact`], die schon JSON tragen (`json-escaped-quote-in-secret`),
/// gehören hier **nicht** herein: Ein zweites Wrapping erzeugte eine doppelte
/// Verschachtelung, die real nicht vorkommt.
const JSON_ARG_CASES: &[(&str, &str, &[&str])] = &[
    (
        // Der eigentliche Bug-Auslöser: `DB_USER=` (Identity, PII) **vor** dem
        // Secret. Nach der JSON-Serialisierung verschmelzen die per `\n`
        // getrennten Zeilen zu einer, eine Secret-Regel gewinnt die überspannte
        // Spanne, und der zweite Verifikationslauf würde den Platzhalter hinter
        // `DB_USER=` zu PII umschreiben — ohne den Idempotenz-Fix als `Unstable`
        // verworfen.
        "dotenv-as-command",
        "printf 'DB_USER=admin\nDB_PASSWORD=hunter2\nTOKEN=abc123def456' > .env",
        &["admin", "hunter2", "abc123def456"],
    ),
    (
        "curl-basic-auth-as-command",
        "curl -u admin:s3cr3tPass https://api.example.test",
        &["admin", "s3cr3tPass"],
    ),
    (
        // Der ShortFlag-Zwilling des Kategorie-Flips: Hinter `-u` gewinnt im
        // ersten Lauf die E-Mail (`[redacted:pii]`), im zweiten würde die
        // ShortFlag-Regel (Secret) den Platzhalter umschreiben. `redact_session`
        // unten verwürfe die Session ohne den zentralen Idempotenz-Filter als
        // `Unstable` — dieser Fall panickt dann statt zu lecken.
        "curl-user-is-an-email",
        "curl -u anna@example.com https://api.example.test",
        &["anna@example.com"],
    ),
    (
        "quoted-password-as-command",
        r#"psql "password=hunter2 host=db.internal""#,
        &["hunter2"],
    ),
    (
        "multibyte-secret-as-command",
        "export PASSWORD=hünter€2",
        &["hünter€2"],
    ),
    (
        "pem-heredoc-as-command",
        "cat <<EOF > id\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAx7Vn9pQmKtLb\n-----END RSA PRIVATE KEY-----\nEOF",
        &["MIIEowIBAAKCAQEAx7Vn9pQmKtLb"],
    ),
];

#[test]
fn a_secret_survives_json_serialization_as_a_tool_argument() {
    // Real sind `ToolCall::arguments` immer ein JSON-Dokument: Hook- wie
    // Import-Weg serialisieren das `tool_input`, und ein Geheimnis darin steht
    // **escapt**. Jeder Fall läuft **einzeln** durch eine eigene Session —
    // sonst ordnete ein gemeinsamer Textpuffer einen überlebenden Needle dem
    // falschen Fixture zu.
    let mut report = Report::default();

    for (id, plaintext, gone) in JSON_ARG_CASES {
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
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
        session.turns.push(Turn {
            role: Role::Assistant,
            text: String::new(),
            tool_calls: vec![ToolCall {
                capture: None,
                name: "Bash".into(),
                arguments: serde_json::json!({ "command": plaintext }).to_string(),
                effect: None,
            }],
            parent: None,
            at: None,
        });

        // `redact_session` erhebt eine instabile Redaktion (Kategorie-Flip auf
        // einem Platzhalter) zum harten Fehler — das Fixture prüft also
        // zugleich, dass dieser envelope-realistische Fall stabil bleibt.
        let redacted = policy()
            .redact_session(session)
            .unwrap_or_else(|e| panic!("{id}: Session nicht redigierbar: {e:?}"));
        let out = &redacted.session().turns[0].tool_calls[0].arguments;

        for needle in *gone {
            let quoted = serde_json::to_string(needle).expect("String serialisiert immer");
            let escaped = &quoted[1..quoted.len() - 1];
            if out.contains(needle) || out.contains(escaped) {
                report.note(format!(
                    "{id}: {needle:?} überlebt die JSON-Serialisierung:\n    {out}"
                ));
            }
        }
    }

    report.finish("Ein Geheimnis leckt durch ein JSON-serialisiertes Tool-Argument");
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

// ---------------------------------------------------------------------------
// Property-Test: die Grundinvarianten über zufälligem Input
// ---------------------------------------------------------------------------
//
// Der Korpus prüft benannte Fälle. Ein Fuzz prüft die *Invarianten*, die für
// **jeden** Input gelten müssen — und findet die Eingabe, an die niemand
// gedacht hat. Der Multibyte-Panic aus #1 hätte so gefunden werden können,
// bevor er in Produktion auffiel.
//
// Bewusst ohne `proptest`: Ein handgerollter, **deterministisch geseedeter**
// Generator ist in CI reproduzierbar (dieselbe Zeichenkette bei jedem Lauf) und
// braucht keine neue Dependency (vgl. #44, musl-static).

/// splitmix64 — ein winziger, deterministischer PRNG. Fester Seed heißt: Findet
/// die CI eine Gegeneingabe, findet der nächste lokale Lauf sie auch.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Ein Alphabet, das die Bruchstellen der Detektoren trifft: ASCII-Struktur
/// (`= : / @ " ' \`), Zeilenenden (`\n \r`) **und** Multibyte-Zeichen — an
/// denen die Byte-rechnenden Regeln auf Zeichengrenzen achten müssen.
const FUZZ_CHARS: &[&str] = &[
    "a", "B", "9", "_", "-", ".", " ", "=", ":", "/", "@", "%", "$", "\"", "'", "\\", "\n", "\r",
    "€", "ä", "ö", "ü", "🦀", "中", "\u{200B}",
];

fn random_noise(rng: &mut Rng, len: usize) -> String {
    (0..len)
        .map(|_| FUZZ_CHARS[rng.below(FUZZ_CHARS.len())])
        .collect()
}

/// Umschließt Rauschen in einer der realen Envelope-Formen, damit der Fuzz die
/// **Wert-Parser** erreicht — `is_filesystem_path`, `has_credential_shape`, die
/// Escape-Behandlung. Reines Rauschen aus [`FUZZ_CHARS`] bildet nie einen
/// Schlüsselnamen und liefe an genau diesen Stellen vorbei.
fn wrap_in_template(rng: &mut Rng, noise: &str) -> String {
    match rng.below(6) {
        0 => format!("PASSWORD={noise}"),
        1 => format!("TOKEN={noise}"),
        2 => format!("--password {noise}"),
        3 => format!("curl -u {noise} https://h"),
        4 => format!("scheme://{noise}@host/pfad"),
        _ => format!(r#"{{"password": "{noise}"}}"#),
    }
}

#[test]
fn fuzz_the_pipeline_never_panics_and_keeps_its_contract() {
    // Die Grundinvariante: Kein UTF-8-Input bringt die Pipeline zum Absturz —
    // nicht durch einen Byte-Slice mitten in ein Mehrbyte-Zeichen (#1), nicht
    // durch einen vertragswidrigen Span. Geprüft wird beides: `redact` panickt
    // nicht, **und** meldet keinen ungültigen Fund (`invalid_findings`) — genau
    // die Span-Vertragsbrüche, die der Fuzz jagt, würde die Pipeline sonst
    // still verwerfen und zählen.
    let pipeline = policy();
    let mut rng = Rng(0x5EED_1234_ABCD_0001);

    for _ in 0..30_000 {
        let noise_len = rng.below(48);
        let noise = random_noise(&mut rng, noise_len);
        // Zur Hälfte nackt (trifft die äußeren Grenzen), zur Hälfte in eine
        // Template-Form, die bis zu den Wert-Parsern durchdringt.
        let input = if rng.below(2) == 0 {
            noise
        } else {
            wrap_in_template(&mut rng, &noise)
        };

        let out = pipeline.redact(&input);
        assert_eq!(
            out.invalid_findings, 0,
            "Detektor-Vertrag verletzt auf {input:?}"
        );
        // Idempotenz: Ein zweiter Lauf über das Ergebnis verändert nichts mehr —
        // sonst würde `redact_session` diese Eingabe als instabil verwerfen.
        // Genau diese Invariante hat der Platzhalter-Kategorie-Flip gebrochen.
        let again = pipeline.redact(&out.text);
        assert_eq!(again.text, out.text, "nicht idempotent auf {input:?}");
    }
}

#[test]
fn fuzz_an_injected_secret_never_survives() {
    // Die zweite Invariante: Ein Geheimnis hinter einem eindeutigen Schlüssel
    // verschwindet, **egal** was für Rauschen davor und dahinter steht — auch
    // Multibyte, Escapes und Zeilenumbrüche. Geprüft in beiden Tiers: Strict
    // (`PASSWORD=`) fasst jeden Wert, Shaped (`TOKEN=`) nur credential-förmige,
    // und das injizierte Geheimnis ist genau das (alphanumerisch mit Ziffer).
    const SECRET: &str = "hunter2SECRETvalue9";
    let pipeline = policy();
    let mut rng = Rng(0x5EED_1234_ABCD_0002);

    for _ in 0..30_000 {
        let before_len = rng.below(24);
        let before = random_noise(&mut rng, before_len);
        let after_len = rng.below(24);
        let after = random_noise(&mut rng, after_len);
        let key = if rng.below(2) == 0 {
            "PASSWORD"
        } else {
            "TOKEN"
        };
        // Trenner vor dem Schlüssel: Die Key-Muster sind links unverankert,
        // ein direkt angeklebtes Rauschzeichen (`xPASSWORD=`) träfe zwar
        // trotzdem — der Space hält den Fall aber eindeutig und die
        // Fehlermeldung lesbar.
        let input = format!("{before} {key}={SECRET} {after}");
        let out = pipeline.redact(&input);

        assert!(
            !out.text.contains(SECRET),
            "injiziertes Geheimnis überlebt:\n  ein: {input:?}\n  aus: {:?}",
            out.text
        );
    }
}
