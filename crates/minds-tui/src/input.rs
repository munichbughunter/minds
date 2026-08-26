//! Tasten → Aktionen. Reines Mapping, ohne Terminal, damit jede Taste
//! ohne Terminal prüfbar ist.
//!
//! Vim-nah, nicht Vim: `j`/`k` und die Pfeile sind gleich, `q` beendet,
//! `Esc` geht einen Schritt zurück (und beendet auf der obersten Ebene).
//! Im Suchmodus gehen Zeichen in die Suche — bis auf `Esc`, `Enter` und
//! `Ctrl-C`, die immer dasselbe tun.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Was der Nutzer will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Eine Zeile hoch.
    Up,
    /// Eine Zeile runter.
    Down,
    /// Eine Seite hoch.
    PageUp,
    /// Eine Seite runter.
    PageDown,
    /// An den Anfang.
    Home,
    /// Ans Ende.
    End,
    /// Öffnen / hinein.
    Enter,
    /// Zurück / Filter löschen / beenden.
    Back,
    /// Graph ↔ Zeitleiste.
    ToggleTimeline,
    /// Die Herkunftskette.
    Why,
    /// Der Evidence-Report der Session.
    Evidence,
    /// Die Detailstufe 1–3.
    Zoom(u8),
    /// Suche beginnen.
    SearchStart,
    /// Ein Zeichen in die Suche.
    SearchInput(char),
    /// Ein Zeichen aus der Suche.
    SearchBackspace,
    /// Suche übernehmen.
    SearchCommit,
    /// Hilfe ein/aus.
    Help,
    /// Beenden.
    Quit,
    /// Nichts.
    None,
}

/// Deutet eine Taste. `searching` sagt, ob gerade in die Suche getippt wird.
pub fn map(key: KeyEvent, searching: bool) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    if searching {
        return match key.code {
            KeyCode::Esc => Action::Back,
            KeyCode::Enter => Action::SearchCommit,
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Action::SearchInput(c)
            }
            KeyCode::Up => Action::Up,
            KeyCode::Down => Action::Down,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Home | KeyCode::Char('g') => Action::Home,
        KeyCode::End | KeyCode::Char('G') => Action::End,
        KeyCode::Enter | KeyCode::Char('l') => Action::Enter,
        KeyCode::Esc | KeyCode::Char('h') => Action::Back,
        KeyCode::Char('t') => Action::ToggleTimeline,
        KeyCode::Char('w') => Action::Why,
        KeyCode::Char('e') => Action::Evidence,
        KeyCode::Char('1') => Action::Zoom(1),
        KeyCode::Char('2') => Action::Zoom(2),
        KeyCode::Char('3') => Action::Zoom(3),
        KeyCode::Char('/') => Action::SearchStart,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('q') => Action::Quit,
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn vim_and_arrow_keys_agree() {
        assert_eq!(map(key(KeyCode::Char('j')), false), Action::Down);
        assert_eq!(map(key(KeyCode::Down), false), Action::Down);
        assert_eq!(map(key(KeyCode::Char('k')), false), Action::Up);
        assert_eq!(map(key(KeyCode::Up), false), Action::Up);
        assert_eq!(map(key(KeyCode::Char('g')), false), Action::Home);
        assert_eq!(map(key(KeyCode::Char('G')), false), Action::End);
    }

    #[test]
    fn every_command_key_has_its_action() {
        for (c, action) in [
            ('w', Action::Why),
            ('e', Action::Evidence),
            ('t', Action::ToggleTimeline),
            ('1', Action::Zoom(1)),
            ('2', Action::Zoom(2)),
            ('3', Action::Zoom(3)),
            ('/', Action::SearchStart),
            ('?', Action::Help),
            ('q', Action::Quit),
            ('l', Action::Enter),
            ('h', Action::Back),
        ] {
            assert_eq!(map(key(KeyCode::Char(c)), false), action, "{c}");
        }
        assert_eq!(map(key(KeyCode::Enter), false), Action::Enter);
        assert_eq!(map(key(KeyCode::Esc), false), Action::Back);
        assert_eq!(map(key(KeyCode::Char('x')), false), Action::None);
    }

    #[test]
    fn while_searching_letters_go_into_the_query() {
        assert_eq!(map(key(KeyCode::Char('q')), true), Action::SearchInput('q'));
        assert_eq!(map(key(KeyCode::Char('/')), true), Action::SearchInput('/'));
        assert_eq!(map(key(KeyCode::Backspace), true), Action::SearchBackspace);
        assert_eq!(map(key(KeyCode::Enter), true), Action::SearchCommit);
        assert_eq!(map(key(KeyCode::Esc), true), Action::Back);
    }

    #[test]
    fn ctrl_c_always_quits() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map(ctrl_c, false), Action::Quit);
        assert_eq!(map(ctrl_c, true), Action::Quit);
    }
}
