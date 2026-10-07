//! Blieb die Arbeit im erklärten Bereich (EA-17)? Zur Lesezeit aus
//! gespeichertem Material berechnet, nie gespeichert (W2/W5).
//!
//! Erklärt der Intent-Anker einen Bereich (`scope=<glob>[,<glob>…]`), ist
//! jeder Pfad außerhalb davon ein Befund [`Finding::OutOfScope`] auf der
//! Coverage-Achse — gleich, woher er stammt:
//!
//! - **Commit:** eine geänderte Datei des geprüften Commits (auch
//!   Submodul-Zeiger, Modus-Wechsel, Symlinks);
//! - **Claim:** ein Schreib- oder Lösch-Claim der Session
//!   ([`Claims::paths`]);
//! - **Observation:** eine Beobachtung des Datei-Beobachters im Fenster der
//!   Session.
//!
//! Ein Befund ändert nie Verdikt oder Exit-Code (W6); nur das ausdrückliche
//! Gate `--require-in-scope` macht daraus Exit 2. `scope=-` (kein Bereich)
//! heißt: keine Aussage, keine Befunde.
//!
//! # Was der Bereich nicht umfasst
//!
//! Globs sind repo-relativ. Ein Claim, der keinen Pfad dieses Repositorys
//! nennt (eine Datei außerhalb des Checkouts, eine nicht eindeutig
//! zuordenbare Schreibweise, siehe [`crate::reconcile::claim_path`]), liegt
//! jenseits der Beobachtungsgrenze und ist keine Scope-Frage.
//!
//! # Wem der Bereich gehört
//!
//! Der Bereich stammt aus dem Anker, an den die Session gebunden ist — so,
//! wie [`crate::intent::intent_of`] ihn ermittelt. Ob ein Mensch ihn
//! freigegeben hat (Signatur unter `minds-intent`) und ob der Witness ihn
//! verkettet hat, sagt die Intent-Zeile bzw. die Assurance; ein Bereich, den
//! sich der Agent selbst gesetzt hat, ist nur so viel wert wie diese.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use minds_core::intent_anchor::IntentAnchor;
use minds_store::ContextStore;

use crate::assurance::IntentState;
use crate::reconcile::{Claims, FsObservation};

/// Die Art eines Befunds — das Wort aus dem gemeinsamen Vokabular
/// (`00-conventions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Finding {
    /// Ein Pfad außerhalb des erklärten Bereichs.
    OutOfScope,
}

impl Finding {
    /// Das Wort der CLI.
    pub const fn word(self) -> &'static str {
        match self {
            Self::OutOfScope => "out of scope",
        }
    }
}

/// Woher ein Pfad außerhalb des Bereichs bekannt ist. Die Reihenfolge ist
/// die der Ausgabe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeSource {
    /// Eine geänderte Datei des geprüften Commits.
    Commit,
    /// Ein Schreib- oder Lösch-Claim der Session.
    Claim,
    /// Eine Beobachtung des Datei-Beobachters.
    Observation,
}

impl ScopeSource {
    /// Das Wort der CLI.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Claim => "claim",
            Self::Observation => "observation",
        }
    }
}

/// Ein Pfad außerhalb des Bereichs, mit allen Quellen, die ihn nennen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFinding {
    /// Repo-relativer Pfad, unentschärft — eine Identität, kein Anzeigetext.
    pub path: String,
    /// Die Quellen, aufsteigend sortiert, ohne Dubletten; nie leer.
    pub sources: Vec<ScopeSource>,
}

impl ScopeFinding {
    /// Die Art des Befunds.
    pub const fn finding(&self) -> Finding {
        Finding::OutOfScope
    }
}

/// Ein erklärter Bereich: die Globs eines Ankers, übersetzt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    globs: Vec<Glob>,
}

impl Scope {
    /// Der Bereich des Ankers; `None` bei `scope=-`.
    pub fn of(anchor: &IntentAnchor) -> Option<Self> {
        Self::from_globs(&anchor.scope)
    }

    /// Der Bereich aus rohen Globs; `None`, wenn keine angegeben sind.
    pub fn from_globs<S: AsRef<str>>(globs: &[S]) -> Option<Self> {
        (!globs.is_empty()).then(|| Self {
            globs: globs.iter().map(|g| Glob::new(g.as_ref())).collect(),
        })
    }

    /// Liegt `path` (repo-relativ, `/`-getrennt) im Bereich?
    ///
    /// Beschränkt, auch gegen einen Bereich, den der Agent selbst gebunden
    /// hat (bis 64 KiB Globs): Ein Pfad über [`MAX_PATH`] Bytes liegt nie im
    /// Bereich, und kostet der Abgleich mehr als [`MATCH_BUDGET`] Schritte,
    /// ebenso nicht — fail-closed, ein Befund statt eines hängenden Laufs.
    ///
    /// Das Budget gilt für alle Globs zusammen, in ihrer Reihenfolge: Ein
    /// teurer Fehltreffer vor einem billigen Treffer kann den Pfad nach
    /// außen kippen — fail-closed, nie nach innen.
    pub fn contains(&self, path: &str) -> bool {
        self.contains_within(path, &Cell::new(MATCH_BUDGET))
    }

    /// Wie [`Scope::contains`], zieht die Schritte aber zusätzlich von
    /// `run` ab — dem Budget eines ganzen Laufs ([`RUN_BUDGET`]). Ist es
    /// leer, liegt jeder weitere Pfad außerhalb.
    fn contains_within(&self, path: &str, run: &Cell<u64>) -> bool {
        if path.len() > MAX_PATH || run.get() == 0 {
            return false;
        }
        // Einmal zerlegt, für alle Globs — nicht je Glob und Vergleich neu.
        let parts = split(path);
        let granted = run.get().min(MATCH_BUDGET);
        let budget = Cell::new(granted);
        let inside = self
            .globs
            .iter()
            .any(|glob| glob.matches_parts(&parts, &budget));
        run.set(run.get() - (granted - budget.get()));
        inside && budget.get() > 0
    }
}

/// Längster Pfad (Bytes), den [`Scope::contains`] abgleicht — `PATH_MAX`
/// der üblichen Systeme. Längere Pfade liegen außerhalb.
pub const MAX_PATH: usize = 4096;

/// Höchstens so viele Abgleich-Schritte für alle Pfade eines
/// [`scope_findings`]-Laufs — der Agent bestimmt Bereich (A1) und Zahl der
/// Pfade. Ist es erschöpft, liegt jeder weitere Pfad außerhalb.
pub const RUN_BUDGET: u64 = 1 << 26;

/// Höchstens so viele Abgleich-Schritte je Pfad (über alle Globs des
/// Bereichs). Ein gewöhnlicher Bereich braucht wenige Tausend.
pub const MATCH_BUDGET: u64 = 1 << 22;

/// Warum über den Bereich einer Session nichts sagbar ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoScope {
    /// Die Session ist an keinen Anker gebunden.
    IntentNotBound,
    /// Der Anker erklärt keinen Bereich (`scope=-`).
    NotDeclared,
    /// Der Anker liegt nicht in diesem Store (etwa noch nicht gesynct).
    AnchorMissing,
    /// Der Anker ist verändert oder nicht lesbar.
    AnchorUnreadable,
    /// Die Beobachtungen des Witness sind unvollständig: Ein bezeugter Seal
    /// nennt ein Objekt, das fehlt oder nicht lesbar ist
    /// ([`crate::observations::WindowObservations::complete`]), oder eine
    /// bezeugte Session hat kein geschlossenes Beobachtungsfenster — was
    /// fehlt, könnte außerhalb des Bereichs liegen.
    ObservationsIncomplete,
    /// Die Integrität ist verletzt: Aus verändertem Material sind Befunde
    /// keine Fakten.
    IntegrityViolated,
    /// Die Pfade der Claims ließen sich nicht gegen die Bäume des Commits
    /// prüfen (Lesefehler) — ein Claim fiele sonst still weg.
    ClaimsUnresolvable,
    /// Ein Glob verneint (`!…`) — das kennt der Matcher nicht.
    UnsupportedGlob,
    /// Vor der Evidence-Chain erfasst (keine Seals).
    NoEvidenceChain,
    /// Die Nutzlast der Session ist nicht lesbar (vergessen, fehlt): Ob und
    /// woran sie gebunden war, ist unbekannt.
    PayloadUnreadable,
    /// Ein späteres Intent-Event nennt einen anderen Anker: Welcher Bereich
    /// für welche Arbeit galt, ist nicht entscheidbar — der jüngste allein
    /// könnte nachträglich weiter sein.
    IntentChangedMidSession,
}

impl NoScope {
    /// Der Grund als Text.
    pub const fn word(self) -> &'static str {
        match self {
            Self::IntentNotBound => "intent not bound",
            Self::NotDeclared => "no scope declared",
            Self::AnchorMissing => "intent anchor not in this store",
            Self::AnchorUnreadable => "intent anchor unreadable",
            Self::ObservationsIncomplete => "witness observations incomplete",
            Self::IntegrityViolated => "integrity violated",
            Self::IntentChangedMidSession => "intent changed mid-session",
            Self::PayloadUnreadable => "session payload unreadable",
            Self::UnsupportedGlob => "negated scope glob unsupported",
            Self::NoEvidenceChain => "captured before the evidence chain",
            Self::ClaimsUnresolvable => "claim paths not resolvable",
        }
    }
}

/// Der erklärte Bereich der Intent-Lage `intent`, aus dem Store gelesen —
/// der Anker hash-geprüft gegen seine Id (`get_intent`). Strikt lesend.
pub fn declared_scope(store: &dyn ContextStore, intent: &IntentState) -> Result<Scope, NoScope> {
    let IntentState::Bound {
        anchor_id,
        changed_mid_session,
        ..
    } = intent
    else {
        return Err(NoScope::IntentNotBound);
    };
    if *changed_mid_session {
        return Err(NoScope::IntentChangedMidSession);
    }
    match store.get_intent(anchor_id) {
        // Eine Verneinung (`!src/auth/**`) kennt dieser Glob nicht; wörtlich
        // genommen träfe sie nichts, und der Bereich wäre weiter als
        // freigegeben.
        Ok(Some(stored)) if stored.anchor.scope.iter().any(|g| g.starts_with('!')) => {
            Err(NoScope::UnsupportedGlob)
        }
        Ok(Some(stored)) => Scope::of(&stored.anchor).ok_or(NoScope::NotDeclared),
        Ok(None) => Err(NoScope::AnchorMissing),
        Err(_) => Err(NoScope::AnchorUnreadable),
    }
}

/// Die Pfade außerhalb von `scope`, nach Pfad sortiert.
///
/// - `changed`: die geänderten Pfade des Commits (leer ohne Commit);
/// - `claims`: die Claims der Session, schon auf repo-relative Pfade
///   abgebildet;
/// - `observations`: die Beobachtungen im Fenster der Session, nur aus
///   vertrauenswürdigen Seals (der Aufrufer filtert).
///
/// Rein und deterministisch: keine I/O. Die Reihenfolge der Eingaben zählt
/// nur, wenn [`RUN_BUDGET`] erschöpft ist — dann entscheidet sie, welche
/// Pfade noch geprüft werden; alle übrigen liegen außerhalb.
pub fn scope_findings<'a>(
    scope: &Scope,
    changed: impl IntoIterator<Item = &'a str>,
    claims: &Claims<'_>,
    observations: &[FsObservation],
) -> Vec<ScopeFinding> {
    let mut found: BTreeMap<&str, BTreeSet<ScopeSource>> = BTreeMap::new();
    let tagged = changed
        .into_iter()
        .map(|path| (path, ScopeSource::Commit))
        .chain(claims.paths().map(|path| (path, ScopeSource::Claim)))
        .chain(
            observations
                .iter()
                .map(|o| (o.path.as_str(), ScopeSource::Observation)),
        );
    // Jeder Pfad wird einmal geprüft, gleich wie oft und von welcher Quelle
    // er genannt wird — alle zusammen im Laufbudget.
    let run = Cell::new(RUN_BUDGET);
    let mut verdict: BTreeMap<&str, bool> = BTreeMap::new();
    for (path, source) in tagged {
        let inside = *verdict
            .entry(path)
            .or_insert_with(|| scope.contains_within(path, &run));
        if !inside {
            found.entry(path).or_default().insert(source);
        }
    }
    found
        .into_iter()
        .map(|(path, sources)| ScopeFinding {
            path: path.to_owned(),
            sources: sources.into_iter().collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Glob
// ---------------------------------------------------------------------------

/// Ein Pfad-Glob, angelehnt an Gits `:(glob)`-Pathspec — bewusst klein,
/// ohne neue Abhängigkeit:
///
/// - Der Glob ist an der Repository-Wurzel verankert: `*.rs` trifft
///   `main.rs`, nicht `src/main.rs`. Ein führendes `/` oder `./` ändert
///   daran nichts.
/// - Ein Glob ohne Platzhalter im letzten Segment trifft auch alles darin,
///   wie ein Pathspec: `src/retry` trifft `src/retry` und
///   `src/retry/backoff.rs`. Mit Platzhalter nicht: `src/*` trifft
///   `src/a`, nicht `src/a/b`.
/// - `*` trifft beliebig viele Zeichen innerhalb eines Pfadsegments, `?`
///   genau eines — nie `/`.
/// - `**` als ganzes Segment trifft null oder mehr Segmente: `**/a` trifft
///   `a` und `x/y/a`, `a/**/b` trifft `a/b` und `a/x/b`. Am Ende trifft es
///   alles **innerhalb**: `a/**` trifft `a/x`, nicht `a` selbst. Innerhalb
///   eines Segments (`a**b`) wirkt `**` wie `*`.
/// - Ein abschließendes `/` meint ein Verzeichnis: `src/` wie `src/**`.
/// - Punktdateien sind keine Ausnahme: `*` trifft `.env`, `**` durchläuft
///   `.github/`.
/// - Alles andere ist wörtlich, auch `[`, `{`, `\` — keine Zeichenklassen,
///   keine Klammer-Expansion, kein Escaping. Groß- und Kleinschreibung
///   zählt, Unicode wird nicht normalisiert (Pfade vergleicht Git als
///   Bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    /// Die Formen, von denen eine treffen muss: der Glob selbst, bei
    /// wörtlichem letztem Segment zusätzlich „alles darin".
    forms: Vec<Vec<Segment>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// `**`: null oder mehr ganze Segmente.
    AnyDepth,
    /// Genau ein Segment, Zeichen für Zeichen.
    Pattern(Vec<Token>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// `*`
    Any,
    /// `?`
    One,
    Literal(char),
}

impl Glob {
    /// Übersetzt `pattern`. Jede Zeichenfolge ist ein Glob — schlimmstenfalls
    /// einer, der nichts trifft (ein leeres Segment wie in `a//b`).
    pub fn new(pattern: &str) -> Self {
        let pattern = pattern
            .strip_prefix("./")
            .or_else(|| pattern.strip_prefix('/'))
            .unwrap_or(pattern);
        let (pattern, directory) = match pattern.strip_suffix('/') {
            Some(inner) => (inner, true),
            None => (pattern, false),
        };
        let mut segments: Vec<Segment> = Vec::new();
        for part in pattern.split('/') {
            let segment = if part == "**" {
                Segment::AnyDepth
            } else {
                let mut tokens: Vec<Token> = Vec::new();
                for c in part.chars() {
                    let token = match c {
                        '*' => Token::Any,
                        '?' => Token::One,
                        c => Token::Literal(c),
                    };
                    // `**…*` ist `*`: Ein Lauf von Sternen bleibt einer —
                    // sonst kostete die Endprüfung ungezählte Schritte.
                    if !(token == Token::Any && tokens.last() == Some(&Token::Any)) {
                        tokens.push(token);
                    }
                }
                Segment::Pattern(tokens)
            };
            // `**/**` ist `**`.
            if !(segment == Segment::AnyDepth && segments.last() == Some(&Segment::AnyDepth)) {
                segments.push(segment);
            }
        }
        let literal = matches!(segments.last(), Some(Segment::Pattern(tokens))
            if !tokens.is_empty() && tokens.iter().all(|t| matches!(t, Token::Literal(_))));
        let mut forms = Vec::new();
        if literal && !directory {
            forms.push(segments.clone());
        }
        if (directory || literal) && segments.last() != Some(&Segment::AnyDepth) {
            segments.push(Segment::AnyDepth);
        }
        // Ein abschließendes `**` trifft nur Inhalt, nicht den Ort selbst:
        // mindestens ein weiteres Segment.
        if segments.last() == Some(&Segment::AnyDepth) {
            segments.push(Segment::Pattern(vec![Token::Any]));
        }
        forms.push(segments);
        Self { forms }
    }

    /// Trifft der Glob `path` (repo-relativ, `/`-getrennt)?
    /// Erschöpft der Abgleich [`MATCH_BUDGET`], trifft der Glob nicht.
    pub fn matches(&self, path: &str) -> bool {
        let budget = Cell::new(MATCH_BUDGET);
        self.matches_parts(&split(path), &budget) && budget.get() > 0
    }

    fn matches_parts(&self, parts: &[Vec<char>], budget: &Cell<u64>) -> bool {
        self.forms
            .iter()
            .any(|form| matches_form(form, parts, budget))
    }
}

/// Ein Pfad als Segmente aus Zeichen.
fn split(path: &str) -> Vec<Vec<char>> {
    path.split('/').map(|part| part.chars().collect()).collect()
}

/// Trifft eine Form die Segmente `parts`?
fn matches_form(form: &[Segment], parts: &[Vec<char>], budget: &Cell<u64>) -> bool {
    wildcard(
        form,
        parts,
        budget,
        |s| *s == Segment::AnyDepth,
        |segment, part| match segment {
            Segment::Pattern(tokens) => wildcard(
                tokens,
                part,
                budget,
                |t| *t == Token::Any,
                |token, c| match token {
                    Token::One => true,
                    Token::Literal(l) => l == c,
                    Token::Any => false,
                },
            ),
            Segment::AnyDepth => false,
        },
    )
}

/// Der klassische Platzhalter-Abgleich mit Rücksprung zum letzten Stern —
/// linear im Normalfall, höchstens `O(n·m)`, nie exponentiell. Ein Stern
/// trifft null oder mehr Elemente, jedes andere Muster-Element genau eins.
/// Jeder Schritt kostet eins aus `budget`; ist es leer, trifft nichts mehr.
fn wildcard<P, T>(
    pattern: &[P],
    text: &[T],
    budget: &Cell<u64>,
    star: impl Fn(&P) -> bool,
    one: impl Fn(&P, &T) -> bool,
) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut resume: Option<(usize, usize)> = None;
    while t < text.len() {
        let Some(left) = budget.get().checked_sub(1) else {
            return false;
        };
        budget.set(left);
        if p < pattern.len() {
            if star(&pattern[p]) {
                resume = Some((p, t));
                p += 1;
                continue;
            }
            if one(&pattern[p], &text[t]) {
                p += 1;
                t += 1;
                continue;
            }
        }
        match resume {
            // Der Stern schluckt ein Element mehr.
            Some((star_at, from)) => {
                p = star_at + 1;
                t = from + 1;
                resume = Some((star_at, from + 1));
            }
            None => return false,
        }
    }
    pattern[p..].iter().all(star)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Tabelle: Glob, Pfad, trifft?
    const TABLE: &[(&str, &str, bool)] = &[
        // Wörtlich und verankert.
        ("Cargo.toml", "Cargo.toml", true),
        ("Cargo.toml", "crates/a/Cargo.toml", false),
        ("Cargo.toml", "Cargo.tom", false),
        ("Cargo.toml", "Cargo.toml.bak", false),
        ("src/main.rs", "src/main.rs", true),
        ("src/main.rs", "src/Main.rs", false),
        ("/src/main.rs", "src/main.rs", true),
        ("./src/main.rs", "src/main.rs", true),
        ("./src/**", "src/a.rs", true),
        // Wörtliches letztes Segment: auch alles darin, wie ein Pathspec.
        ("src", "src/a.rs", true),
        ("src", "src", true),
        ("src", "srcx/a.rs", false),
        ("src/retry", "src/retry/x.rs", true),
        ("src/retry", "src/retry/a/b.rs", true),
        ("src/retry", "src/retrying.rs", false),
        ("src/*.rs", "src/a.rs/x", false),
        // `*` bleibt im Segment.
        ("*.rs", "main.rs", true),
        ("*.rs", "src/main.rs", false),
        ("src/*.rs", "src/main.rs", true),
        ("src/*.rs", "src/a/main.rs", false),
        ("src/*", "src/a", true),
        ("src/*", "src/a/b", false),
        ("src/*", "src", false),
        ("tests/retry_*.rs", "tests/retry_backoff.rs", true),
        ("tests/retry_*.rs", "tests/retry_.rs", true),
        ("tests/retry_*.rs", "tests/other.rs", false),
        ("a*b*c", "abc", true),
        ("a*b*c", "axxbyyc", true),
        ("a*b*c", "axxbyy", false),
        ("*", "a", true),
        ("*", "a/b", false),
        // `?` genau ein Zeichen, nie `/`.
        ("?.rs", "a.rs", true),
        ("?.rs", "ab.rs", false),
        ("?.rs", ".rs", false),
        ("a?b", "a/b", false),
        ("ä?.rs", "äö.rs", true),
        // `**` am Anfang.
        ("**/foo.rs", "foo.rs", true),
        ("**/foo.rs", "a/foo.rs", true),
        ("**/foo.rs", "a/b/c/foo.rs", true),
        ("**/foo.rs", "a/xfoo.rs", false),
        ("**/*.md", "README.md", true),
        ("**/*.md", "docs/x/README.md", true),
        // `**` in der Mitte.
        ("a/**/b", "a/b", true),
        ("a/**/b", "a/x/b", true),
        ("a/**/b", "a/x/y/z/b", true),
        ("a/**/b", "a/x/y/z/c", false),
        ("a/**/b", "x/a/b", false),
        ("a/**/b/**/c", "a/1/b/2/3/c", true),
        ("a/**/b/**/c", "a/b/c", true),
        ("a/**/**/b", "a/b", true),
        // `**` am Ende: alles innerhalb, nicht der Ort selbst.
        ("src/**", "src/a.rs", true),
        ("src/**", "src/a/b/c.rs", true),
        ("src/**", "src", false),
        ("src/**", "srcx/a.rs", false),
        ("src/**", "other/src/a.rs", false),
        ("**", "a", true),
        ("**", "a/b/c", true),
        // `**` innerhalb eines Segments wirkt wie `*`.
        ("src/a**b", "src/axxb", true),
        ("src/a**b", "src/ax/xb", false),
        ("x**", "x/y", false),
        // Abschließendes `/`: ein Verzeichnis.
        ("src/", "src/a.rs", true),
        ("src/", "src/a/b.rs", true),
        ("src/", "src", false),
        ("docs/sub/", "docs/sub/x.md", true),
        ("src/**/", "src/a/b.rs", true),
        // Punktdateien sind keine Ausnahme.
        ("*", ".env", true),
        (".*", ".env", true),
        ("*.yml", ".travis.yml", true),
        ("**/*.yml", ".github/workflows/ci.yml", true),
        ("src/**", "src/.hidden/x", true),
        ("**", ".git-blame-ignore-revs", true),
        ("?env", ".env", true),
        // Alles andere ist wörtlich.
        ("src/{a,b}.rs", "src/a.rs", false),
        ("src/{a,b}.rs", "src/{a,b}.rs", true),
        ("src/[ab].rs", "src/a.rs", false),
        ("src/[ab].rs", "src/[ab].rs", true),
        ("a\\*b", "a\\xb", true),
        // Was nichts trifft.
        ("a//b", "a/b", false),
        ("/", "a", false),
        ("", "a", false),
    ];

    #[test]
    fn glob_table() {
        for (glob, path, expected) in TABLE {
            assert_eq!(
                Glob::new(glob).matches(path),
                *expected,
                "glob {glob:?} against {path:?}"
            );
        }
    }

    #[test]
    fn glob_stars_never_blow_up() {
        // Viele Sterne gegen einen langen Fehltreffer: linear, nicht
        // exponentiell — der Test endet sofort.
        let glob = Glob::new(&format!("{}b", "a*".repeat(64)));
        assert!(!glob.matches(&"a".repeat(10_000)));
        let deep = Glob::new(&format!("{}x", "**/a/".repeat(64)));
        assert!(!deep.matches(&vec!["a"; 5_000].join("/")));
        // Globs, die zum Rücksprung zwingen (`*` + 2048 × `a` + `b` gegen
        // 4096 × `a`, je rund 4 Mio. Schritte): Das Budget je Pfad
        // erschöpft sich wirklich — und der Pfad liegt außerhalb.
        let costly = format!("*{}b", "a".repeat(2_048));
        let mut globs = vec![costly; 3];
        let hostile = Scope::from_globs(&globs).unwrap();
        let run = Cell::new(RUN_BUDGET);
        assert!(!hostile.contains_within(&"a".repeat(MAX_PATH), &run));
        assert_eq!(RUN_BUDGET - run.get(), MATCH_BUDGET);
        globs.push("**".into());
        let scope = Scope::from_globs(&globs).unwrap();
        // Teurer Fehltreffer zuerst: fail-closed nach außen, nie nach innen.
        assert!(!scope.contains(&"a".repeat(MAX_PATH)));
        // Das Laufbudget: Nach ihm liegt jeder weitere Pfad außerhalb, ohne
        // dass er noch Arbeit kostet.
        let run = Cell::new(MATCH_BUDGET * 2);
        assert!(!scope.contains_within(&"a".repeat(MAX_PATH), &run));
        assert!(!scope.contains_within(&"a".repeat(MAX_PATH - 1), &run));
        assert_eq!(run.get(), 0);
        let cheap = Scope::from_globs(&["**"]).unwrap();
        assert!(!cheap.contains_within("src/a.rs", &run));
        assert!(cheap.contains_within("src/a.rs", &Cell::new(RUN_BUDGET)));
        // Ein gewöhnlicher Bereich bleibt weit unter dem Budget.
        // Ein Lauf von 60 000 Sternen ist ein Stern: Die Endprüfung bleibt
        // billig.
        let stars = Scope::from_globs(&[format!("**/a{}/b", "*".repeat(60_000))]).unwrap();
        let deep = format!("{}/c", vec!["a"; 2_000].join("/"));
        let start = std::time::Instant::now();
        assert!(!stars.contains(&deep));
        assert!(stars.contains("x/a/b"));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let many: Vec<String> = (0..2_000).map(|i| format!("crates/c{i}/**")).collect();
        let many = Scope::from_globs(&many).unwrap();
        assert!(many.contains("crates/c1999/src/lib.rs"));
    }

    #[test]
    fn scope_is_any_of_its_globs() {
        let scope = Scope::from_globs(&["src/retry/**", "tests/retry_*.rs"]).unwrap();
        assert!(scope.contains("src/retry/backoff.rs"));
        assert!(scope.contains("tests/retry_x.rs"));
        assert!(!scope.contains("src/lib.rs"));
        assert_eq!(Scope::from_globs::<&str>(&[]), None);
        // Überlange Pfade liegen fail-closed außerhalb.
        let all = Scope::from_globs(&["**"]).unwrap();
        assert!(all.contains(&"a/".repeat(MAX_PATH / 2)[..MAX_PATH - 1]));
        assert!(!all.contains(&"a".repeat(MAX_PATH + 1)));
    }

    #[test]
    fn finding_words_match_the_vocabulary() {
        assert_eq!(Finding::OutOfScope.word(), "out of scope");
        assert_eq!(
            [
                ScopeSource::Commit,
                ScopeSource::Claim,
                ScopeSource::Observation
            ]
            .map(ScopeSource::word),
            ["commit", "claim", "observation"]
        );
    }
}
