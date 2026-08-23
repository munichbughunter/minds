//! Fremder Text, der auf ein Terminal oder in eine Datei darf.
//!
//! Das Entschärfen selbst — ANSI, Bidi, Zeilentrenner, unsichtbare Träger —
//! lebt seit der TUI in `minds_reader::text`, damit Reader und TUI dieselben
//! Zeichen dieselbe Weise zeigen; hier wird es nur reexportiert. Was bleibt,
//! ist die CLI-eigene Senke für Zugangsdaten in URLs.

pub(crate) use minds_reader::{sanitize, sanitize_path};

/// Nimmt Zugangsdaten aus URLs, lässt aber den Rest der Meldung stehen.
///
/// Der Anlass: `git push` schreibt die Remote-URL in seine Fehlermeldung, und
/// steht darin ein Token (`https://glpat-…@gitlab.com/…`), landete es über den
/// Umweg der Fehlermeldung in `hook.log` — einer Datei, auf die `minds fsck`
/// aktiv verweist und die in einem Bug-Report mitgeschickt wird. Redigiert wird
/// deshalb **an der Senke**: `hooklog::entry` ruft diese Funktion für jede
/// Zeile auf, egal wer sie schreibt (#92). `sync` redigiert seine Push-Fehler
/// zusätzlich an der Quelle, weil sie auch an stderr und an den Aufrufer
/// gehen — ein zweiter Lauf ändert nichts mehr, die Funktion ist idempotent.
///
/// Geschnitten wird nur der Autoritätsteil zwischen `://` und `@`. Host und
/// Pfad bleiben stehen — ohne sie wäre die Diagnose wertlos, und genau dafür
/// gibt es die Datei.
///
/// Danach folgen zwei weitere Durchgänge:
///
/// 1. [`without_query_credentials`] über den **Query-Teil** — `?private_token=…`
///    trägt bei GitLab dasselbe Geheimnis, hat aber kein `@` und käme sonst
///    wörtlich durch.
/// 2. [`without_known_tokens`] als **Auffangnetz** über den ganzen Text, für
///    alles, was in keiner der beiden Strukturen steht: ein Token im Pfad
///    (`/api/v4/glpat-…/x`), im Fragment (`#access_token=…`, der
///    OAuth-Implicit-Flow) oder mitten in einer `remote:`-Zeile.
///
/// Redigiert wird dabei auch [`Category::Pii`](minds_redact::Category) —
/// `?login=…`, `?username=…`. Das ist gewollt: Die Datei geht in Bug-Reports
/// mit, und ein Login-Name benennt eine Person.
pub(crate) fn without_url_credentials(text: &str) -> String {
    let stripped = without_authority_credentials(text);
    without_known_tokens(&without_query_credentials(&stripped))
}

/// Der Autoritätsteil zwischen `://` und `@`.
fn without_authority_credentials(text: &str) -> String {
    const MARK: &str = "://";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find(MARK) {
        let (before, tail) = rest.split_at(at + MARK.len());
        out.push_str(before);

        // Die Autorität endet beim ersten Zeichen, das nicht mehr zu ihr
        // gehört. Ein `@` dahinter ist keins mehr — deshalb wird nur innerhalb
        // dieser Spanne gesucht.
        let end = tail
            .find(|c: char| {
                matches!(c, '/' | '?' | '#' | '\'' | '"' | '<' | '>') || c.is_whitespace()
            })
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at_sign) => {
                out.push_str("…@");
                rest = &tail[at_sign + '@'.len_utf8()..];
            }
            // Kein `@` in der Autorität, also keine Zugangsdaten. Der Rest
            // bleibt wörtlich — `https://gitlab.com:8443/x` ist ein Port, kein
            // Geheimnis, und eine Diagnose ohne Host ist keine.
            None => {
                out.push_str(authority);
                rest = &tail[end..];
            }
        }
    }

    out.push_str(rest);
    out
}

/// Die Policy, mit der Query-Parameter beurteilt werden — einmal gebaut.
///
/// Bewusst die **Default**-Policy und nicht die des Repos: Diese Funktion läuft
/// im Fehlerpfad, teils bevor überhaupt feststeht, ob eine Repo-Konfiguration
/// lesbar ist. Eine deterministische Policy ist dort mehr wert als eine
/// konfigurierbare.
///
/// # Warum `token` hier in den Strict-Tier gehoben wird
///
/// In der Default-Policy liegt `token` im [`Tier::Shaped`](minds_redact::Tier):
/// Der Wert muss zusätzlich credential-typisch *aussehen*, weil `token` in
/// Prosa vorkommt („Token-Limit: 4096"). Diese Begründung trägt hier nicht —
/// in einem `name=wert`-Segment einer Query gibt es keine Prosa. Ohne die
/// Anhebung bliebe `?private_token=supersecrettoken` stehen (rein alphabetisch,
/// besteht `has_credential_shape` nicht), ebenso ein kurzer selbstgesetzter
/// Wert. Betroffen wären Tokens ohne routbares Präfix, also self-hosted GitLab
/// vor 16.x.
///
/// Der Preis ist `?token_type=bearer`, das jetzt mit verschwindet. In einer
/// Diagnosedatei ist das der richtige Tausch.
///
/// Der Bau kann nicht scheitern: `secret_keys` wird regex-escaped, die
/// Denylists sind leer, alle übrigen Muster sind Konstanten. Deshalb `expect`
/// wie überall sonst im Workspace — ein stiller `None`-Zweig würde hier
/// Rohtext auf die Platte schreiben und wäre fail-open.
fn query_policy() -> &'static minds_redact::RedactionPipeline {
    static POLICY: std::sync::OnceLock<minds_redact::RedactionPipeline> =
        std::sync::OnceLock::new();
    POLICY.get_or_init(|| {
        minds_redact::RedactionConfig {
            secret_keys: vec!["token".to_string()],
            ..Default::default()
        }
        .pipeline()
        .expect("konstante Log-Policy muss kompilieren")
    })
}

/// Das Auffangnetz: **nur** die formbasierten Token-Regeln.
///
/// Bewusst nicht die volle Policy. Auf den ganzen Text losgelassen macht die
/// aus `Pushing to https://oauth2:glpat-…@gitlab.com/x.git` ein
/// `Pushing to https://[redacted:secret]` — Host und Pfad weg, gemessen. Für
/// den Envelope ist das richtig, für eine Diagnosedatei wäre es ein Rückschritt.
///
/// [`KnownTokenRedactor`](minds_redact::KnownTokenRedactor) erkennt dagegen an
/// der **Form** und braucht keinen Kontext. Er fasst deshalb genau das, was
/// zwischen den Strukturen durchfällt, und lässt Prosa, Host, Pfad und
/// `! [rejected]`-Marker unangetastet.
fn known_token_policy() -> &'static minds_redact::RedactionPipeline {
    static POLICY: std::sync::OnceLock<minds_redact::RedactionPipeline> =
        std::sync::OnceLock::new();
    POLICY.get_or_init(|| {
        minds_redact::RedactionPipeline::new().with(minds_redact::KnownTokenRedactor::new())
    })
}

/// Wendet eine Policy an und behandelt einen **nicht ersetzbaren Fund**
/// fail-closed.
///
/// `invalid_findings` heißt: Ein Detektor hat etwas gefunden, das die Pipeline
/// nicht ersetzen konnte (Span außerhalb des Textes oder nicht auf einer
/// Zeichengrenze). Der Text enthält den Fund dann **noch**. Für das Envelope
/// erhebt `redact_session` das zum harten Fehler; hier gibt es keinen Aufrufer,
/// der abbrechen könnte — also fällt der Inhalt weg statt das Geheimnis.
fn redact_or_drop(policy: &minds_redact::RedactionPipeline, text: &str, marker: &str) -> String {
    let out = policy.redact(text);
    if out.invalid_findings > 0 {
        marker.to_string()
    } else {
        out.text
    }
}

/// Formbasiertes Auffangnetz über den ganzen Text.
fn without_known_tokens(text: &str) -> String {
    redact_or_drop(known_token_policy(), text, "[redacted:message]")
}

/// Zugangsdaten in der **Query** einer URL: `?private_token=…`, `&job_token=…`.
///
/// # Warum die Redaction-Policy und keine eigene Parameterliste
///
/// Naheliegend wäre, die bekannten Namen (`private_token`, `access_token`,
/// `job_token`) hier aufzuzählen. Genau diese Doppelung war aber schon einmal
/// die Fehlerquelle: Zwei Stellen, die dasselbe Wissen halten, laufen
/// auseinander. `minds-redact` kennt die Namen bereits — nachgemessen fängt die
/// Default-Policy alle drei Formen, und sie fällt auch nicht auf
/// `?token_type=bearer` herein, was eine selbstgestrickte `*_token=`-Regel
/// getan hätte.
///
/// # Warum segmentweise und nicht auf dem ganzen Text
///
/// Die Policy ist für **Envelope-Text** gebaut und schwärzt dort im Zweifel
/// großzügig: Auf die ganze Meldung losgelassen macht sie aus
/// `Pushing to https://oauth2:glpat-…@gitlab.com/x.git` ein
/// `Pushing to https://[redacted:secret]` — Host und Pfad weg, gemessen. Für
/// den Envelope ist das richtig, für eine Diagnosedatei wäre es ein Rückschritt
/// hinter den Zustand, den diese Funktion herstellt.
///
/// Deshalb sieht die Policy hier immer nur **ein** `name=wert`-Paar. Was das
/// leistet und was nicht, genau benannt: Nachbarparameter hinter `&` bleiben,
/// ebenso Schema, Host und Pfad. Was dagegen **ohne Trenner** am Wert klebt,
/// liegt mit im Segment und fällt mit — der nackte Wert läuft bis zum nächsten
/// Leerraum. Deshalb stehen `,` und `)` mit in der Terminatorenliste: Ohne sie
/// nimmt `…?private_token=…,https://andere.url/x` die zweite URL mit, und eine
/// Meldung in Klammern verliert ihre schließende. Token sind base64url oder
/// hex und enthalten weder Komma noch Klammer, der Schnitt kostet also nichts.
fn without_query_credentials(text: &str) -> String {
    let policy = query_policy();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(q) = rest.find('?') {
        let (before, tail) = rest.split_at(q + '?'.len_utf8());
        out.push_str(before);

        // Die Query endet dort, wo die URL endet. Wie beim Autoritätsteil, aber
        // ohne `/` (das steht in Query-Werten) und zusätzlich mit `#`: Was
        // hinter dem Fragment-Zeichen steht, ist keine Query mehr — es fängt
        // stattdessen [`without_known_tokens`]. Backtick, Komma und Klammer
        // begrenzen die URL in Fließtext und Markdown-Meldungen.
        //
        // Ein `?` in Prosa („Was nun?") läuft hier ebenfalls hinein, schadet
        // aber nicht: Ohne `=` bleibt jedes Segment unangetastet.
        let end = tail
            .find(|c: char| {
                matches!(c, '#' | '\'' | '"' | '<' | '>' | '`' | ',' | ')') || c.is_whitespace()
            })
            .unwrap_or(tail.len());
        let (query, after) = tail.split_at(end);

        for (i, segment) in query.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            if segment.contains('=') {
                out.push_str(&redact_or_drop(policy, segment, "[redacted:parameter]"));
            } else {
                // Ohne `=` gibt es keinen Schlüsselnamen, an dem sich etwas
                // festmachen ließe. Ein Token, das hier stünde, fängt das
                // formbasierte Auffangnetz.
                out.push_str(segment);
            }
        }

        rest = after;
    }

    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Was `git push` in seine Fehlermeldung schreibt, wenn ein Token in der
    /// Remote-URL steht — und was davon in `hook.log` stehen darf.
    #[test]
    fn credentials_in_a_url_never_reach_the_log() {
        const CASES: &[(&str, &str)] = &[
            (
                "fatal: could not read Password for \
                 'https://glpat-AAAAAAAAAAAAAAAAAAAA@gitlab.com': terminal prompts disabled",
                "glpat-AAAAAAAAAAAAAAAAAAAA",
            ),
            (
                "fatal: unable to access \
                 'https://oauth2:glpat-BBBBBBBBBBBBBBBBBBBB@gitlab.example.com/team/repo.git/': 403",
                "glpat-BBBBBBBBBBBBBBBBBBBB",
            ),
            (
                "remote: fatal: unable to access \
                 'https://gitlab-ci-token:CCCCCCCCCCCCCCCCCCCC@gitlab.com/x/y.git/'",
                "CCCCCCCCCCCCCCCCCCCC",
            ),
            // Ein anderes Schema, damit kein `https://`-Sonderfall als Lösung
            // durchgeht.
            ("ssh://deploy:hunter2@git.internal:2222/x.git", "hunter2"),
        ];

        for (message, secret) in CASES {
            let cleaned = without_url_credentials(message);
            assert!(!cleaned.contains(secret), "{cleaned}");
            // Der Host muss bleiben — ohne ihn ist die Diagnose wertlos, und
            // dann löscht jemand die Datei statt sie zu lesen.
            assert!(cleaned.contains("…@"), "{cleaned}");
        }
    }

    #[test]
    fn a_message_without_credentials_stays_word_for_word() {
        // Die Gegenprobe: Der Zweck der Datei ist die Diagnose, nicht die
        // Redaction. Was kein Geheimnis ist, darf nicht verstümmelt werden.
        for message in [
            "fatal: Authentication failed for 'https://gitlab.com/team/repo.git/'",
            "fatal: '/gibt/es/nicht.git' does not appear to be a git repository",
            "kein Doppelpunkt-Schrägstrich hier, aber ein @ mittendrin",
            "https://gitlab.com/team/repo.git",
            // Query-Parameter, die keine Zugangsdaten sind.
            "GET https://gitlab.com/api/v4/projects?per_page=100&page=2 -> 200",
            "https://gitlab.com/api/v4/x?recursive=true&sha=356a192b7913b04c54574d18c28d46e6395428ab",
            "remote: HTTP Basic: Access denied. The provided password or token is incorrect.",
            "Was nun? Der Push wurde abgelehnt.",
            // Die Marker, an denen `sync::is_rejected` den Kontrollfluss
            // festmacht — verschwinden sie, entscheidet der Sync falsch.
            "! [rejected] refs/minds/reviews -> refs/minds/reviews (non-fast-forward)",
            "To https://gitlab.com/team/repo.git",
        ] {
            assert_eq!(without_url_credentials(message), message);
        }
    }

    #[test]
    fn token_type_is_over_redacted_on_purpose() {
        // Der Preis dafür, `token` in den Strict-Tier zu heben: Auch ein
        // harmloses `token_type=bearer` verschwindet. Ohne die Anhebung bliebe
        // dafür `?private_token=supersecrettoken` stehen — in einer
        // Diagnosedatei ist das der schlechtere Tausch.
        let cleaned = without_url_credentials("https://gitlab.com/x?token_type=bearer");
        assert!(!cleaned.contains("bearer"), "{cleaned}");
        assert!(cleaned.contains("token_type="), "{cleaned}");
        assert!(cleaned.contains("gitlab.com/x"), "{cleaned}");
    }

    #[test]
    fn a_token_in_the_query_is_cut_out() {
        // Die Form, die bei GitLab dokumentiert ist und kein `@` hat — der
        // Autoritäts-Schnitt allein greift hier nicht.
        // Die Werte sind bewusst **präfixlos**: Ein `glpat-…` finge der
        // formbasierte Detektor ohnehin, unabhängig vom Schlüsselnamen — der
        // Test bewiese dann nicht den Weg, um den es in #73 geht.
        const CASES: &[(&str, &str)] = &[
            (
                "fatal: unable to access \
                 'https://gitlab.com/team/repo.git?private_token=aB3xK9mQ7zR2wL5nT8vY': 403",
                "aB3xK9mQ7zR2wL5nT8vY",
            ),
            (
                "GET https://gitlab.com/api/v4/projects?access_token=s3cr3tv4lue1234 -> 401",
                "s3cr3tv4lue1234",
            ),
            (
                "https://gitlab.com/api/v4/jobs?job_token=s3cr3tv4lue1234",
                "s3cr3tv4lue1234",
            ),
            // Nicht an erster Stelle der Query — der `&`-Fall.
            (
                "https://gitlab.com/api/v4/projects?per_page=100&private_token=aB3xK9mQ7zR2wL5nT8vY&page=2",
                "aB3xK9mQ7zR2wL5nT8vY",
            ),
            // Rein alphabetisch und kurz: beides scheitert an
            // `has_credential_shape`. Nur weil `token` hier im Strict-Tier
            // liegt, verschwinden sie trotzdem.
            (
                "https://gitlab.com/x?private_token=supersecrettoken",
                "supersecrettoken",
            ),
            ("https://gitlab.com/x?job_token=hunter2", "hunter2"),
        ];

        for (message, secret) in CASES {
            let cleaned = without_url_credentials(message);
            assert!(!cleaned.contains(secret), "Token blieb stehen:\n{cleaned}");
            // Der Host muss auch hier bleiben, sonst ist die Diagnose wertlos.
            assert!(
                cleaned.contains("gitlab.com"),
                "Host verschwunden:\n{cleaned}"
            );
        }
    }

    #[test]
    fn the_query_cut_keeps_its_neighbours() {
        // Die Policy sieht immer nur **ein** `name=wert`-Paar. Nachbar-
        // parameter und Pfad dürfen deshalb nicht mitverschwinden.
        let cleaned = without_url_credentials(
            "https://gitlab.com/api/v4/projects?per_page=100&private_token=glpat-AAAAAAAAAAAAAAAAAAAA&page=2",
        );
        assert!(cleaned.contains("per_page=100"), "{cleaned}");
        assert!(cleaned.contains("page=2"), "{cleaned}");
        assert!(cleaned.contains("/api/v4/projects"), "{cleaned}");
    }

    #[test]
    fn a_token_outside_both_structures_is_still_caught() {
        // Weder Autorität noch Query: Der Token steht im **Pfad**, im
        // **Fragment** (OAuth-Implicit-Flow) oder mitten in Prosa. Dafür gibt
        // es das formbasierte Auffangnetz.
        const TOKEN: &str = "glpat-ORS6ilI8ihN5KXSc7TvA1b2C3d4";
        for (message, kept) in [
            (
                "fatal: unable to access https://gitlab.com/api/v4/glpat-ORS6ilI8ihN5KXSc7TvA1b2C3d4/x: 403",
                "gitlab.com",
            ),
            (
                "https://gitlab.com/oauth/cb#access_token=glpat-ORS6ilI8ihN5KXSc7TvA1b2C3d4",
                "gitlab.com/oauth/cb",
            ),
            (
                "remote: der Token glpat-ORS6ilI8ihN5KXSc7TvA1b2C3d4 ist abgelaufen",
                "ist abgelaufen",
            ),
        ] {
            let cleaned = without_url_credentials(message);
            assert!(!cleaned.contains(TOKEN), "Token blieb stehen:\n{cleaned}");
            assert!(cleaned.contains(kept), "Kontext {kept:?} weg:\n{cleaned}");
        }
    }

    #[test]
    fn a_second_url_after_a_comma_survives() {
        // Ohne `,` in der Terminatorenliste frisst der nackte Wert die zweite
        // URL mitsamt Host — genau das, was diese Funktion verhindern soll.
        let cleaned = without_url_credentials(
            "URLs: https://g.com/x?private_token=aB3xK9mQ7zR2wL5nT8vY,https://g.com/y?ref=main",
        );
        assert!(!cleaned.contains("aB3xK9mQ7zR2wL5nT8vY"), "{cleaned}");
        assert!(cleaned.contains("https://g.com/y?ref=main"), "{cleaned}");
    }

    #[test]
    fn the_query_cut_survives_odd_shapes() {
        // Die billigen Ränder, die die Index-Arithmetik festnageln. Alle
        // verhalten sich heute korrekt — als Test geschrieben, damit die
        // nächste Änderung an den Terminatoren sie nicht still bricht.
        for message in [
            "endet mit einem Fragezeichen?",
            "https://gitlab.com/x?",
            "https://gitlab.com/x?&&a=1&&",
            "https://gitlab.com/pfad/mit/ü?ref=main",
            "kein Fragezeichen, keine URL",
        ] {
            let once = without_url_credentials(message);
            // Idempotenz: Ein zweiter Lauf darf nichts weiter verändern.
            assert_eq!(without_url_credentials(&once), once, "nicht idempotent");
        }
    }

    #[test]
    fn authority_and_query_are_both_handled_in_one_message() {
        // Beide Formen in einer Zeile — der Autoritäts-Schnitt darf die
        // Query-Behandlung nicht überspringen und umgekehrt.
        let cleaned = without_url_credentials(
            "fatal: 'https://oauth2:glpat-BBBBBBBBBBBBBBBBBBBB@gitlab.com/x.git?private_token=glpat-AAAAAAAAAAAAAAAAAAAA'",
        );
        assert!(!cleaned.contains("glpat-BBBBBBBBBBBBBBBBBBBB"), "{cleaned}");
        assert!(!cleaned.contains("glpat-AAAAAAAAAAAAAAAAAAAA"), "{cleaned}");
        assert!(cleaned.contains("gitlab.com"), "{cleaned}");
    }
}
