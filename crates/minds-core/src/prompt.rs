//! Die Hülle um einen Prompt — was der Agent-Harness um die Worte des
//! Menschen legt, bevor der Hook sie sieht.
//!
//! Claude Code reicht einen eingefügten Text nicht roh weiter, sondern als
//! `<pasted_content id="…">…</pasted_content>`. Im `UserPromptSubmit`-Payload
//! steht dann die Hülle **mitsamt** Inhalt, und ohne Gegenmaßnahme landete
//! sie in `intent.request`: Die Überschrift jeder Übersicht begänne mit
//! einem Tag statt mit der Absicht.
//!
//! Die Hülle ist kein Teil der Absicht — sie sagt nur, *wie* der Text in die
//! Eingabe kam. Deshalb wird sie abgestreift, der Inhalt bleibt. Das ist
//! Normalisierung wie das Entschärfen von Steuerzeichen, keine Deutung: Was
//! der Mensch schrieb, steht hinterher unverändert da.
//!
//! Zwei Senken teilen sich diese eine Funktion: der Normalizer beim Capture
//! (neue Sessions tragen die Hülle gar nicht erst) und die Anzeige (Sessions,
//! die vor dieser Regel erfasst wurden, sind unveränderliche Records und
//! behalten ihre Bytes — der Reader zeigt sie trotzdem ohne Hülle).

use std::borrow::Cow;

const OPEN: &str = "<pasted_content";
const CLOSE: &str = "</pasted_content>";

/// Entfernt jede `<pasted_content …>`-Hülle aus `prompt` und behält den
/// Inhalt. Öffnende Tags werden samt ihren Attributen entfernt, schließende
/// ganz; führender und folgender Leerraum des Ergebnisses fällt weg,
/// Zeilenumbrüche im Inhalt bleiben.
///
/// Ein Prompt ohne Hülle kommt unverändert und ohne Kopie zurück. Ein
/// öffnendes Tag ohne `>` ist kein Tag und bleibt stehen.
pub fn without_pasted_wrapper(prompt: &str) -> Cow<'_, str> {
    if !prompt.contains(OPEN) && !prompt.contains(CLOSE) {
        return Cow::Borrowed(prompt);
    }
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while !rest.is_empty() {
        let open = rest.find(OPEN);
        let close = rest.find(CLOSE);
        let (at, len) = match (open, close) {
            (Some(o), Some(c)) if o <= c => (o, open_tag_len(&rest[o..])),
            (Some(o), None) => (o, open_tag_len(&rest[o..])),
            (_, Some(c)) => (c, Some(CLOSE.len())),
            (None, None) => break,
        };
        match len {
            Some(len) => {
                out.push_str(&rest[..at]);
                rest = &rest[at + len..];
            }
            None => {
                // Ein `<pasted_content` ohne schließendes `>`: kein Tag.
                out.push_str(&rest[..at + OPEN.len()]);
                rest = &rest[at + OPEN.len()..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out.trim().to_string())
}

/// Die Länge eines öffnenden Tags ab seinem `<` — bis einschließlich `>`;
/// `None`, wenn keines folgt. Ein `<pasted_contents>` o. Ä. zählt nicht:
/// Nach dem Namen muss Leerraum oder das `>` kommen.
fn open_tag_len(s: &str) -> Option<usize> {
    let after = s[OPEN.len()..].chars().next()?;
    if after != '>' && !after.is_whitespace() {
        return None;
    }
    s.find('>').map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::without_pasted_wrapper;

    #[test]
    fn a_wrapped_prompt_keeps_only_its_content() {
        let wrapped = "<pasted_content id=\"fb19\">\nFüge die Konstante hinzu.\nNicht committen.\n</pasted_content>";
        assert_eq!(
            without_pasted_wrapper(wrapped),
            "Füge die Konstante hinzu.\nNicht committen."
        );
    }

    #[test]
    fn a_plain_prompt_is_borrowed_unchanged() {
        let plain = "Fix den Retry-Test\n  mit Einrückung ";
        let out = without_pasted_wrapper(plain);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out, plain);
    }

    #[test]
    fn text_around_and_between_several_pastes_survives() {
        let mixed = "Schau dir das an: <pasted_content id=\"1\">eins</pasted_content> und <pasted_content id=\"2\">zwei</pasted_content>.";
        assert_eq!(
            without_pasted_wrapper(mixed),
            "Schau dir das an: eins und zwei."
        );
    }

    #[test]
    fn a_tag_without_attributes_and_a_bare_closing_tag_are_removed() {
        assert_eq!(
            without_pasted_wrapper("<pasted_content>x</pasted_content>"),
            "x"
        );
        assert_eq!(without_pasted_wrapper("x</pasted_content>"), "x");
    }

    #[test]
    fn an_unterminated_or_lookalike_tag_stays() {
        assert_eq!(
            without_pasted_wrapper("<pasted_content id=\"3\" ohne Ende"),
            "<pasted_content id=\"3\" ohne Ende"
        );
        assert_eq!(
            without_pasted_wrapper("<pasted_contents>a</pasted_contents>"),
            "<pasted_contents>a</pasted_contents>"
        );
    }

    #[test]
    fn only_a_wrapper_leaves_nothing() {
        assert_eq!(
            without_pasted_wrapper("<pasted_content id=\"9\"></pasted_content>"),
            ""
        );
    }
}
