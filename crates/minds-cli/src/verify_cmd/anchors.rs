//! Erstsicht-Gegenzeichnungen in `minds verify` (EA-19): die Zeile
//! `anchored: pipeline #N, <at>` je Seal und — mit `--online` — der Abgleich
//! mit den MR-Notes, die `minds anchor` geschrieben hat.
//!
//! # Der Abgleich (`--online`)
//!
//! Die Note am Merge Request ist die Kopie, die der Agent nicht löschen
//! kann. Nennt ein **gültig signierter** Eintrag einer Note einen Seal der
//! Session, dessen Ref fehlt (oder keine gültige Gegenzeichnung trägt), oder
//! einen Seal, den es nicht mehr gibt, ist das ein Integritätsbefund
//! (`TAMPERED`). Ein Eintrag mit ungültiger Signatur zählt nie —
//! kommentieren darf am Merge Request auch, wer nicht signieren kann; ohne
//! vertrauenswürdige Signer bleibt es ein Hinweis.
//!
//! Instanz und Token wie bei der Versionsprüfung von Issue-Ankern
//! ([`crate::intent_issue`]); das Projekt aus `MINDS_GITLAB_PROJECT`, sonst
//! `CI_PROJECT_PATH` — nie aus `.git/config`, die der Agent schreiben kann.
//! Gefragt wird nach den Merge Requests, die den geprüften Commit enthalten.
//!
//! Drei Ausgänge, nie still:
//!
//! - **nicht geprüft** — es fehlt, was die Prüfung braucht (Projekt, Token,
//!   Commit; in GitLab-CI ist das ein Fehler), oder es gibt weder Anker im
//!   Klon noch gültig signierte Einträge. Das Verdikt bleibt.
//!
//! Ob ein fehlender Ref gelöscht oder bloß nicht geholt ist, entscheidet die
//! Zeit: Eine Note, die vor dem Start dieses CI-Jobs angelegt wurde
//! (`CI_JOB_STARTED_AT`), nennt einen Ref, der beim Fetch schon lag —
//! `--mirror` postet erst nach dem atomaren Push. Sonst zählt ein gültiger
//! Anker im Klon aus derselben oder einer späteren Pipeline. Was beides
//! nicht belegt, ist „womöglich veraltet": allein Exit 4, nie TAMPERED.
//! - **gescheitert** — GitLab nicht erreichbar, mehr Notes oder Merge
//!   Requests, als gelesen werden, ein Ref nicht lesbar: operativer Fehler
//!   (Exit 4). Wer `--online` verlangt, bekommt kein grünes Verdikt aus
//!   einer Prüfung, die nicht stattfand.
//! - **geprüft** — mit Befunden und Zählern.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use minds_core::ContentHash;
use minds_core::first_sight::is_project_path;
use minds_reader::first_sight::{AnchorSignature, FirstSightState, NoteCheck, NoteEntry};
use minds_store::ContextStore;

use super::witness_trust::WitnessTrust;

/// Die Variable mit dem Projekt für `--online`.
const PROJECT_ENV: &str = "MINDS_GITLAB_PROJECT";

/// Die Zeile unter einer Seal-Zeile — `None` ohne Ref. Eine ungeprüfte
/// Gegenzeichnung ist eine Behauptung dessen, der den Ref schrieb, und
/// heißt so.
pub(super) fn seal_line(state: &Result<FirstSightState, String>) -> Option<String> {
    let text = match state {
        Ok(FirstSightState::Absent) => return None,
        Ok(FirstSightState::Anchored { anchor, signature }) => {
            let when = format!(
                "pipeline #{}, {}",
                anchor.pipeline,
                crate::text::sanitize(&anchor.at)
            );
            match signature {
                AnchorSignature::Valid => format!("anchored: {when}"),
                AnchorSignature::NotChecked => {
                    format!("anchor claims {when} (signature not checked — verify with --signers)")
                }
                AnchorSignature::Invalid => {
                    format!("anchor not signed under minds-anchor ({when} claimed) — not counted")
                }
            }
        }
        Ok(FirstSightState::Unreadable) => {
            "anchor ref holds no readable countersignature — not counted".to_owned()
        }
        Err(_) => "anchor ref cannot be read — not counted".to_owned(),
    };
    Some(format!("               {text}"))
}

/// So viele Anker-Refs prüft die Frische-Rechnung höchstens — die Refs kann
/// jeder mit Push-Recht auf `refs/minds/*` vermehren.
const MAX_FRESHNESS_CHECKS: usize = 256;

/// So viele Commits einer Session fragt der Abgleich höchstens nach ihren
/// Merge Requests.
const MAX_SESSION_COMMITS: usize = 20;

/// Die Einträge der Notes eines Commits, samt dem, was über ihre Herkunft
/// sagbar ist.
#[derive(Clone)]
struct Fetched {
    entries: Vec<NoteEntry>,
    /// Notes mit Einträgen, die nach dem Anlegen bearbeitet wurden.
    edited: usize,
    /// Jeder Eintrag mit dem Zeitpunkt seiner Note: Seal, Projekt, Text,
    /// Signatur, `created_at`. Ungeprüft — belegen darf nur, was die
    /// Signaturprüfung besteht (ein alter Kommentar mit Müll-Signatur
    /// beweist nichts).
    timed: Vec<(ContentHash, String, String, String, Option<jiff::Timestamp>)>,
}

/// So viel Uhrenabweichung zwischen GitLab und den Runnern wird hingenommen,
/// wenn ein Anker im Klon belegen soll, dass er nach einer Note geholt
/// wurde.
const CLOCK_MARGIN: jiff::SignedDuration = jiff::SignedDuration::from_mins(10);

/// Die Prüfung der MR-Notes eines Laufs — je Commit einmal gefragt.
pub(crate) struct OnlineAnchors {
    /// `None`: ohne `--online`. Sonst Zugang und Projektpfad — oder warum
    /// es keinen gibt (`true`: verweigert, nicht bloß unkonfiguriert).
    access: Option<Result<(minds_gitlab::Project, String), (bool, String)>>,
    /// Die Einträge je Commit (oder warum sie fehlen).
    cache: RefCell<HashMap<String, Result<Fetched, String>>>,
    /// Schon gemeldete Befunde zu Seals außerhalb der Session: Sie stehen
    /// nur im ersten Block.
    reported: RefCell<BTreeSet<ContentHash>>,
    /// In GitLab-CI: der Start dieses Jobs (`CI_JOB_STARTED_AT`).
    job_started: Option<jiff::Timestamp>,
    /// Läuft der Abgleich in GitLab-CI? Dort ist eine Prüfung, die nicht
    /// zustande kommt, ein Fehler, nie „nicht geprüft".
    in_ci: bool,
    /// Eine Trigger-, API- oder Downstream-Pipeline: Ihre Variablen setzt der
    /// Aufrufer.
    foreign_trigger: bool,
    /// Der Ziel-Branch, in den „gemergt" zählt: in GitLab-CI
    /// `CI_DEFAULT_BRANCH`, sonst der Default-Branch laut GitLab.
    branch: Option<String>,
    /// Der späteste Zeitpunkt (`at`) unter den **gültig** gegengezeichneten
    /// Ankern im Klon — höchstens einmal je Lauf gerechnet, und nur, wenn er
    /// gebraucht wird.
    freshest: std::cell::OnceCell<Result<Option<jiff::Timestamp>, String>>,
    /// In GitLab-CI: ob der Token von `verify` nur lesen darf — einmal je
    /// Lauf gefragt.
    scopes: std::cell::OnceCell<Result<(), String>>,
    /// Ob ein Seal unverändert im Store liegt — je Seal einmal gelesen.
    present: RefCell<HashMap<ContentHash, Result<bool, String>>>,
}

/// Was die Prüfung für eine Session ergab.
pub(super) enum NotesState {
    /// Ohne `--online`.
    Offline,
    /// Nicht prüfbar (Konfiguration, nichts geholt) — der Grund. Das
    /// Verdikt bleibt.
    NotChecked(String),
    /// Gescheitert — der Grund, entschärft. Operativer Fehler.
    Failed(String),
    /// Geprüft — dazu die Zahl der gültig gegengezeichneten Seals der
    /// Session, die keine Note nennt (unterdrückt, anderes Projekt oder in
    /// einem Merge Request, der den Commit nicht enthält), und Befunde, die
    /// auch ein veralteter Klon erklären könnte (als Hinweise neben echten
    /// Befunden).
    Checked(NoteCheck, usize, Vec<String>),
}

impl OnlineAnchors {
    /// Wurde `--online` verlangt?
    pub(super) fn online(&self) -> bool {
        self.access.is_some()
    }

    pub(crate) fn new(online: bool, gitlab_url: Option<&str>, root: &std::path::Path) -> Self {
        Self::with(online, gitlab_url, root, &|name| std::env::var(name).ok())
    }

    /// Der Zugang. In GitLab-CI zählen allein `CI_SERVER_URL` und
    /// `CI_PROJECT_PATH`: `MINDS_GITLAB_URL`, `--gitlab-url` und
    /// `MINDS_GITLAB_PROJECT` kann dort ein Aufrufer setzen (Push-Option
    /// `ci.variable`) — und den Token an seinen Server lenken oder ein leeres
    /// Projekt fragen lassen. Weichen sie ab, wird verweigert (Exit 4).
    fn with(
        online: bool,
        gitlab_url: Option<&str>,
        root: &std::path::Path,
        var: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        let access = online.then(
            || -> Result<(minds_gitlab::Project, String), (bool, String)> {
                let unconfigured = |reason: &str| (false, reason.to_owned());
                let (base, path) = if var("GITLAB_CI").as_deref() == Some("true") {
                    let server = set("CI_SERVER_URL")
                        .ok_or_else(|| unconfigured("CI_SERVER_URL is not set"))?;
                    let path = set("CI_PROJECT_PATH")
                        .ok_or_else(|| unconfigured("CI_PROJECT_PATH is not set"))?;
                    let other_url = gitlab_url
                        .map(str::to_owned)
                        .or_else(|| set("MINDS_GITLAB_URL"))
                        .is_some_and(|url| {
                            url.trim_end_matches('/') != server.trim_end_matches('/')
                        });
                    let other_project = set(PROJECT_ENV).is_some_and(|project| project != path);
                    if other_url || other_project {
                        return Err((
                            true,
                            "in GitLab CI only CI_SERVER_URL and CI_PROJECT_PATH are used — \
                         --gitlab-url, MINDS_GITLAB_URL or MINDS_GITLAB_PROJECT differ"
                                .to_owned(),
                        ));
                    }
                    (
                        crate::intent_issue::base_url(Some(&server)).map_err(unconfigured)?,
                        path,
                    )
                } else {
                    let base = crate::intent_issue::base_url(gitlab_url).map_err(unconfigured)?;
                    let path = [PROJECT_ENV, "CI_PROJECT_PATH"]
                        .iter()
                        .find_map(|name| set(name))
                        .ok_or_else(|| {
                            unconfigured(
                                "no GitLab project: MINDS_GITLAB_PROJECT or CI_PROJECT_PATH",
                            )
                        })?;
                    (base, path)
                };
                // Ein Pfad mit Gruppe: Eine numerische Projekt-Id fände nie
                // einen Eintrag (die Gegenzeichnung nennt den Pfad).
                if !is_project_path(&path) || !path.contains('/') {
                    return Err(unconfigured(
                        "the GitLab project is not a group/project path",
                    ));
                }
                let project =
                    crate::intent_issue::project(&base, &path).map_err(|err| match err {
                        minds_gitlab::TokenError::Missing(_) => {
                            unconfigured("MINDS_GITLAB_TOKEN is not set")
                        }
                        minds_gitlab::TokenError::Malformed(_) => {
                            unconfigured("MINDS_GITLAB_TOKEN is not a valid token")
                        }
                    })?;
                // Der Token geht über stdin an curl — an ein curl außerhalb des
                // Checkouts.
                let curl = set("PATH")
                    .as_deref()
                    .and_then(|path| crate::replay_cmd::find_program("curl", path, root))
                    .ok_or_else(|| {
                        unconfigured(
                            "curl was not found under an absolute PATH entry outside the checkout",
                        )
                    })?;
                Ok((project.with_curl(curl), path))
            },
        );
        let in_ci = var("GITLAB_CI").as_deref() == Some("true");
        Self {
            access,
            cache: RefCell::new(HashMap::new()),
            reported: RefCell::new(BTreeSet::new()),
            job_started: in_ci
                .then(|| set("CI_JOB_STARTED_AT"))
                .flatten()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok()),
            in_ci,
            foreign_trigger: matches!(
                var("CI_PIPELINE_SOURCE").as_deref(),
                Some("trigger" | "api" | "pipeline" | "parent_pipeline")
            ),
            branch: in_ci.then(|| set("CI_DEFAULT_BRANCH")).flatten(),
            freshest: std::cell::OnceCell::new(),
            scopes: std::cell::OnceCell::new(),
            present: RefCell::new(HashMap::new()),
        }
    }

    /// Gleicht die Notes der Merge Requests von `commits` mit den Refs ab:
    /// für die `seals` der Session und für jeden Seal, den eine Note eines
    /// gemergten Merge Requests nennt. `commits` sind der geprüfte Commit und
    /// jeder Commit, dessen Trailer die Session nennt — eine Session über
    /// mehrere Merge Requests hat ihre erste Sicht in der Note des ersten;
    /// wer den Ref löscht und im zweiten neu zeichnen lässt, fiele sonst nur
    /// beim ersten Commit auf. Höchstens [`MAX_SESSION_COMMITS`].
    pub(super) fn check(
        &self,
        store: &dyn ContextStore,
        trust: &WitnessTrust<'_>,
        commits: &[String],
        seals: &[ContentHash],
    ) -> NotesState {
        // In GitLab-CI ist eine verlangte Prüfung, die nicht zustande kommt,
        // ein Fehler — sonst liefe ein Gate mit `--online` still grün.
        let not_checked = |reason: String| {
            if self.in_ci {
                NotesState::Failed(reason)
            } else {
                NotesState::NotChecked(reason)
            }
        };
        let (project, path) = match &self.access {
            None => return NotesState::Offline,
            Some(Err((true, reason))) => return NotesState::Failed(reason.clone()),
            Some(Err((false, reason))) => return not_checked(reason.clone()),
            Some(Ok(access)) => access,
        };
        if commits.is_empty() {
            return not_checked("no commit — verify a revision or pass --commit".into());
        }
        if commits.len() > MAX_SESSION_COMMITS {
            return NotesState::Failed(format!(
                "more than {MAX_SESSION_COMMITS} commits name this session — not checked completely"
            ));
        }
        // Ausgelöst von außen (Trigger, API, Downstream): Die Variablen
        // dieses Jobs — auch `CI_JOB_STARTED_AT` — kann der Aufrufer setzen.
        if self.in_ci && self.foreign_trigger {
            return NotesState::Failed(
                "refusing to judge anchor notes in a trigger, API or downstream pipeline — its \
                 variables are not reviewed"
                    .into(),
            );
        }
        // In GitLab-CI sieht der Agent in Merge-Request-Pipelines den Token
        // von `verify`. Darf er schreiben (`api`), kann er die Notes des Bots
        // löschen — dann gibt es nichts mehr abzugleichen.
        if self.in_ci
            && let Err(err) = self.scopes.get_or_init(|| {
                let scopes = project
                    .token_scopes()
                    .map_err(|err| crate::text::sanitize(&err))?;
                if scopes.iter().any(|scope| scope == "api") {
                    return Err(format!(
                        "{} has the api scope — in CI, verify --online needs a read_api token \
                         (an api token could delete the anchor notes)",
                        crate::intent_issue::TOKEN_ENV
                    ));
                }
                Ok(())
            })
        {
            return NotesState::Failed(err.clone());
        }
        let mut fetched = Fetched {
            entries: Vec::new(),
            edited: 0,
            timed: Vec::new(),
        };
        for commit in commits {
            let one = self
                .cache
                .borrow_mut()
                .entry(commit.clone())
                .or_insert_with(|| self.fetch(project, commit))
                .clone();
            match one {
                Ok(one) => {
                    fetched.entries.extend(one.entries);
                    fetched.edited += one.edited;
                    fetched.timed.extend(one.timed);
                }
                Err(err) => return NotesState::Failed(err),
            }
        }
        let no_refs = match store.list_first_sights() {
            Ok(anchors) => anchors.is_empty(),
            Err(err) => return NotesState::Failed(crate::text::sanitize(&err.to_string())),
        };
        // Ein Lesefehler ist weder „da" noch „weg" — die Prüfung scheitert,
        // statt zu urteilen.
        let unreadable = RefCell::new(None::<String>);
        // „Da" heißt: Der Seal liegt und sein Text hasht auf seine Id — ein
        // Ref, der auf Fremdes zeigt, ist so weg wie ein gelöschter.
        let present = |seal: &ContentHash| {
            let known = self
                .present
                .borrow_mut()
                .entry(seal.clone())
                .or_insert_with(|| match store.seal_text(seal) {
                    Ok(Some(_)) => Ok(true),
                    Ok(None) | Err(minds_store::StoreError::SealMismatch { .. }) => Ok(false),
                    Err(err) => Err(err.to_string()),
                })
                .clone();
            known.unwrap_or_else(|err| {
                unreadable.borrow_mut().get_or_insert(err);
                true
            })
        };
        let session: BTreeSet<ContentHash> = seals.iter().cloned().collect();
        let check = minds_reader::first_sight::note_findings(
            &fetched.entries,
            &session,
            path,
            &present,
            &|seal| match trust.first_sight_checked(seal) {
                Ok(state) => state,
                Err(err) => {
                    unreadable.borrow_mut().get_or_insert(err);
                    FirstSightState::Unreadable
                }
            },
            &|signature| trust.anchor_key(signature),
            &|text, signature| trust.anchor_check(text, signature),
        );
        if let Some(err) = unreadable.into_inner().or_else(|| trust.anchor_error()) {
            return NotesState::Failed(crate::text::sanitize(&err));
        }
        // Was schon feststeht, verschwindet nicht hinter einem Fehler: Die
        // Befunde stehen in der Meldung.
        let established = |reason: String| {
            let texts: Vec<String> = check
                .findings
                .iter()
                .filter(|finding| finding.integrity())
                .map(|finding| finding.text())
                .collect();
            if texts.is_empty() {
                NotesState::Failed(reason)
            } else {
                NotesState::Failed(format!("{reason}; already found: {}", texts.join("; ")))
            }
        };
        // Eine Anker-Note schreibt `--mirror` einmal und nie wieder: Eine
        // bearbeitete hat jemand anders angefasst.
        if fetched.edited > 0 {
            return established(format!(
                "{} merge request note(s) with anchor entries were edited after posting — \
                 check their history in GitLab",
                fetched.edited
            ));
        }
        // Eine Prüfgrenze ließ womöglich gültige Einträge ungeprüft — nie
        // „nichts gefunden".
        if check.exhausted {
            return established(
                "more MR note entries than are checked — not checked completely".into(),
            );
        }
        // Einträge, die etwas hätten sagen können, aber keiner ließ sich als
        // gültig belegen (keine Signer für `minds-anchor`, falsche
        // Signer-Datei, nur Fälschungen): Der Abgleich sagt nichts — nie
        // „geprüft".
        if check.unvalidated > 0 {
            return established(format!(
                "{} MR note entr{} could not be validated — pass --signers with a principal \
                 restricted to minds-anchor (after a key rotation: both keys)",
                check.unvalidated,
                if check.unvalidated == 1 { "y" } else { "ies" }
            ));
        }
        // Nur Einträge eines anderen Projekts: falsch eingestellt (oder
        // umbenannt) — sonst hieße es still „0 geprüft".
        if check.noted.is_empty() && check.other_project > 0 {
            return NotesState::Failed(
                "the MR notes carry anchors only for another project — check the GitLab \
                 project (renamed or transferred?)"
                    .into(),
            );
        }
        if no_refs && check.signed == 0 {
            return not_checked(
                "no refs/minds/anchors/first-sight in this clone — fetch refs/minds/* first".into(),
            );
        }
        // Ein fehlender Ref oder Seal kann zwei Gründe haben: gelöscht —
        // oder dieser Klon ist älter als die Note. Sicher gelöscht ist er,
        // wenn die früheste **gültig signierte** Note-Fassung des Eintrags
        // älter ist als dieser Job (in CI), oder wenn ein gültiger Anker im
        // Klon deutlich nach ihr gezeichnet wurde (dann wurde nach ihr
        // geholt). Pipeline-Nummern taugen dafür nicht — sie ordnen das
        // Anlegen, nicht den Push. Alles andere ist „womöglich veraltet":
        // allein nie TAMPERED, nie grün (Exit 4, mit den Befunden); neben
        // echten Befunden ein Hinweis.
        use minds_reader::first_sight::NoteFinding;
        let freshest = || -> Result<Option<jiff::Timestamp>, String> {
            self.freshest
                .get_or_init(|| self.freshest_anchor(store, trust))
                .clone()
        };
        let mut stale = BTreeSet::new();
        for finding in &check.findings {
            let (NoteFinding::RefMissing { seal, .. } | NoteFinding::SealMissing { seal, .. }) =
                finding
            else {
                continue;
            };
            // Die früheste Note-Fassung eines gültig signierten Eintrags
            // dieses Projekts — höchstens einige je Seal geprüft.
            let noted_at = fetched
                .timed
                .iter()
                .filter(|(noted, project, _, signature, at)| {
                    noted == seal && project == path && at.is_some() && trust.anchor_key(signature)
                })
                .take(minds_reader::first_sight::MAX_CHECKS_PER_SEAL)
                .filter(|(_, _, text, signature, _)| {
                    trust.anchor_check(text, signature) == Some(true)
                })
                .filter_map(|(_, _, _, _, at)| *at)
                .min();
            let Some(noted_at) = noted_at else {
                stale.insert(seal.clone());
                continue;
            };
            // Kein einziger Anker im Klon: eher ein unvollständiger Fetch als
            // eine Löschung — nie TAMPERED (Exit 4, die Befunde stehen da).
            if no_refs {
                stale.insert(seal.clone());
                continue;
            }
            if self.job_started.is_some_and(|started| noted_at < started) {
                continue;
            }
            match freshest() {
                Ok(Some(freshest)) if freshest >= noted_at + CLOCK_MARGIN => {}
                Ok(_) => {
                    stale.insert(seal.clone());
                }
                Err(err) => return NotesState::Failed(err),
            }
        }
        let (stale_findings, mut real): (Vec<NoteFinding>, Vec<NoteFinding>) = check
            .findings
            .iter()
            .cloned()
            .partition(|finding| stale.contains(finding.seal()));
        let stale_texts: Vec<String> = stale_findings.iter().map(NoteFinding::text).collect();
        if !stale_texts.is_empty() && !real.iter().any(NoteFinding::integrity) {
            return NotesState::Failed(format!(
                "the anchor refs in this clone may be older than the MR note — fetch \
                 refs/minds/* and rerun; if they were fetched, they were deleted: {}",
                stale_texts.join("; ")
            ));
        }
        // Gültig gegengezeichnete Seals der Session, die keine Note nennt.
        let unnoted = seals
            .iter()
            .filter(|seal| !check.noted.contains(*seal) && trust.first_sight(seal).valid())
            .count();
        let mut reported = self.reported.borrow_mut();
        real.retain(|finding| {
            seals.contains(finding.seal()) || reported.insert(finding.seal().clone())
        });
        let mut check = check;
        check.findings = real;
        NotesState::Checked(check, unnoted, stale_texts)
    }

    /// Liest die Einträge der Notes von `commit` — mit Herkunft.
    fn fetch(&self, project: &minds_gitlab::Project, commit: &str) -> Result<Fetched, String> {
        let sourced = project
            .anchor_entries_for_commit(commit, self.branch.as_deref())
            .map_err(|err| crate::text::sanitize(&err))?;
        let mut fetched = Fetched {
            entries: Vec::with_capacity(sourced.len()),
            edited: 0,
            timed: Vec::new(),
        };
        for item in sourced {
            if item.edited {
                fetched.edited += 1;
            }
            let created = item
                .created_at
                .as_deref()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok());
            if let Ok(anchor) = minds_core::first_sight::FirstSight::parse(&item.entry.text) {
                fetched.timed.push((
                    anchor.seal,
                    anchor.project,
                    item.entry.text.clone(),
                    item.entry.signature.clone(),
                    created,
                ));
            }
            fetched.entries.push(NoteEntry {
                text: item.entry.text,
                signature: item.entry.signature,
                merged: item.merged,
            });
        }
        Ok(fetched)
    }

    /// Der späteste Zeitpunkt unter den gültig gegengezeichneten Ankern im
    /// Klon: die Refs nach behauptetem `at` absteigend, geprüft bis zum
    /// ersten gültigen — höchstens [`MAX_FRESHNESS_CHECKS`]. Ein
    /// untergeschobener Ref mit Zeitpunkt in ferner Zukunft zählt nicht.
    fn freshest_anchor(
        &self,
        store: &dyn ContextStore,
        trust: &WitnessTrust<'_>,
    ) -> Result<Option<jiff::Timestamp>, String> {
        let mut claims: Vec<(jiff::Timestamp, ContentHash)> = Vec::new();
        for seal in store
            .list_first_sights()
            .map_err(|err| crate::text::sanitize(&err.to_string()))?
        {
            if let Ok(minds_store::FirstSightRef::Present {
                text: Some(text), ..
            }) = store.first_sight(&seal)
                && let Ok(anchor) = minds_core::first_sight::FirstSight::parse(&text)
                && anchor.seal == seal
                && let Ok(at) = anchor.at.parse::<jiff::Timestamp>()
            {
                claims.push((at, seal));
            }
        }
        claims.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (checked, (at, seal)) in claims.iter().enumerate() {
            if checked == MAX_FRESHNESS_CHECKS {
                return Err(format!(
                    "more than {MAX_FRESHNESS_CHECKS} anchor refs without a valid countersignature \
                     ahead of the newest valid one — not checked completely"
                ));
            }
            if trust.first_sight(seal).valid() {
                return Ok(Some(*at));
            }
        }
        Ok(None)
    }
}

/// Die `Anchor notes`-Zeile.
pub(super) fn notes_line(state: &NotesState) -> Option<String> {
    let text = match state {
        NotesState::Offline => return None,
        NotesState::NotChecked(reason) => format!("not checked ({reason})"),
        NotesState::Failed(reason) => format!("check failed ({reason})"),
        NotesState::Checked(check, unnoted, _) => {
            let mut text = format!(
                "checked ({} MR note entr{} for this session",
                check.mirrored,
                if check.mirrored == 1 { "y" } else { "ies" }
            );
            if check.forged > 0 {
                text.push_str(&format!(
                    ", {} not signed under minds-anchor — ignored",
                    check.forged
                ));
            }
            if check.other_project > 0 {
                text.push_str(&format!(
                    ", {} for another project — ignored",
                    check.other_project
                ));
            }
            if *unnoted > 0 {
                text.push_str(&format!(
                    ", {unnoted} anchored seal(s) in no MR note of this commit"
                ));
            }
            text.push(')');
            text
        }
    };
    Some(format!("Anchor notes   {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::first_sight::FirstSight;

    fn anchored(signature: AnchorSignature) -> Result<FirstSightState, String> {
        Ok(FirstSightState::Anchored {
            anchor: FirstSight {
                seal: ContentHash::from_bytes([1; 32]),
                project: "g/r".into(),
                pipeline: 4711,
                at: "2026-10-08T12:34:56Z".into(),
            },
            signature,
        })
    }

    /// Golden: die Zeile unter einem Seal, je Lage.
    #[test]
    fn verify_anchor_line_golden() {
        assert_eq!(seal_line(&Ok(FirstSightState::Absent)), None);
        assert_eq!(
            seal_line(&anchored(AnchorSignature::Valid)).unwrap(),
            "               anchored: pipeline #4711, 2026-10-08T12:34:56Z"
        );
        assert_eq!(
            seal_line(&anchored(AnchorSignature::NotChecked)).unwrap(),
            "               anchor claims pipeline #4711, 2026-10-08T12:34:56Z (signature not checked — verify with --signers)"
        );
        assert_eq!(
            seal_line(&anchored(AnchorSignature::Invalid)).unwrap(),
            "               anchor not signed under minds-anchor (pipeline #4711, 2026-10-08T12:34:56Z claimed) — not counted"
        );
        assert_eq!(
            seal_line(&Ok(FirstSightState::Unreadable)).unwrap(),
            "               anchor ref holds no readable countersignature — not counted"
        );
        assert_eq!(
            seal_line(&Err("io".into())).unwrap(),
            "               anchor ref cannot be read — not counted"
        );
    }

    /// Die Fehler-Art des Zugangs, wenn es keinen gibt.
    fn refusal(check: &OnlineAnchors) -> Option<(bool, String)> {
        match &check.access {
            Some(Err(err)) => Some(err.clone()),
            _ => None,
        }
    }

    /// Ohne Projekt oder mit einer unzulässigen Instanz geht keine Anfrage
    /// hinaus — die Prüfung sagt, warum nicht (unkonfiguriert: `false`).
    #[test]
    fn the_online_check_names_why_it_cannot_run() {
        let root = std::path::Path::new("/nonexistent");
        let https = Some("https://gitlab.example.com");
        let none = OnlineAnchors::with(true, https, root, &|_| None);
        assert_eq!(
            refusal(&none),
            Some((
                false,
                "no GitLab project: MINDS_GITLAB_PROJECT or CI_PROJECT_PATH".into()
            ))
        );
        for bad in ["a/../b", "12345"] {
            let check = OnlineAnchors::with(true, https, root, &|name| {
                (name == "CI_PROJECT_PATH").then(|| bad.to_owned())
            });
            assert_eq!(
                refusal(&check),
                Some((
                    false,
                    "the GitLab project is not a group/project path".into()
                )),
                "{bad}"
            );
        }
        let http = OnlineAnchors::with(true, Some("http://gitlab.example.com"), root, &|_| None);
        assert!(
            refusal(&http).is_some_and(|(fatal, reason)| !fatal && reason.contains("https://"))
        );
        let offline = OnlineAnchors::with(false, None, root, &|_| None);
        assert!(offline.access.is_none());
    }

    /// Security-Review EA-19: In GitLab-CI zählen allein `CI_SERVER_URL` und
    /// `CI_PROJECT_PATH` — eine abweichende Instanz oder ein anderes Projekt
    /// (Push-Option `ci.variable`) wird verweigert, nie still befolgt.
    #[test]
    fn in_ci_only_the_ci_instance_and_project_are_used() {
        let root = std::path::Path::new("/nonexistent");
        let ci = |extra: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                [
                    ("GITLAB_CI", "true"),
                    ("CI_SERVER_URL", "https://gitlab.example.com"),
                    ("CI_PROJECT_PATH", "group/repo"),
                ]
                .iter()
                .chain(extra.iter())
                .rev()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
            }
        };
        for extra in [
            &[("MINDS_GITLAB_URL", "https://evil.example")][..],
            &[("MINDS_GITLAB_PROJECT", "other/empty")][..],
        ] {
            let check = OnlineAnchors::with(true, None, root, &ci(extra));
            assert!(
                refusal(&check).is_some_and(|(fatal, reason)| fatal
                    && reason.starts_with("in GitLab CI only CI_SERVER_URL and CI_PROJECT_PATH")),
                "{extra:?}"
            );
        }
        let flag = OnlineAnchors::with(true, Some("https://evil.example"), root, &ci(&[]));
        assert!(refusal(&flag).is_some_and(|(fatal, _)| fatal));
        // Dieselbe Instanz (mit Schrägstrich) ist keine Abweichung.
        let same = OnlineAnchors::with(true, Some("https://gitlab.example.com/"), root, &ci(&[]));
        assert!(refusal(&same).is_none_or(|(fatal, _)| !fatal));
    }

    #[test]
    fn the_notes_line_counts_what_it_saw() {
        assert_eq!(notes_line(&NotesState::Offline), None);
        assert_eq!(
            notes_line(&NotesState::NotChecked(
                "MINDS_GITLAB_TOKEN is not set".into()
            ))
            .unwrap(),
            "Anchor notes   not checked (MINDS_GITLAB_TOKEN is not set)"
        );
        assert_eq!(
            notes_line(&NotesState::Failed("HTTP 500".into())).unwrap(),
            "Anchor notes   check failed (HTTP 500)"
        );
        assert_eq!(
            notes_line(&NotesState::Checked(
                NoteCheck {
                    mirrored: 1,
                    forged: 2,
                    other_project: 3,
                    ..NoteCheck::default()
                },
                4,
                Vec::new()
            ))
            .unwrap(),
            "Anchor notes   checked (1 MR note entry for this session, 2 not signed under minds-anchor — ignored, 3 for another project — ignored, 4 anchored seal(s) in no MR note of this commit)"
        );
    }
}
