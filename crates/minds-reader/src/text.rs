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
//! deckt `Cc`, `Cf`, `Zl`, `Zp` und `Zs` ab. Drei Korrekturen daran, in
//! [`is_escapeworthy`], [`escape`] und [`INVISIBLE_CARRIERS`] begründet:
//!
//! - **Zurück**: Die ASCII-Anführungszeichen escapt `escape_debug` mit; in einem
//!   Pfad wären sie nur unschön, gefährlich sind sie nicht.
//! - **Dazu**: Die typografischen Anführungszeichen, in die `fsck` und `enable`
//!   ihre Pfade klammern. Sie gehören zur Struktur der Ausgabe, nicht zum
//!   Inhalt — ein Pfad, der sie enthält, könnte die Klammer sonst früh schließen
//!   und danach beliebigen Text anhängen.
//! - **Dazu**: Die druckbaren Zeichen ohne Glyph, an denen der Kategorienblick
//!   vorbeisieht — Hangul-Füller, Braille-Blank, kombinierende Träger. Das
//!   Kriterium, aus dem diese Liste folgt, steht an [`INVISIBLE_CARRIERS`].

/// Entschärft alles, was mehr kann, als ein Zeichen zu sein — für eine Zeile,
/// die auch dann noch eine ist, wenn der Text von woanders kam.
///
/// Der **Backslash** wird mitentschärft. Ohne das wäre die Abbildung nicht
/// eindeutig: Ein Pfad `C:\neu` und ein entschärfter Zeilenumbruch ergäben
/// beide `…\n…`, und beim Lesen ließe sich nicht mehr entscheiden, was dastand.
/// Für die Anzeige eines *Pfades* ist diese Eindeutigkeit unnötig und die
/// verdoppelten Trenner nur störend — dafür gibt es [`sanitize_path`].
pub fn sanitize(text: &str) -> String {
    escape(text, true)
}

/// Wie [`sanitize`], nur ohne den Backslash.
///
/// Für Pfade, die in einer Meldung stehen: Dort ist der Backslash unter Windows
/// der Trenner, und `C:\\repo\\.git` zu lesen wäre eine Zumutung ohne Gewinn.
/// Die gefährlichen Zeichen — ANSI, Bidi, Zeilentrenner, unsichtbare
/// Formatzeichen — werden genauso entschärft.
pub fn sanitize_path(text: &str) -> String {
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
        } else if INVISIBLE_CARRIERS.iter().any(|range| range.contains(&c)) {
            // Nicht `escape_debug`: Das ließe die *druckbaren* Träger — die
            // Hangul-Füller (`Lo`), das Braille-Blank (`So`) — wörtlich
            // stehen. Für die `Mn`-Träger ist `escape_unicode` deckungsgleich.
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
/// Der Preis des Sentinels: In Folgeposition escapt `str::escape_debug` nur,
/// was es für nicht druckbar hält. Druckbare Zeichen ohne Glyph — `Mn`-Träger,
/// die Hangul-Füller (`Lo`), das leere Braille-Muster (`So`) — fallen durch.
/// Damit der nächste Fall keine Einzelfallentscheidung wird, folgt die Liste
/// aus einem Kriterium statt aus einer Aufzählung:
///
/// **Unsichtbar ist, was Unicode als `Default_Ignorable_Code_Point` führt
/// („render als nichts", `DerivedCoreProperties.txt`, Stand Unicode 16.0) oder
/// dessen definierter Glyph leer ist („render als Leerzeichen": `Zs` — außer
/// dem Leerzeichen `U+0020` selbst — und `U+2800`). Einzige Ausnahme:
/// `U+FE00`–`U+FE0F`, wegen Häufigkeit.**
///
/// `Cc`/`Cf`/`Zl`/`Zp`/`Zs` und Unzugewiesenes escapt `escape_debug` selbst;
/// hier steht der Rest — der Test `every_default_ignorable_is_escaped` misst
/// die Behauptung gegen die komplette Property nach:
///
/// - `U+034F` — Combining Grapheme Joiner, der Klassiker zum Filter-Umgehen:
///   `Mn`, mitten im String unsichtbar.
/// - `U+115F`/`U+1160` — die Hangul-Conjoining-Füller. `Lo` ist ein Artefakt
///   des Kodierungsmodells; die definierte Funktion ist „Platzhalter, der als
///   nichts gerendert wird" — zusammen mit `U+3164`/`U+FFA0` die einzigen
///   `Lo`-Einträge der ganzen Property.
/// - `U+17B4`/`U+17B5` — Khmer-Vokale, die nichts darstellen.
/// - `U+180B`–`U+180F` — mongolische Variantenselektoren und Vokaltrenner.
/// - `U+2800` — Braille Pattern Blank. Nicht Default-Ignorable, aber per
///   Definition leer (nicht per Font-Zufall) — und neben `U+3164` das
///   gängigste „unsichtbare Zeichen" für leere Namen, gerade weil `So` von
///   fast keinem Filter angefasst wird. Echte Braille-Zellen
///   (`U+2801`–`U+28FF`) bleiben unberührt.
/// - `U+3164`/`U+FFA0` — Hangul Filler und sein Halbbreit-Zwilling, beide
///   NFKC-äquivalent zu `U+1160`: drei Schreibweisen desselben Platzhalters.
///   Modernes Koreanisch besteht aus vorkomponierten Silben (`U+AC00`–`D7A3`)
///   und bleibt unangetastet; der legitime Filler-Einsatz sind
///   Jamo-Sequenzen für unvollständige Silben — in einem Pfad praktisch nie,
///   und `\u{1160}` im Log ist verlustfrei und informativer als ein
///   unsichtbarer Glyph.
/// - `U+1D159` — Musical Symbol Null Notehead: außerhalb der BMP, aber
///   derselbe Fall wie `U+2800` — ein `So`-Zeichen, dessen Glyph per
///   Definition leer ist.
/// - `U+E0100`–`U+E01EF` — der Variantenselektor-Nachtrag, 240 Codepoints und
///   der heute gebräuchlichere Bruder der Unicode-Tags aus dem Modulkopf.
///
/// **Bewusst nicht dabei: `U+FE00`–`U+FE0F`.** Die Variantenselektoren 1–16
/// stehen in echten Dateinamen (`❤\u{FE0F}`) — Dauerrauschen, und ein
/// Entschärfer, der die zerlegt, wird abgeschaltet. Sie hängen an einem
/// sichtbaren Basiszeichen und tragen keinen eigenen Inhalt. Die Ausnahme
/// überträgt sich nicht auf die Füller: Dort ist die Fehlalarmrate effektiv
/// null, und die Kosten sind asymmetrisch — Fehlalarm heißt hässliche, aber
/// korrekte Zeile; Durchlasser heißt Eintrag, den niemand lesen oder abtippen
/// kann. Kippen würde das Urteil nur eine gemessene Fehlalarmrate wie bei
/// `U+FE0F`.
///
/// Nicht der Weg: NFKC vor dem Entschärfen. Der Sanitizer darf die
/// Darstellung ändern, nicht die Identität des Pfads.
const INVISIBLE_CARRIERS: [std::ops::RangeInclusive<char>; 9] = [
    '\u{034F}'..='\u{034F}',
    '\u{115F}'..='\u{1160}',
    '\u{17B4}'..='\u{17B5}',
    '\u{180B}'..='\u{180F}',
    '\u{2800}'..='\u{2800}',
    '\u{3164}'..='\u{3164}',
    '\u{FFA0}'..='\u{FFA0}',
    '\u{1D159}'..='\u{1D159}',
    '\u{E0100}'..='\u{E01EF}',
];

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
        // Vorkomponierte Silben — modernes Koreanisch. Die Füller-Entschärfung
        // (#72) darf hier nicht hineinkippen.
        ("koreanisch", "프로젝트/한국어.rs"),
        // Kompatibilitäts-Jamo ohne Füller (U+3131, U+314F) und Conjoining
        // Jamo (U+1100, U+1161): pinnt die Füller-Regel auf vier Codepoints
        // statt auf Blöcke.
        ("jamo_ohne_fueller", "\u{3131}\u{314F}/\u{1100}\u{1161}.rs"),
        // Braille mit Punkten — nur das leere Muster U+2800 ist ein Träger.
        ("braille_mit_punkten", "\u{2801}\u{28FF}/braille.rs"),
        // U+FFFC rendert ein sichtbares Ersatzsymbol — kein Durchrutscher,
        // sondern Absicht.
        ("objekt_ersatzzeichen", "a\u{FFFC}b.rs"),
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
        // Lo, aber Default-Ignorable: die Hangul-Füller — Platzhalter, die als
        // nichts gerendert werden (#72).
        ("hangul_choseong_filler", '\u{115F}'),
        ("hangul_jungseong_filler", '\u{1160}'),
        ("hangul_filler", '\u{3164}'),
        ("halfwidth_hangul_filler", '\u{FFA0}'),
        // So, aber per Definition leer — neben U+3164 das gängigste
        // „unsichtbare Zeichen" für leere Namen (#72).
        ("braille_pattern_blank", '\u{2800}'),
        // Mn, Default-Ignorable, in Folgeposition unsichtbar durch
        // `str::escape_debug` — der Klassiker zum Filter-Umgehen (#72).
        ("combining_grapheme_joiner", '\u{034F}'),
        // So außerhalb der BMP, Glyph per Definition leer — derselbe Fall
        // wie U+2800.
        ("musical_null_notehead", '\u{1D159}'),
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
    fn a_filler_falls_per_codepoint_not_per_sequence() {
        // `\u{115F}\u{1161}` — eine Jamo-Sequenz mit fehlendem Anlaut: Der
        // Füller wird entschärft, der Vokal bleibt. Die Regel gilt pro
        // Codepoint, nicht pro Sequenz.
        let shown = sanitize("\u{115F}\u{1161}");
        assert!(!shown.contains('\u{115F}'), "{shown:?}");
        assert!(shown.contains('\u{1161}'), "{shown:?}");
    }

    #[test]
    fn a_name_made_only_of_invisibles_does_not_arrive_empty() {
        // Der klassische Angriff mit „leerem" Namen — nicht nur in
        // Mittelposition, sondern als kompletter String.
        for name in ["\u{3164}", "\u{2800}\u{2800}\u{2800}", "\u{115F}\u{1160}"] {
            let shown = sanitize(name);
            assert!(shown.starts_with("\\u{"), "{shown:?}");
        }
        // Der bewusste Durchlasser als gemessener Grenzfall: Ein Nur-VS-String
        // bleibt wörtlich — die FE00–FE0F-Ausnahme aus `INVISIBLE_CARRIERS`.
        assert_eq!(sanitize("\u{FE0F}"), "\u{FE0F}");
    }

    /// `Default_Ignorable_Code_Point`, Unicode 16.0 — die Bereiche aus
    /// `DerivedCoreProperties.txt`, zusammenhängende verschmolzen. Die Liste
    /// ist kurz und ändert sich selten; bei einem Unicode-Sprung hier
    /// nachziehen.
    const DEFAULT_IGNORABLE: &[std::ops::RangeInclusive<char>] = &[
        '\u{00AD}'..='\u{00AD}',   // SOFT HYPHEN
        '\u{034F}'..='\u{034F}',   // COMBINING GRAPHEME JOINER
        '\u{061C}'..='\u{061C}',   // ARABIC LETTER MARK
        '\u{115F}'..='\u{1160}',   // HANGUL CHOSEONG/JUNGSEONG FILLER
        '\u{17B4}'..='\u{17B5}',   // KHMER VOWEL INHERENT AQ/AA
        '\u{180B}'..='\u{180F}',   // MONGOLIAN FVS + VOWEL SEPARATOR
        '\u{200B}'..='\u{200F}',   // ZWSP..RLM
        '\u{202A}'..='\u{202E}',   // LRE..RLO
        '\u{2060}'..='\u{206F}',   // WORD JOINER..NOMINAL DIGIT SHAPES
        '\u{3164}'..='\u{3164}',   // HANGUL FILLER
        '\u{FE00}'..='\u{FE0F}',   // VARIATION SELECTOR-1..16
        '\u{FEFF}'..='\u{FEFF}',   // ZERO WIDTH NO-BREAK SPACE
        '\u{FFA0}'..='\u{FFA0}',   // HALFWIDTH HANGUL FILLER
        '\u{FFF0}'..='\u{FFF8}',   // <reserved>
        '\u{1BCA0}'..='\u{1BCA3}', // SHORTHAND FORMAT CONTROLS
        '\u{1D173}'..='\u{1D17A}', // MUSICAL SYMBOL BEGIN BEAM..END PHRASE
        '\u{E0000}'..='\u{E0FFF}', // LANGUAGE TAG, TAGS, VS-17..256, <reserved>
    ];

    #[test]
    fn every_default_ignorable_is_escaped() {
        // Das Kriterium aus `INVISIBLE_CARRIERS`, nachgemessen gegen die
        // komplette Property: „render als nichts" heißt entschärfen — egal ob
        // `Cf`, `Mn`, `Lo` oder unzugewiesen. Einzige Ausnahme die
        // Variantenselektoren 1–16, deren Begründung dort steht.
        for range in DEFAULT_IGNORABLE {
            for c in range.clone() {
                if ('\u{FE00}'..='\u{FE0F}').contains(&c) {
                    // Die Ausnahme positiv gemessen statt nur ausgespart:
                    // Landet die Range je versehentlich in
                    // `INVISIBLE_CARRIERS`, schlägt es hier fehl.
                    let text = format!("davor{c}danach");
                    assert_eq!(sanitize(&text), text, "U+{:04X}", c as u32);
                    continue;
                }
                let shown = sanitize(&format!("davor{c}danach"));
                assert!(!shown.contains(c), "U+{:04X} geht roh durch", c as u32);
            }
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
