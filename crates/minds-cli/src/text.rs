//! Fremder Text, der auf ein Terminal oder in eine Datei darf.
//!
//! Fast nichts, was `minds` ausgibt, stammt von `minds`: Pfade kommen aus der
//! Arbeitskopie, Fehlermeldungen tragen Dateinamen und Parser-Ausschnitte,
//! Log-Zeilen tragen den Wortlaut fremder Fehler. Roh ausgegeben kann so ein
//! Text mehr als sich selbst — eine ANSI-Sequenz löscht Zeilen im Terminal des
//! Lesers, ein Bidi-Zeichen dreht die Leserichtung um, ein Zeilentrenner sieht
//! in einem zeilenweisen Log aus wie ein eigener Eintrag, und ein unsichtbares
//! Formatzeichen sieht nach gar nichts aus.
//!
//! Beide Senken sind fremd: `<git-dir>/minds/hook.log` liest ein Mensch im
//! Terminal, die Ausgabe von `minds fsck` landet obendrein im Job-Log der
//! GitLab-Pipeline — dort, wo ein Reviewer nur überfliegt.
//!
//! # Warum nicht `is_control()`
//!
//! Naheliegend wäre `char::is_control`. Das ist aber **nur** die Kategorie `Cc`,
//! und die reicht nicht:
//!
//! - `U+2028` (LINE SEPARATOR, `Zl`) und `U+2029` (PARAGRAPH SEPARATOR, `Zp`)
//!   sind keine `Cc`. Rusts `str::lines` bricht daran nicht — Browser,
//!   `str.splitlines()` in Python und diverse Log-Viewer schon. Ein Log, dessen
//!   Zeilenzahl davon abhängt, wer es liest, ist als Beweismittel wertlos, und
//!   im GitLab-Job-Log ließe sich damit eine Zeile fälschen.
//! - Die Bidi-Marken (`RLO` & Co.) und die Unicode-Tags `U+E0020`–`U+E007F` sind
//!   `Cf`. Letztere sind vollständig unsichtbar — der bekannte Träger für
//!   versteckten Text.
//!
//! Statt diese Liste von Hand zu pflegen (und die eine zu vergessen, die zählt),
//! wird `escape_debug` selbst gefragt: Was es escapt, ist zu entschärfen. Es
//! deckt `Cc`, `Cf`, `Zl`, `Zp` und `Zs` ab. Zwei Korrekturen daran, beide in
//! [`is_escapeworthy`] und [`escape`] begründet:
//!
//! - **Zurück**: Die ASCII-Anführungszeichen escapt `escape_debug` mit; in einem
//!   Pfad wären sie nur unschön, gefährlich sind sie nicht.
//! - **Dazu**: Die typografischen Anführungszeichen, in die `fsck` und `enable`
//!   ihre Pfade klammern. Sie gehören zur Struktur der Ausgabe, nicht zum
//!   Inhalt — ein Pfad, der sie enthält, könnte die Klammer sonst früh schließen
//!   und danach beliebigen Text anhängen.

/// Entschärft alles, was mehr kann, als ein Zeichen zu sein — für eine Zeile,
/// die auch dann noch eine ist, wenn der Text von woanders kam.
///
/// Der **Backslash** wird mitentschärft. Ohne das wäre die Abbildung nicht
/// eindeutig: Ein Pfad `C:\neu` und ein entschärfter Zeilenumbruch ergäben
/// beide `…\n…`, und beim Lesen ließe sich nicht mehr entscheiden, was dastand.
/// Für die Anzeige eines *Pfades* ist diese Eindeutigkeit unnötig und die
/// verdoppelten Trenner nur störend — dafür gibt es [`sanitize_path`].
pub(crate) fn sanitize(text: &str) -> String {
    escape(text, true)
}

/// Wie [`sanitize`], nur ohne den Backslash.
///
/// Für Pfade, die in einer Meldung stehen: Dort ist der Backslash unter Windows
/// der Trenner, und `C:\\repo\\.git` zu lesen wäre eine Zumutung ohne Gewinn.
/// Die gefährlichen Zeichen — ANSI, Bidi, Zeilentrenner, unsichtbare
/// Formatzeichen — werden genauso entschärft.
pub(crate) fn sanitize_path(text: &str) -> String {
    escape(text, false)
}

/// Die Zeichen, in die `fsck` und `enable` ihre Pfade klammern.
///
/// Sie gehören zur *Struktur* der Ausgabe, nicht zum Inhalt: Ein
/// Verzeichnisname, der ein `“` enthält, schlösse die Klammer früh und könnte
/// danach beliebigen Text anhängen — in einem Job-Log, das ein Reviewer nur
/// überfliegt. Anders als die ASCII-Anführungszeichen, die deshalb roh
/// durchgehen dürfen.
const DELIMITERS: [char; 3] = ['„', '“', '”'];

fn escape(text: &str, escape_backslash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\\' {
            if escape_backslash {
                out.push_str("\\\\");
            } else {
                out.push(c);
            }
        } else if DELIMITERS.contains(&c) {
            out.extend(c.escape_unicode());
        } else if is_escapeworthy(c) {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// Ob dieses Zeichen mehr kann, als ein Zeichen zu sein.
///
/// Gefragt wird [`str::escape_debug`], und zwar mit einem **Sentinel davor**.
/// Der Grund ist eine Feinheit, die man einmal sehen muss: `char::escape_debug`
/// escapt zusätzlich jedes `Grapheme_Extend`-Zeichen — also die kombinierenden
/// Akzente. `str::escape_debug` tut das nur am *Anfang* der Zeichenkette, weil
/// dort keine Basis steht, an die sie sich hängen könnten.
///
/// Ohne den Sentinel würde aus einem Pfad `süß`, wie ihn APFS in NFD liefert,
/// die Zeichenfolge `su\u{308}ß` — und ein Nutzer, dem `minds` seine Pfade
/// zerlegt, schaltet den Entschärfer ab. Mit dem Sentinel steht das Zeichen in
/// Folgeposition, kombinierende Akzente gehen durch, und `Cc`, `Cf`, `Zl`, `Zp`
/// und `Zs` werden weiterhin alle escapt.
fn is_escapeworthy(c: char) -> bool {
    if INVISIBLE_CARRIERS.iter().any(|range| range.contains(&c)) {
        return true;
    }
    if matches!(c, '\'' | '"') {
        return false;
    }

    // Kein `String`: Ein Sentinel plus höchstens vier Bytes passen auf den
    // Stapel, und diese Funktion läuft je Zeichen jeder Ausgabe.
    let mut buf = [0u8; 5];
    buf[0] = b'x';
    let len = 1 + c.encode_utf8(&mut buf[1..]).len();
    // Unerreichbar — `b'x'` und `encode_utf8` liefern beide gültiges UTF-8. Der
    // Rückfall ist trotzdem der sichere: `"x"` hat *ein* Zeichen, also `!= 2`,
    // also wird escapt.
    let probe = std::str::from_utf8(&buf[..len]).unwrap_or("x");

    // Zwei Zeichen heißt: Der Sentinel und `c` selbst, also unverändert. Alles
    // Längere ist eine Escape-Sequenz.
    probe.escape_debug().count() != 2
}

/// Unsichtbare Zeichen, die der Sentinel-Trick *nicht* erwischt.
///
/// Der Preis des Sentinels: In Folgeposition escapt `str::escape_debug` kein
/// `Grapheme_Extend`-Zeichen — und ein Teil der `Cf`-Zeichen ist genau das.
/// Diese hier rendern nichts und können deshalb Text verstecken, den ein
/// Reviewer im Job-Log nicht sieht:
///
/// - `U+17B4`/`U+17B5` — Khmer-Vokale, die nichts darstellen.
/// - `U+180B`–`U+180F` — mongolische Variantenselektoren.
/// - `U+E0100`–`U+E01EF` — der Variantenselektor-Nachtrag, 240 Codepoints und
///   der heute gebräuchlichere Bruder der Unicode-Tags aus dem Modulkopf.
///
/// **Bewusst nicht dabei: `U+FE00`–`U+FE0F`.** Die Variantenselektoren 1–16
/// stehen in echten Dateinamen (`❤\u{FE0F}`), und ein Entschärfer, der die
/// zerlegt, wird abgeschaltet. Sie hängen an einem sichtbaren Basiszeichen und
/// tragen keinen eigenen Inhalt.
const INVISIBLE_CARRIERS: [std::ops::RangeInclusive<char>; 3] = [
    '\u{17B4}'..='\u{17B5}',
    '\u{180B}'..='\u{180F}',
    '\u{E0100}'..='\u{E01EF}',
];

/// Nimmt Zugangsdaten aus URLs, lässt aber den Rest der Meldung stehen.
///
/// Der Anlass: `git push` schreibt die Remote-URL in seine Fehlermeldung, und
/// steht darin ein Token (`https://glpat-…@gitlab.com/…`), landete es über den
/// Umweg der Fehlermeldung in `hook.log` — einer Datei, auf die `minds fsck`
/// aktiv verweist und die in einem Bug-Report mitgeschickt wird. Redigiert wird
/// deshalb **an der Quelle**, bevor der Text zu einer Meldung wird.
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

    /// Was unverändert durchgehen muss. Ein Entschärfer, der Umlaute
    /// verstümmelt, wird abgeschaltet — und dann schützt er gar nichts mehr.
    const MUST_SURVIVE: &[(&str, &str)] = &[
        ("umlaute", "crates/minds-cli/süß"),
        // Wie APFS ihn liefert: dasselbe Wort in NFD, mit kombinierendem
        // Umlaut. `char::escape_debug` allein zerlegte das zu `su\u{308}ß` —
        // siehe die Sentinel-Begründung in `is_escapeworthy`.
        ("umlaute_nfd", "crates/minds-cli/su\u{308}\u{df}"),
        ("devanagari", "प्रोजेक्ट/हिन्दी.rs"),
        ("emoji_mit_variante", "❤\u{FE0F}/herz.rs"),
        ("cjk", "プロジェクト/日本語.rs"),
        ("emoji", "🌱/wachstum.rs"),
        ("leerzeichen", "mein ordner/hooks"),
        ("anfuehrungszeichen", "der \"hook\" und 'das' andere"),
        ("tilde_und_punkt", "~/.config/minds/redact.json"),
    ];

    /// Was keine Zeile öffnen und nichts überschreiben darf.
    const MUST_ESCAPE: &[(&str, char)] = &[
        ("zeilenumbruch", '\n'),
        ("wagenruecklauf", '\r'),
        ("tabulator", '\t'),
        ("escape_sequenz", '\u{1b}'),
        ("nullbyte", '\0'),
        // Zl/Zp — nicht `is_control`, aber in Browsern und in Pythons
        // `splitlines()` ein Zeilenumbruch.
        ("line_separator", '\u{2028}'),
        ("paragraph_separator", '\u{2029}'),
        // Cf — unsichtbar.
        ("rechts_nach_links", '\u{202E}'),
        ("links_nach_rechts", '\u{200E}'),
        ("zero_width_space", '\u{200B}'),
        ("byte_order_mark", '\u{FEFF}'),
        ("weiches_trennzeichen", '\u{00AD}'),
        ("unicode_tag", '\u{E0041}'),
        ("unicode_tag_ende", '\u{E007F}'),
        // Cc, aber der klassisch vergessene Zeilenumbruch.
        ("next_line", '\u{0085}'),
        // Bidi-Isolate — die handgepflegte Liste von früher hatte sie, die
        // Begründung über `escape_debug` nennt sie nicht mehr einzeln.
        ("isolate_anfang", '\u{2066}'),
        ("isolate_ende", '\u{2069}'),
        // Zs: öffnet keine Zeile, belegt aber den Beleg, dass die Erkennung
        // über `Cc`/`Cf`/`Zl`/`Zp` hinaus trägt.
        ("geschuetztes_leerzeichen", '\u{00A0}'),
        ("ideographisches_leerzeichen", '\u{3000}'),
        // Die Klammer der Ausgabe selbst — sonst ließe sich „…“ fälschen.
        ("klammer_auf", '„'),
        ("klammer_zu", '“'),
    ];

    #[test]
    fn harmless_text_survives_unchanged() {
        for (name, text) in MUST_SURVIVE {
            assert_eq!(&sanitize(text), text, "{name}");
            assert_eq!(&sanitize_path(text), text, "{name}");
        }
    }

    #[test]
    fn everything_that_can_do_more_than_be_a_character_is_escaped() {
        for (name, c) in MUST_ESCAPE {
            let shown = sanitize(&format!("davor{c}danach"));
            assert!(!shown.contains(*c), "{name}: {shown:?}");
            assert!(
                shown.starts_with("davor") && shown.ends_with("danach"),
                "{name}"
            );
            // Auch auf dem Pfad-Weg, der nur den Backslash auslässt.
            assert!(
                !sanitize_path(&format!("davor{c}danach")).contains(*c),
                "{name} (Pfad)"
            );
        }
    }

    #[test]
    fn an_escape_sequence_loses_its_effect() {
        // „\u{1b}[2K\u{1b}[A" löscht im Terminal die Zeile und springt hoch —
        // damit ließe sich eine vorangegangene Ausgabezeile überschreiben.
        let shown = sanitize("\u{1b}[2K\u{1b}[Aböse");
        assert!(!shown.contains('\u{1b}'), "{shown}");
        assert!(shown.contains("böse"), "{shown}");
    }

    #[test]
    fn a_line_cannot_be_forged() {
        // Der Grund, aus dem `hooklog` diese Funktion braucht: Weder ein echter
        // Umbruch noch ein Zeilentrenner darf einen eigenen Eintrag vortäuschen.
        for opener in ['\n', '\u{2028}', '\u{2029}'] {
            let shown = sanitize(&format!(
                "harmlos{opener}2026-01-01T00:00:00Z checkpoint: alles gut"
            ));
            assert_eq!(shown.lines().count(), 1, "{shown:?}");
            assert!(!shown.contains(opener), "{shown:?}");
        }
    }

    #[test]
    fn an_escaped_newline_is_not_confusable_with_a_literal_backslash_n() {
        // Sonst ließe sich ein Umbruch vortäuschen, indem man `\n` einfach
        // hinschreibt — und beim Lesen wäre nicht zu entscheiden, was es war.
        assert_ne!(sanitize("a\nb"), sanitize("a\\nb"));
        assert_eq!(sanitize("a\nb"), "a\\nb");
        assert_eq!(sanitize("a\\nb"), "a\\\\nb");
    }

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

    #[test]
    fn a_path_keeps_its_separators() {
        // Die eine Stelle, an der sich die beiden Wege unterscheiden.
        assert_eq!(
            sanitize_path("C:\\repo\\.git\\hooks"),
            "C:\\repo\\.git\\hooks"
        );
        assert_eq!(sanitize("C:\\repo"), "C:\\\\repo");
    }
}
