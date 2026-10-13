//! Conformance: ein Commit gegen das signierte Soll
//! ([`minds_core::spec`]) — je Regel *expected → observed → result*.
//!
//! # Evidence und Ableitung
//!
//! Evidence sind der Commit (content-addressed) und das signierte Soll im
//! Store (`refs/minds/specs/<id>`: der Umschlag aus Soll, Pins und
//! Signatur-Commit `at`, dazu die Signaturen). Alles hier ist
//! **Ableitung**: Aus denselben Objekten rechnet jeder dasselbe Ergebnis
//! nach, nichts wird geschrieben.
//!
//! # Welches Soll gilt
//!
//! Die Signaturen prüft der Aufrufer (`Inputs::verify`, gefragt nur für die
//! Umschläge, auf die es ankommt) — der Reader kennt keine Signer. Ein
//! Umschlag gilt für einen Commit nur, wenn er **frisch** ist: Sein
//! Signatur-Commit `at` liegt in der Historie des Commits, jeder Nachfahre
//! von `at` darin (der Commit eingeschlossen) trägt genau den signierten
//! Stand (Soll und jede gepinnte Datei), und jeder übrige Commit, der nicht
//! schon vor `at` lag (ein Seitenzweig, der vor `at` abzweigt), trägt diesen
//! Stand oder einen, den es vor `at` schon gab. Das hängt nur an der
//! Historie, nicht am Store, den der Agent mitschreiben (und leeren) kann:
//!
//! - Ein Rückfall (direkt, über eine Löschung, Füll-Commits, einen Merge
//!   gleich mit welcher Seite zuerst, oder auf einen Stand, der vor einer
//!   späteren Signatur schon einmal galt), ein lockererer Schema-Pin und ein
//!   zurückgespulter oder geleerter Store machen den älteren Umschlag
//!   unfrisch: DRIFT, bis der aktuelle Stand signiert ist. Auch ein
//!   wieder zurückgenommener unsignierter Zwischenstand verlangt eine neue
//!   Signatur.
//! - Kehrt ein Mensch bewusst zu einer älteren Fassung zurück, signiert er
//!   sie neu.
//!
//! Die erste Zeile jedes Berichts ist die Regel `spec`: PASS, wenn das Soll
//! im Commit einen frischen, gültig signierten Umschlag hat; sonst DRIFT, und
//! die Regeln kommen aus dem jüngsten gültigen Umschlag in der Historie (nach
//! Generation), nie aus dem Soll im Commit. Nicht entscheidbar
//! (Deckel, Shallow Clone, Signaturen nicht prüfbar): UNKNOWN. Gilt in der
//! Historie gar kein Umschlag und liegt im Commit kein Soll, gibt es keinen
//! Bericht.
//!
//! # Lesen ohne Vertrauen
//!
//! Die Objektdatenbank kann dem beobachteten Agenten gehören. Gelesen wird
//! deshalb nur über hash-geprüfte, gedeckelte Zugriffe
//! ([`Repo::list_blobs_verified`], [`Repo::read_blob_verified`],
//! [`Repo::history`], [`Repo::entry_at`]); was scheitert oder einen
//! Deckel reißt, ergibt
//! UNKNOWN, nie PASS. Symlinks und Submodule unter einem Glob einer Regel
//! ebenso: Ihren Inhalt zeigt die Liste nicht. Kein `git`-Prozess.

mod imports;
mod manifest;
mod schema;

use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use minds_core::ContentHash;
use minds_core::spec::{Expect, MAX_SPEC, Rule, RuleKind, SPEC_PATH, SignedSpec, Spec, pin_of};
use minds_git::ancestry::{Budget, PathEntry, TreeCache};
use minds_git::{BlobId, CommitId, Repo, VerifiedListing};

use crate::scope::Glob;
use crate::text::{sanitize, sanitize_path};

/// So viele Commits liest eine Auswertung höchstens, um Umschläge in der
/// Historie zu verorten (über alle Umschläge zusammen).
pub const MAX_ANCESTRY: usize = 200_000;

/// So viele Bytes (Commits, Bäume, gepinnte Dateien) liest diese Verortung
/// höchstens.
pub const ANCESTRY_BYTES: u64 = 512 * 1024 * 1024;

/// Höchstzahl der gültigen Umschläge, die eine Auswertung verortet.
pub const MAX_ENVELOPES: usize = 256;

/// Höchstzahl der Einträge (Dateien, Verzeichnisse, Links) eines Commits.
pub const MAX_ENTRIES: usize = 200_000;

/// Höchstmenge der Baum-Bytes (und der Pfad-Bytes) je Listing.
pub const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;

/// Höchstmenge aller gelesenen Datei-Bytes je Bericht.
pub const READ_BUDGET: u64 = 64 * 1024 * 1024;

/// Höchstzahl der Schema-Prüfschritte je Bericht, über alle Dateien und
/// Regeln.
pub const STEP_BUDGET: u64 = 4_000_000;

/// Höchstzahl der Dateien, die eine Regel prüft.
pub const MAX_FILES: usize = 5_000;

/// Höchstgröße einer Quelldatei (`no-import`).
const MAX_SOURCE: u64 = 1024 * 1024;

/// Höchstgröße eines Cargo-Manifests.
const MAX_MANIFEST: u64 = 256 * 1024;

/// Höchstgröße eines Schemas und einer geprüften JSON-Datei.
const MAX_JSON: u64 = 4 * 1024 * 1024;

/// Das Ergebnis einer Regel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    /// Das Beobachtete erfüllt das Erwartete.
    Pass,
    /// Nicht entscheidbar — fehlende Daten, Deckel, unbekannte Regel.
    Unknown,
    /// Das Beobachtete weicht ab.
    Drift,
}

impl Outcome {
    /// `PASS`, `DRIFT`, `UNKNOWN`.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Drift => "DRIFT",
            Outcome::Unknown => "UNKNOWN",
        }
    }
}

/// Eine bewertete Regel. Alle Texte sind für eine Zeile entschärft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleResult {
    /// Die `id` aus dem Soll (`spec` für die Zeile des Solls selbst).
    pub id: String,
    /// Die Art der Regel.
    pub kind: String,
    /// Was das Soll verlangt.
    pub expected: String,
    /// Was der Commit zeigt.
    pub observed: String,
    /// PASS, DRIFT oder UNKNOWN.
    pub outcome: Outcome,
}

/// Was die Signaturprüfung des Aufrufers über ein Soll sagt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecTrust {
    /// Ein Umschlag mit diesem Soll ist gültig signiert (`minds-spec`) von
    /// einem vertrauten Signer.
    Valid {
        /// Wer signiert hat (Principal, wie der Prüfer ihn nennt).
        signer: String,
        /// Der signierte Umschlag: Pins und Signatur-Commit.
        signed: SignedSpec,
    },
    /// Keine Signatur im Store (oder das Soll liegt dort nicht).
    Unsigned,
    /// Signaturen, aber keine gültige eines vertrauten Signers.
    Invalid,
    /// Nicht prüfbar — mit Grund.
    NotChecked(String),
}

/// Was der minds-Graph über den Commit weiß — vom Aufrufer gefüllt
/// (Trailer, Store-Index, geprüfte Intent-Signaturen).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphFacts {
    /// Verknüpfte Sessions.
    pub sessions: usize,
    /// Davon ohne gültig signierten Intent-Anker.
    pub without_intent: usize,
    /// Nicht entscheidbar: Ein Trailer nennt eine Session, die hier nicht
    /// liegt, oder eine Intent-Signatur ließ sich nicht prüfen.
    pub unknown: bool,
}

/// Der Conformance-Bericht eines Commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Der bewertete Commit.
    pub commit: CommitId,
    /// Die Id des Solls im Commit — `None`, wenn es fehlt oder unlesbar ist.
    pub here: Option<ContentHash>,
    /// Das Soll, aus dem die Regeln stammen, und der Commit, in dem es lag.
    pub active: Option<(ContentHash, CommitId)>,
    /// Wer das aktive Soll signiert hat — `None`: Die Regeln sind ungeprüft.
    pub signer: Option<String>,
    /// Die Zeile des Solls selbst (`spec`).
    pub spec: RuleResult,
    /// Die Regeln in der Reihenfolge des Solls.
    pub rules: Vec<RuleResult>,
}

impl Report {
    /// Alle Zeilen, das Soll zuerst.
    pub fn rows(&self) -> impl Iterator<Item = &RuleResult> {
        std::iter::once(&self.spec).chain(&self.rules)
    }

    /// Anzahl PASS, DRIFT, UNKNOWN über alle Zeilen.
    pub fn counts(&self) -> (usize, usize, usize) {
        self.rows()
            .fold((0, 0, 0), |(p, d, u), row| match row.outcome {
                Outcome::Pass => (p + 1, d, u),
                Outcome::Drift => (p, d + 1, u),
                Outcome::Unknown => (p, d, u + 1),
            })
    }

    /// Das schlechteste Ergebnis: DRIFT vor UNKNOWN vor PASS.
    pub fn outcome(&self) -> Outcome {
        self.rows()
            .map(|row| row.outcome)
            .max()
            .unwrap_or(Outcome::Pass)
    }
}

/// Ein gültig signierter Umschlag und wer ihn signiert hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Der Umschlag (Soll, Pins, `at`).
    pub signed: SignedSpec,
    /// Der Principal, wie der Prüfer ihn nennt.
    pub signer: String,
}

/// Was der Aufrufer mitbringt: die Umschläge des Stores, ihre
/// Signaturprüfung und die Graph-Fakten — alles erst gefragt, wenn es
/// gebraucht wird.
pub struct Inputs<'a> {
    /// Alle Umschläge des Stores, **ungeprüft**. `Err` mit Grund, wenn der
    /// Store nicht lesbar ist.
    pub envelopes: &'a mut dyn FnMut() -> Result<Vec<SignedSpec>, String>,
    /// Wer einen Umschlag gültig signiert hat — `Ok(None)`: niemand; `Err`:
    /// nicht prüfbar (keine Signer, kein `ssh-keygen`, Prozessfehler,
    /// Prüf-Deckel). Gefragt nur für Umschläge, auf die es ankommt: deren
    /// Signatur-Commit in der Historie liegt (oder die das Soll im Commit
    /// tragen).
    pub verify: &'a mut dyn FnMut(&SignedSpec) -> Result<Option<String>, String>,
    /// Die Graph-Fakten des Commits; höchstens einmal gefragt.
    pub graph: &'a mut dyn FnMut() -> GraphFacts,
}

/// Wie viele Umschläge außerhalb der Historie (mit dem Soll des Commits)
/// höchstens geprüft werden — nur für eine genauere Meldung.
const MAX_ELSEWHERE_CHECKS: usize = 4;

/// Ob ein Umschlag für den Commit frisch ist.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fresh {
    Yes,
    /// Ein Commit seit `at` trägt einen Stand (Soll und gepinnte Dateien),
    /// den es vor `at` nie gab.
    Changed(CommitId),
    /// Nicht entschieden (Budget erschöpft, Objekt beschädigt).
    Undecided,
}

/// Bewertet `commit` gegen das signierte Soll. `None`, wenn im Commit kein
/// Soll liegt und in seiner Historie kein Umschlag gilt.
pub fn evaluate(repo: &Repo, commit: CommitId, inputs: Inputs<'_>) -> Option<Report> {
    let Inputs {
        envelopes,
        verify,
        graph,
    } = inputs;
    let mut ctx = Context {
        repo,
        commit,
        graph,
        facts: None,
    };
    let here = read_spec(repo, commit);
    let here_id = match &here {
        Found::Spec(spec) => Some(spec.id().clone()),
        _ => None,
    };
    // Nicht prüfbar: Die Regeln des Commits stehen da, als ungeprüft.
    let unchecked = |ctx: &mut Context<'_, '_>, reason: &str| {
        let observed = format!("signatures not checked ({})", sanitize(reason));
        match &here {
            Found::Spec(spec) => {
                let row = spec_row(
                    Outcome::Unknown,
                    "a signed spec".into(),
                    format!("spec {} — {observed}", short(spec.id())),
                );
                Some(ctx.report(here_id.clone(), Some((spec, commit)), None, None, row))
            }
            Found::Absent => None,
            Found::Unreadable(_) => {
                let row = spec_row(Outcome::Unknown, "a signed spec".into(), observed);
                Some(ctx.report(None, None, None, None, row))
            }
        }
    };
    let candidates = match envelopes() {
        Ok(list) => list,
        Err(reason) => return unchecked(&mut ctx, &reason),
    };
    // Nur Umschläge, deren Signatur-Commit hier liegt, kosten etwas — die
    // übrigen sind ohnehin anderswo signiert.
    if candidates
        .iter()
        .filter(|e| {
            e.at()
                .parse::<CommitId>()
                .is_ok_and(|at| repo.has_commit(at))
        })
        .count()
        > MAX_ENVELOPES
    {
        let row = spec_row(
            Outcome::Unknown,
            "a signed spec".into(),
            format!("more than {MAX_ENVELOPES} envelopes in the store"),
        );
        return Some(ctx.report(here_id, None, None, None, row));
    }

    // Die Historie einmal laufen; jeder Umschlag wird darin verortet — im
    // Prozess, vor jeder Signaturprüfung, und nur als Mitgliedschaft.
    let mut budget = Budget::new(MAX_ANCESTRY, ANCESTRY_BYTES);
    let history = repo.history(commit, &mut budget);
    let mine = |signed: &SignedSpec| here_id.as_ref() == Some(signed.id());
    let mut mine_in: Vec<(SignedSpec, CommitId)> = Vec::new();
    let mut others_in: Vec<(SignedSpec, CommitId)> = Vec::new();
    let mut mine_elsewhere: Vec<SignedSpec> = Vec::new();
    let mut undecided_local = false;
    for signed in candidates {
        // Ein `at`, das kein Commit dieses Repositorys sein kann (anderes
        // Objektformat): anderswo signiert.
        let Ok(at) = signed.at().parse::<CommitId>() else {
            if mine(&signed) {
                mine_elsewhere.push(signed);
            }
            continue;
        };
        match history.contains(at) {
            Some(true) if mine(&signed) => mine_in.push((signed, at)),
            Some(true) => others_in.push((signed, at)),
            Some(false) if mine(&signed) => mine_elsewhere.push(signed),
            Some(false) => {}
            None => undecided_local |= mine(&signed) || repo.has_commit(at),
        }
    }
    // Die Historie ist nicht vollständig, und es gibt Umschläge, auf die es
    // ankommen könnte: nicht entschieden — auch, wenn das Soll fehlt (ein
    // DRIFT verschwindet nicht still).
    if !history.complete() && undecided_local {
        let row = spec_row(
            Outcome::Unknown,
            "a signed spec".into(),
            format!(
                "the history cannot be walked completely (more than {MAX_ANCESTRY} commits, a shallow clone, or a damaged commit)"
            ),
        );
        let spec = match &here {
            Found::Spec(spec) => Some((spec, commit)),
            _ => None,
        };
        return Some(ctx.report(here_id, spec, None, None, row));
    }

    // Die Umschläge nach Generation ihres Signatur-Commits, jüngste zuerst —
    // geprüft wird nur, bis feststeht, was gilt.
    let generations = history.generations();
    let generation = |at: &CommitId| generations.get(at).copied().unwrap_or(0);
    mine_in.sort_by_key(|entry| std::cmp::Reverse(generation(&entry.1)));
    let mut checker = Freshness::default();
    let mut changed: Option<(CommitId, CommitId)> = None;
    let mut undecided = false;
    for (signed, at) in &mine_in {
        // Erst im Prozess: Trägt der Commit genau diesen Stand? Sonst kostet
        // der Umschlag keine Signaturprüfung (untergeschobene Umschläge mit
        // falschem Pin verbrauchen so keinen Prüf-Deckel).
        match checker.carries(repo, commit, signed, &mut budget) {
            Err(()) => {
                undecided = true;
                continue;
            }
            Ok(false) => {
                changed.get_or_insert((*at, commit));
                continue;
            }
            Ok(true) => {}
        }
        let signer = match verify(signed) {
            Ok(Some(signer)) => signer,
            Ok(None) => continue,
            Err(reason) => return unchecked(&mut ctx, &reason),
        };
        match checker.check(repo, signed, *at, commit, &history, &mut budget) {
            Fresh::Yes => {
                let Found::Spec(spec) = &here else {
                    unreachable!("nur Umschläge des Solls im Commit");
                };
                let row = spec_row(
                    Outcome::Pass,
                    format!("signed spec {}", short(spec.id())),
                    format!("signed by {}", sanitize(&signer)),
                );
                return Some(ctx.report(
                    here_id.clone(),
                    Some((spec, *at)),
                    Some(signer),
                    Some(signed),
                    row,
                ));
            }
            Fresh::Changed(node) => {
                changed.get_or_insert((*at, node));
            }
            Fresh::Undecided => undecided = true,
        }
    }
    if undecided {
        let row = spec_row(
            Outcome::Unknown,
            "a signed spec".into(),
            "the freshness of its signature cannot be decided (read budget exhausted or a damaged object)".into(),
        );
        let spec = match &here {
            Found::Spec(spec) => Some((spec, commit)),
            _ => None,
        };
        return Some(ctx.report(here_id, spec, None, None, row));
    }

    let observed = match &here {
        Found::Spec(spec) => {
            if let Some((at, node)) = changed {
                format!(
                    "spec {} was signed at {}, but {} carries an unsigned spec or pinned-file state since then — sign the current state again",
                    short(spec.id()),
                    short_commit(&at.to_string()),
                    short_commit(&node.to_string())
                )
            } else if mine_elsewhere
                .iter()
                .take(MAX_ELSEWHERE_CHECKS)
                .any(|signed| matches!(verify(signed), Ok(Some(_))))
            {
                format!(
                    "spec {} was signed outside this history — sign it again here (after a squash or rebase)",
                    short(spec.id())
                )
            } else {
                format!(
                    "spec {} carries no valid signature (fetch refs/minds/* if it was signed elsewhere)",
                    short(spec.id())
                )
            }
        }
        Found::Unreadable(reason) => format!("{SPEC_PATH} is unreadable ({reason})"),
        Found::Absent => format!("{SPEC_PATH} was removed"),
    };

    // Die Regeln: aus dem jüngsten gültig signierten Umschlag der Historie
    // (nach Generation; geprüft, bis einer gilt).
    let mut all_in: Vec<(SignedSpec, CommitId)> = mine_in.into_iter().chain(others_in).collect();
    all_in.sort_by(|a, b| {
        generation(&b.1)
            .cmp(&generation(&a.1))
            .then_with(|| a.0.id().hex().cmp(b.0.id().hex()))
    });
    let mut governing: Option<(SignedSpec, CommitId, String)> = None;
    let mut governing_unchecked: Option<String> = None;
    for (signed, at) in all_in {
        // Signiert wird HEAD: Ein echter Umschlag trägt an seinem `at` genau
        // seinen Stand. Was das nicht tut, kostet keine Signaturprüfung.
        if !matches!(checker.carries(repo, at, &signed, &mut budget), Ok(true)) {
            continue;
        }
        match verify(&signed) {
            Ok(Some(signer)) => {
                governing = Some((signed, at, signer));
                break;
            }
            Ok(None) => {}
            Err(reason) => {
                governing_unchecked = Some(reason);
                break;
            }
        }
    }
    match governing {
        Some((signed, at, signer)) => {
            let expected = format!(
                "signed spec {} (signed at {})",
                short(signed.id()),
                short_commit(&at.to_string())
            );
            let row = spec_row(Outcome::Drift, expected, observed);
            Some(ctx.report(
                here_id,
                Some((signed.spec(), at)),
                Some(signer),
                Some(&signed),
                row,
            ))
        }
        None => {
            if matches!(here, Found::Absent) && governing_unchecked.is_none() {
                // Kein gültiger Umschlag in dieser Historie: Hier gilt kein Soll.
                return None;
            }
            let expected = match governing_unchecked {
                Some(reason) => format!("a signed spec (not checked: {})", sanitize(&reason)),
                None => "a signed spec".into(),
            };
            let row = spec_row(Outcome::Drift, expected, observed);
            Some(ctx.report(here_id, None, None, None, row))
        }
    }
}

/// Ein Stand eines Commits für einen Umschlag: die Id des Solls dort und der
/// Pin jeder Datei, die der Umschlag pinnt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct State {
    spec: Option<ContentHash>,
    pins: Vec<Option<String>>,
}

/// Prüft die Frische eines Umschlags — mit Caches über Bäume, Soll-Blobs und
/// gepinnte Blobs (ein Blob wird einmal gelesen).
#[derive(Default)]
struct Freshness {
    trees: TreeCache,
    specs: HashMap<BlobId, Option<ContentHash>>,
    pins: HashMap<BlobId, String>,
}

impl Freshness {
    /// Frisch, wenn jeder Nachfahre von `at` in der Historie (der Commit
    /// eingeschlossen) genau den Stand des Umschlags trägt und jeder übrige
    /// Commit, der nicht schon vor `at` lag (Seitenzweige, die vor `at`
    /// abzweigen), diesen Stand oder einen, der vor `at` schon vorkam. Ein neuerer oder parallel entstandener Stand (ein
    /// anderes Soll, ein anderer Schema-Pin) kommt vor `at` nie vor —
    /// gleich, ob sein Umschlag im Store liegt.
    fn check(
        &mut self,
        repo: &Repo,
        signed: &SignedSpec,
        at: CommitId,
        commit: CommitId,
        history: &minds_git::ancestry::History,
        budget: &mut Budget,
    ) -> Fresh {
        let own = State {
            spec: Some(signed.id().clone()),
            pins: signed.pins().values().cloned().map(Some).collect(),
        };
        // Der Commit selbst trägt genau den signierten Stand — ein älterer
        // Stand (etwa ein zurückgedrehtes Schema) gilt nie unter einer
        // neueren Signatur.
        match self.state(repo, commit, signed, budget) {
            Err(()) => return Fresh::Undecided,
            Ok(state) if state != own => return Fresh::Changed(commit),
            Ok(_) => {}
        }
        let before = history.ancestors(at);
        let before_set: HashSet<CommitId> = before.iter().copied().collect();
        // Nachfahren von `at` tragen genau den signierten Stand — sonst wäre
        // ein Stand, der *vor* `at` schon galt und *nach* `at` wieder
        // auftaucht (strict → loose signiert → strict → loose), ein
        // Rückfall ohne Signatur. Nur Seitenzweige, die vor `at` abzweigen,
        // dürfen einen Stand von dort tragen.
        let after: HashSet<CommitId> = history.descendants(at);
        let mut foreign: HashSet<State> = HashSet::new();
        let mut first: Option<CommitId> = None;
        // Ab C in Breitensuche — deterministisch, und die Meldung nennt den
        // nächsten abweichenden Commit.
        for node in history.ancestors(commit) {
            if before_set.contains(&node) {
                continue;
            }
            let Ok(state) = self.state(repo, node, signed, budget) else {
                return Fresh::Undecided;
            };
            if state != own {
                if after.contains(&node) {
                    return Fresh::Changed(node);
                }
                first.get_or_insert(node);
                foreign.insert(state);
            }
        }
        if foreign.is_empty() {
            return Fresh::Yes;
        }
        // Kam jeder fremde Stand vor `at` schon vor? (Ein älterer Zweig, auf
        // dem noch die frühere Fassung lag.)
        for node in before {
            let Ok(state) = self.state(repo, node, signed, budget) else {
                return Fresh::Undecided;
            };
            foreign.remove(&state);
            if foreign.is_empty() {
                return Fresh::Yes;
            }
        }
        Fresh::Changed(first.unwrap_or(at))
    }

    /// Ob `node` genau den Stand des Umschlags trägt.
    fn carries(
        &mut self,
        repo: &Repo,
        node: CommitId,
        signed: &SignedSpec,
        budget: &mut Budget,
    ) -> Result<bool, ()> {
        let own = State {
            spec: Some(signed.id().clone()),
            pins: signed.pins().values().cloned().map(Some).collect(),
        };
        Ok(self.state(repo, node, signed, budget)? == own)
    }

    fn state(
        &mut self,
        repo: &Repo,
        node: CommitId,
        signed: &SignedSpec,
        budget: &mut Budget,
    ) -> Result<State, ()> {
        let spec = self.spec_at(repo, node, budget)?;
        let mut pins = Vec::with_capacity(signed.pins().len());
        for file in signed.pins().keys() {
            pins.push(self.pin_at(repo, node, file, budget)?);
        }
        Ok(State { spec, pins })
    }

    /// Die Id des Solls in `commit` — `None`: fehlt oder unlesbar.
    fn spec_at(
        &mut self,
        repo: &Repo,
        commit: CommitId,
        budget: &mut Budget,
    ) -> Result<Option<ContentHash>, ()> {
        let blob = match repo.entry_at(commit, SPEC_PATH, &mut self.trees, budget) {
            Ok(PathEntry::Blob(blob)) => blob,
            Ok(_) => return Ok(None),
            Err(_) => return Err(()),
        };
        if let Some(id) = self.specs.get(&blob) {
            return Ok(id.clone());
        }
        let bytes = read_charged(repo, blob, MAX_SPEC as u64, budget)?;
        let id = Spec::parse(&bytes).ok().map(|spec| spec.id().clone());
        self.specs.insert(blob, id.clone());
        Ok(id)
    }

    /// Der Pin der Datei `file` in `commit` — `None`: fehlt oder ist keine
    /// reguläre Datei.
    fn pin_at(
        &mut self,
        repo: &Repo,
        commit: CommitId,
        file: &str,
        budget: &mut Budget,
    ) -> Result<Option<String>, ()> {
        let blob = match repo.entry_at(commit, file, &mut self.trees, budget) {
            Ok(PathEntry::Blob(blob)) => blob,
            Ok(_) => return Ok(None),
            Err(_) => return Err(()),
        };
        if let Some(pin) = self.pins.get(&blob) {
            return Ok(Some(pin.clone()));
        }
        let bytes = read_charged(repo, blob, MAX_JSON, budget)?;
        let pin = pin_of(&bytes);
        self.pins.insert(blob, pin.clone());
        Ok(Some(pin))
    }
}

/// Liest einen Blob geprüft und belastet das Budget.
fn read_charged(repo: &Repo, blob: BlobId, max: u64, budget: &mut Budget) -> Result<Vec<u8>, ()> {
    let bytes = repo
        .read_blob_verified(blob, max.min(budget.bytes))
        .map_err(|_| ())?;
    budget.bytes = budget.bytes.saturating_sub(bytes.len() as u64);
    Ok(bytes)
}

/// Der Commit, die Graph-Fakten (einmal gefragt) — was die Regeln brauchen.
struct Context<'r, 'g> {
    repo: &'r Repo,
    commit: CommitId,
    graph: &'g mut dyn FnMut() -> GraphFacts,
    facts: Option<GraphFacts>,
}

impl Context<'_, '_> {
    fn facts(&mut self) -> GraphFacts {
        if self.facts.is_none() {
            self.facts = Some((self.graph)());
        }
        self.facts.clone().unwrap_or_default()
    }

    /// Bewertet die Regeln von `active` (falls gegeben) mit den Pins aus
    /// `signed` (`None`: ungeprüft — Schemas ohne Pin sind UNKNOWN).
    fn report(
        &mut self,
        here: Option<ContentHash>,
        active: Option<(&Spec, CommitId)>,
        signer: Option<String>,
        signed: Option<&SignedSpec>,
        spec: RuleResult,
    ) -> Report {
        let snapshot = Snapshot::new(self.repo, self.commit);
        let unverified = signer.is_none();
        let rules = active
            .map(|(spec, _)| {
                spec.rules()
                    .iter()
                    .map(|rule| {
                        let mut row = evaluate_rule(&snapshot, self, signed, rule);
                        if unverified {
                            row.expected = format!("{} (unverified spec)", row.expected);
                        }
                        row
                    })
                    .collect()
            })
            .unwrap_or_default();
        Report {
            commit: self.commit,
            here,
            active: active.map(|(spec, at)| (spec.id().clone(), at)),
            signer,
            spec,
            rules,
        }
    }
}

/// Was unter [`SPEC_PATH`] in einem Commit liegt.
enum Found {
    Absent,
    Unreadable(&'static str),
    Spec(Spec),
}

fn read_spec(repo: &Repo, commit: CommitId) -> Found {
    match repo.read_blob_bounded(commit, SPEC_PATH, MAX_SPEC as u64) {
        Ok(None) => Found::Absent,
        Ok(Some((_, bytes))) => match Spec::parse(&bytes) {
            Ok(spec) => Found::Spec(spec),
            Err(_) => Found::Unreadable("not a minds-spec-v1 document"),
        },
        Err(_) => Found::Unreadable("cannot be read from the object database"),
    }
}

fn spec_row(outcome: Outcome, expected: String, observed: String) -> RuleResult {
    RuleResult {
        id: "spec".into(),
        kind: "signature".into(),
        expected,
        observed,
        outcome,
    }
}

/// `b3-` und die ersten zwölf Hex-Zeichen.
fn short(id: &ContentHash) -> String {
    format!("b3-{}", &id.hex()[..12])
}

/// Die ersten zwölf Zeichen einer Commit-Id (schon geprüftes Hex).
fn short_commit(id: &str) -> String {
    id.chars().take(12).collect()
}

fn evaluate_rule(
    snapshot: &Snapshot<'_>,
    ctx: &mut Context<'_, '_>,
    signed: Option<&SignedSpec>,
    rule: &Rule,
) -> RuleResult {
    let (expected, judged) = match &rule.kind {
        RuleKind::NoDependency { krate, on } => (
            format!("{krate} does not depend on {}", on.join(", ")),
            manifest::no_dependency(snapshot, krate, on),
        ),
        RuleKind::NoImport { paths, import } => (
            format!(
                "no file under {} uses {}",
                paths.join(", "),
                import.join(", ")
            ),
            imports::no_import(snapshot, paths, import),
        ),
        RuleKind::JsonSchema {
            schema: path,
            files,
        } => (
            format!("{} match {path} as signed", files.join(", ")),
            schema::json_schema(
                snapshot,
                path,
                signed.and_then(|s| s.pins().get(path)).map(String::as_str),
                files,
            ),
        ),
        RuleKind::Graph { expect, paths } => {
            let scope = if paths.is_empty() {
                "every commit".to_owned()
            } else {
                format!("every commit changing {}", paths.join(", "))
            };
            let what = match expect {
                Expect::Session => "carries a minds session",
                Expect::Intent => "binds each agent session to a signed intent",
            };
            (
                format!("{scope} {what}"),
                graph_rule(snapshot, ctx, *expect, paths),
            )
        }
        RuleKind::Unsupported { reason, .. } => (
            "a rule this minds understands".to_owned(),
            (Outcome::Unknown, (*reason).to_owned()),
        ),
    };
    let (outcome, observed) = judged;
    RuleResult {
        id: rule.id.clone(),
        kind: rule.kind.name().to_owned(),
        expected: sanitize(&expected),
        observed: sanitize(&observed),
        outcome,
    }
}

/// Ein Urteil: Ergebnis und Beobachtung.
type Judged = (Outcome, String);

fn graph_rule(
    snapshot: &Snapshot<'_>,
    ctx: &mut Context<'_, '_>,
    expect: Expect,
    paths: &[String],
) -> Judged {
    if !paths.is_empty() {
        let changed = match snapshot.changed() {
            Ok(changed) => changed,
            Err(reason) => return (Outcome::Unknown, reason),
        };
        match any_match(paths, changed.iter().map(String::as_str)) {
            Err(reason) => return (Outcome::Unknown, reason),
            Ok(false) => {
                return (
                    Outcome::Pass,
                    "not triggered — nothing changed under these paths".into(),
                );
            }
            Ok(true) => {}
        }
    }
    let facts = ctx.facts();
    if facts.unknown {
        return (
            Outcome::Unknown,
            "a linked session is not in this store, or an intent signature could not be checked"
                .into(),
        );
    }
    match expect {
        Expect::Session if facts.sessions == 0 => (
            Outcome::Drift,
            "no minds session linked to this commit".into(),
        ),
        Expect::Session => (
            Outcome::Pass,
            format!("{} session(s) linked", facts.sessions),
        ),
        Expect::Intent if facts.sessions == 0 => (
            Outcome::Pass,
            "no agent session linked (combine with expect session to require one)".into(),
        ),
        Expect::Intent if facts.without_intent > 0 => (
            Outcome::Drift,
            format!(
                "{} of {} session(s) without a validly signed intent",
                facts.without_intent, facts.sessions
            ),
        ),
        Expect::Intent => (
            Outcome::Pass,
            format!(
                "{} session(s), each bound to a signed intent",
                facts.sessions
            ),
        ),
    }
}

/// Trifft einer der Globs einen der Pfade? `Err`, wenn ein Abgleich sein
/// Budget erschöpft — dann ist nichts entschieden.
fn any_match<'p>(globs: &[String], paths: impl Iterator<Item = &'p str>) -> Result<bool, String> {
    let globs: Vec<Glob> = globs.iter().map(|glob| Glob::new(glob)).collect();
    for path in paths {
        for glob in &globs {
            match glob.matches_checked(path) {
                Some(true) => return Ok(true),
                Some(false) => {}
                None => return Err("a glob is too costly to match".into()),
            }
        }
    }
    Ok(false)
}

/// Der Inhalt eines Commits, geprüft gelesen und gedeckelt: das Listing
/// einmal, Dateien über ihre Blob-Id, alle Lesezugriffe und Schema-Schritte
/// gegen gemeinsame Budgets.
pub(crate) struct Snapshot<'r> {
    repo: &'r Repo,
    commit: CommitId,
    listing: OnceCell<Result<Listing, String>>,
    parent: OnceCell<Result<Option<Listing>, String>>,
    spent: Cell<u64>,
    /// Die verbleibenden Schema-Prüfschritte ([`STEP_BUDGET`]).
    pub(crate) steps: Cell<u64>,
}

/// Ein Listing als Karte: Dateien und Links.
struct Listing {
    files: BTreeMap<String, BlobId>,
    links: Vec<String>,
}

impl<'r> Snapshot<'r> {
    fn new(repo: &'r Repo, commit: CommitId) -> Self {
        Self {
            repo,
            commit,
            listing: OnceCell::new(),
            parent: OnceCell::new(),
            spent: Cell::new(0),
            steps: Cell::new(STEP_BUDGET),
        }
    }

    fn listing(&self) -> Result<&Listing, String> {
        self.listing
            .get_or_init(|| list(self.repo, self.commit))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Alle Dateien des Commits, sortiert.
    pub(crate) fn files(&self) -> Result<&BTreeMap<String, BlobId>, String> {
        self.listing().map(|listing| &listing.files)
    }

    /// Die Symlinks und Submodule des Commits.
    pub(crate) fn links(&self) -> Result<&[String], String> {
        self.listing().map(|listing| listing.links.as_slice())
    }

    /// Die Dateien, die einer der Globs trifft — höchstens [`MAX_FILES`].
    /// Trifft ein Glob einen Symlink oder ein Submodul, ist nichts
    /// entschieden: deren Inhalt zeigt die Liste nicht.
    pub(crate) fn matching(&self, globs: &[String]) -> Result<Vec<(&str, BlobId)>, String> {
        let compiled: Vec<Glob> = globs.iter().map(|glob| Glob::new(glob)).collect();
        let hit = |path: &str| -> Result<bool, String> {
            for glob in &compiled {
                match glob.matches_checked(path) {
                    Some(true) => return Ok(true),
                    Some(false) => {}
                    None => return Err("a glob is too costly to match".into()),
                }
            }
            Ok(false)
        };
        for link in self.links()? {
            if hit(link)? {
                return Err(format!(
                    "{} is a symlink or submodule — its content is not checked",
                    sanitize_path(link)
                ));
            }
        }
        let mut out = Vec::new();
        for (path, blob) in self.files()? {
            if hit(path)? {
                if out.len() == MAX_FILES {
                    return Err(format!("more than {MAX_FILES} files match"));
                }
                out.push((path.as_str(), *blob));
            }
        }
        Ok(out)
    }

    /// Eine Datei des Commits nach Pfad — `Ok(None)`, wenn es sie nicht gibt.
    /// Ist der Pfad ein Symlink, ist nichts entschieden.
    pub(crate) fn read_path(&self, path: &str, max: u64) -> Result<Option<Vec<u8>>, String> {
        if self.links()?.iter().any(|link| link == path) {
            return Err(format!(
                "{} is a symlink — its content is not checked",
                sanitize_path(path)
            ));
        }
        match self.files()?.get(path) {
            None => Ok(None),
            Some(blob) => self.read(path, *blob, max).map(Some),
        }
    }

    /// Eine Datei über ihre Blob-Id, gegen Einzel- und Gesamtdeckel.
    pub(crate) fn read(&self, path: &str, blob: BlobId, max: u64) -> Result<Vec<u8>, String> {
        let left = READ_BUDGET.saturating_sub(self.spent.get());
        if left == 0 {
            return Err("read budget exhausted".into());
        }
        let bytes = self
            .repo
            .read_blob_verified(blob, max.min(left))
            .map_err(|_| {
                format!(
                    "{} cannot be read (too large, over the read budget or damaged)",
                    sanitize_path(path)
                )
            })?;
        self.spent
            .set(self.spent.get().saturating_add(bytes.len() as u64));
        Ok(bytes)
    }

    /// Die gegenüber dem ersten Elternteil geänderten Pfade (hinzugefügt,
    /// geändert, gelöscht; Links eingeschlossen). Beim Wurzel-Commit alle.
    pub(crate) fn changed(&self) -> Result<BTreeSet<String>, String> {
        let now = self.listing()?;
        let parent = self
            .parent
            .get_or_init(|| match self.repo.first_parent(self.commit) {
                Ok(Some(parent)) if self.repo.has_commit(parent) => {
                    list(self.repo, parent).map(Some)
                }
                Ok(Some(_)) => Err("the parent commit is not in this clone".into()),
                Ok(None) => Ok(None),
                Err(_) => Err("the commit cannot be read".into()),
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let mut changed: BTreeSet<String> = now.links.iter().cloned().collect();
        let Some(before) = parent else {
            changed.extend(now.files.keys().cloned());
            return Ok(changed);
        };
        for (path, blob) in &now.files {
            if before.files.get(path) != Some(blob) {
                changed.insert(path.clone());
            }
        }
        changed.extend(
            before
                .files
                .keys()
                .filter(|path| !now.files.contains_key(*path))
                .cloned(),
        );
        changed.extend(before.links.iter().cloned());
        Ok(changed)
    }
}

fn list(repo: &Repo, commit: CommitId) -> Result<Listing, String> {
    repo.list_blobs_verified(commit, MAX_ENTRIES, MAX_TREE_BYTES)
        .map(|VerifiedListing { blobs, links }| Listing {
            files: blobs.into_iter().collect(),
            links,
        })
        .map_err(|err| match err {
            minds_git::GitError::TooLarge { .. } => {
                "the commit is too large to list completely".to_owned()
            }
            _ => "the commit tree cannot be read (damaged or not hash-consistent)".to_owned(),
        })
}

/// Breitensuche über einen Abhängigkeitsgraphen: der Pfad von `from` zu
/// einem der Ziele, falls es einen gibt.
pub(crate) fn path_to(
    edges: &BTreeMap<String, BTreeSet<String>>,
    from: &str,
    targets: &[String],
) -> Option<Vec<String>> {
    let mut previous: HashMap<&str, &str> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::from([from]);
    let mut seen: BTreeSet<&str> = BTreeSet::from([from]);
    while let Some(node) = queue.pop_front() {
        if node != from && targets.iter().any(|target| target == node) {
            let mut path = vec![node.to_owned()];
            let mut at = node;
            while let Some(prev) = previous.get(at) {
                path.push((*prev).to_owned());
                at = prev;
            }
            path.reverse();
            return Some(path);
        }
        for next in edges.get(node).into_iter().flatten() {
            if seen.insert(next.as_str()) {
                previous.insert(next.as_str(), node);
                queue.push_back(next.as_str());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
