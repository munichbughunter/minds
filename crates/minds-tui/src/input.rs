//! Tasten → Aktionen. Reines Mapping, ohne Terminal, damit jede Taste
//! ohne Terminal prüfbar ist.
//!
//! Vim-nah, nicht Vim: `j`/`k` und die Pfeile sind gleich, `q` beendet,
//! `Esc` geht einen Schritt zurück und beendet nie (B1).
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
    /// Der Evidence-Report der Session.
    Evidence,
    /// Die Detailstufe 1–3 (direkt; die Tasten sind `z` / `Z`).
    Zoom(u8),
    /// `z` (`true`): feiner — mehr Details; `Z`: gröber.
    ZoomStep(bool),
    /// `v` — den Commit unter dem Fokus prüfen (in der Übersicht, EVIDENCE).
    Verify,
    /// `d` — der Diff des Commits unter dem Fokus.
    Diff,
    /// `i` — die Anforderung (Intent-Anker) der Session unter dem Fokus.
    Intent,
    /// `c` — im Changes-Tab die Spalte „Why this line?“ ein/aus.
    Context,
    /// Suche beginnen.
    SearchStart,
    /// Ein Zeichen in die Suche.
    SearchInput(char),
    /// Ein Zeichen aus der Suche.
    SearchBackspace,
    /// Suche übernehmen.
    SearchCommit,
    /// Neu laden — sofort, ohne auf den Fingerabdruck zu warten.
    Reload,
    /// Zum nächsten Tab (`true`) oder vorigen.
    CycleTab(bool),
    /// `Tab` (`true`) / `Shift-Tab` in der Dateizeile des Changes-Tabs
    /// (unter 160 Spalten, C9.2): die nächste/vorige Datei, umlaufend. Wird
    /// aus [`Action::CycleTab`] abgeleitet, nie direkt aus einer Taste.
    CycleFile(bool),
    /// Direkt zu einem Tab (F1, F2, …), nullbasiert.
    TabTo(u8),
    /// `]` (`true`) oder `[` — im Diff der nächste/vorige Hunk, in der
    /// Dateiliste der nächste/vorige Commit.
    Bracket(bool),
    /// `n` (`true`) oder `N` — die nächste/vorige unerklärte Zeile.
    Unexplained(bool),
    /// `s` — Unified ↔ Split.
    Split,
    /// `a` — in der Übersicht: alle Commits ↔ nur die mit Agent-Session
    /// (C3.3).
    AgentFilter,
    /// Space — in der Übersicht den Anforderungstext auf-/zuklappen (C3.4).
    Space,
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
        // `t` ist seit C11 frei: Die Story ist die Zeitleiste.
        KeyCode::Char('t') => Action::None,
        // `w` ist seit C10 frei: Die Herkunftskette gibt es nicht mehr, ihre
        // Glieder stehen in der Why-Box (Changes) und im Overview-Detail.
        KeyCode::Char('w') => Action::None,
        KeyCode::Char('e') => Action::Evidence,
        // B4: Ziffern wählen den Tab, `z` / `Z` zoomen, die Objekt-Verben
        // sind in jedem Tab dieselben. Seit C12.2 sind es vier Tabs — `5`
        // bleibt belegt, ohne Aktion (nichts verschwindet stillschweigend).
        KeyCode::Char(c @ '1'..='4') => Action::TabTo(c as u8 - b'1'),
        KeyCode::Char('5') => Action::None,
        KeyCode::Char('z') => Action::ZoomStep(true),
        KeyCode::Char('Z') => Action::ZoomStep(false),
        KeyCode::Char('v') => Action::Verify,
        KeyCode::Char('d') => Action::Diff,
        KeyCode::Char('i') => Action::Intent,
        KeyCode::Char('c') => Action::Context,
        KeyCode::Char('r') => Action::Reload,
        KeyCode::Tab => Action::CycleTab(true),
        KeyCode::BackTab => Action::CycleTab(false),
        KeyCode::F(n @ 1..=9) => Action::TabTo(n - 1),
        KeyCode::Char(']') => Action::Bracket(true),
        KeyCode::Char('[') => Action::Bracket(false),
        KeyCode::Char('n') => Action::Unexplained(true),
        KeyCode::Char('N') => Action::Unexplained(false),
        KeyCode::Char('s') => Action::Split,
        KeyCode::Char('a') => Action::AgentFilter,
        KeyCode::Char(' ') => Action::Space,
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
            // C10: `w` bleibt in der Tabelle — jetzt ohne Aktion; die Kette
            // ist weg, nichts verschwindet stillschweigend.
            ('w', Action::None),
            ('e', Action::Evidence),
            // C11: `t` bleibt in der Tabelle — die Zeitleiste ist die Story.
            ('t', Action::None),
            // B4: Ziffern sind Tabs, Zoom liegt auf z / Z. C12.2: vier Tabs
            // (Overview · Changes · Sessions · Requirements); `5` bleibt in
            // der Tabelle — ohne Aktion, nichts verschwindet stillschweigend.
            ('1', Action::TabTo(0)),
            ('2', Action::TabTo(1)),
            ('3', Action::TabTo(2)),
            ('4', Action::TabTo(3)),
            ('5', Action::None),
            ('z', Action::ZoomStep(true)),
            ('Z', Action::ZoomStep(false)),
            ('v', Action::Verify),
            ('d', Action::Diff),
            ('i', Action::Intent),
            ('c', Action::Context),
            ('/', Action::SearchStart),
            ('?', Action::Help),
            ('q', Action::Quit),
            ('r', Action::Reload),
            (']', Action::Bracket(true)),
            ('[', Action::Bracket(false)),
            ('n', Action::Unexplained(true)),
            ('N', Action::Unexplained(false)),
            ('s', Action::Split),
            ('l', Action::Enter),
            ('h', Action::Back),
        ] {
            assert_eq!(map(key(KeyCode::Char(c)), false), action, "{c}");
        }
        assert_eq!(map(key(KeyCode::Enter), false), Action::Enter);
        assert_eq!(map(key(KeyCode::Esc), false), Action::Back);
        assert_eq!(map(key(KeyCode::Char('x')), false), Action::None);
        // Keine Ziffer zoomt mehr (B4).
        for c in ['1', '2', '3'] {
            assert!(
                !matches!(map(key(KeyCode::Char(c)), false), Action::Zoom(_)),
                "{c}"
            );
        }
    }

    /// B4 erweitert die Such-Regel (Klasse 1, unverändert oben): Auch die
    /// neuen Tasten landen beim Tippen einer Suche in der Suche.
    #[test]
    fn b4_while_searching_the_new_keys_go_into_the_query() {
        for c in ['1', '2', '3', '4', '5', 'z', 'Z', 'v', 'd', 'i', 'c'] {
            assert_eq!(
                map(key(KeyCode::Char(c)), true),
                Action::SearchInput(c),
                "{c}"
            );
        }
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

    /// C3.3: `a` schaltet den Agent-Filter — im Suchmodus landet `a` in
    /// der Suche wie jeder Buchstabe.
    #[test]
    fn a_toggles_the_agent_filter_and_searching_takes_it_as_a_letter() {
        assert_eq!(map(key(KeyCode::Char('a')), false), Action::AgentFilter);
        assert_eq!(map(key(KeyCode::Char('a')), true), Action::SearchInput('a'));
    }

    /// C3.4: Space klappt auf — im Suchmodus ist es ein Leerzeichen.
    #[test]
    fn space_toggles_and_searching_takes_it_as_text() {
        assert_eq!(map(key(KeyCode::Char(' ')), false), Action::Space);
        assert_eq!(map(key(KeyCode::Char(' ')), true), Action::SearchInput(' '));
    }
}
