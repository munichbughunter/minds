//! `minds anchor` (EA-19): CI zeichnet jeden Seal, den es zum ersten Mal
//! sieht, gegen — „existierte spätestens in Pipeline #N" — mit einem
//! Schlüssel, den der Agent nie sieht, und spiegelt das in den Merge
//! Request, wo der Agent es nicht löschen kann.
//!
//! # Was gegengezeichnet wird
//!
//! Die Seals der Sessions, die die Commits **dieser Pipeline** per Trailer
//! nennen — `CI_COMMIT_BEFORE_SHA..HEAD`, sonst der erste Elternteil von HEAD
//! (der Merge eines Merge Requests bringt dessen Commits mit). Nicht jeder
//! Seal im Store: Die Note geht an die Merge Requests dieses Commits, und
//! `verify --online` liest genau dort — ein Seal eines anderen, noch offenen
//! Merge Requests stünde in der falschen Note und fiele dort nie auf.
//!
//! Je Seal ohne Gegenzeichnung: der Text `minds-anchor-v1`
//! ([`minds_core::first_sight::FirstSight`]) mit Projekt, Pipeline und der
//! Uhrzeit der CI, signiert unter `minds-anchor`, abgelegt unter
//! `refs/minds/anchors/first-sight/<64 hex>` (Baum `anchor`, `anchor.sig`).
//! Liegt der Ref schon, geschieht nichts — die erste Sicht gewinnt. Trägt er
//! keine gültige Gegenzeichnung **dieses** Schlüssels (geprüft mit
//! `ssh-keygen -Y verify`), wird er trotzdem nie überschrieben, aber laut
//! gemeldet. Ein Seal, dessen Text nicht auf seine Id hasht, wird nicht
//! gegengezeichnet.
//!
//! # Zwei Schritte: signieren, pushen, spiegeln
//!
//! `minds anchor` signiert und legt ab; der Job pusht `refs/minds/*`;
//! danach postet `minds anchor --mirror` die Note — erst wenn die Refs
//! draußen sind, sonst meldete jedes `verify --online` dazwischen einen
//! fehlenden Ref. Ein Fehler beim Spiegeln hält so auch den Push nicht auf.
//! Die Note (idempotent über den Inhalt eigener Notes) trägt jede gültige
//! Gegenzeichnung dieses Schlüssels zu einem Seal dieses Pushs — aus
//! welcher Pipeline auch immer — samt Signatur und geht an die in diesen
//! Branch gemergten Merge Requests des Commits ([`minds_gitlab::anchor`]).
//! Die Instanz ist allein `CI_SERVER_URL`. Der Push muss atomar sein
//! (`git push --atomic`): Nennt die Note einen Ref, der nie ankam, meldet
//! `verify --online` ihn als fehlend. Und `[skip ci]` gehört auf dem
//! geschützten Branch per `workflow:rules` gesperrt — ein Merge ohne
//! Pipeline bliebe sonst ungezeichnet (der nächste Bereich beginnt danach).
//!
//! # Wo signiert wird (Abweichung von der Spec)
//!
//! Die Spec sah die Note in der Merge-Request-Pipeline vor
//! (`CI_MERGE_REQUEST_IID`). Dort stammt `.gitlab-ci.yml` aus dem Branch des
//! Merge Requests — den der Agent pusht. Ein Schlüssel, der dort erreichbar
//! ist, ist dem Agenten erreichbar; ein geschützter ist dort gar nicht da.
//! Deshalb dieselben positiven Bedingungen wie beim signierenden
//! `minds replay` ([`crate::replay_cmd`]): GitLab-CI, **geschützter** Ref
//! (`CI_COMMIT_REF_PROTECTED=true`), Auslöser `push`, ausgecheckt ist genau
//! `CI_COMMIT_SHA`, keine Merge-Request-, Trigger- oder API-Pipeline.
//! Signiert wird nur mit der privaten Schlüsseldatei aus
//! `MINDS_ANCHOR_KEY_FILE`, nie über einen ssh-agent. Fehlt etwas davon,
//! endet der Lauf mit 4, bevor etwas geschrieben wird.
//!
//! # Der Job
//!
//! ```yaml
//! minds-anchor:
//!   resource_group: minds-anchor        # process_mode: oldest_first
//!   variables: { GIT_DEPTH: "0" }
//!   rules: [{ if: '$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH && $CI_PIPELINE_SOURCE == "push"' }]
//!   script:
//!     - git fetch origin '+refs/minds/*:refs/minds/*'
//!     - minds anchor || rc=$?           # gezeichnet ist, was ging
//!     - git push --atomic origin 'refs/minds/anchors/*'
//!     - minds anchor --mirror
//!     - exit ${rc:-0}
//! ```
//!
//! Nacheinander (`resource_group`, älteste zuerst), sonst überholt eine
//! spätere Pipeline eine frühere; gepusht wird auch nach einem belegten Ref
//! (Exit 4), sonst blieben alle übrigen Seals des Pushs ungezeichnet. Der Job
//! läuft nicht `interruptible` und hängt an keinem Test-Job: Ein Merge, dessen
//! Pipeline nicht bis hierher kam, wäre sonst nie gezeichnet. `verify
//! --online` bekommt einen Token mit `read_api`, nie `api` — ein Token, den
//! der Agent im Merge Request sieht, darf keine Note löschen können.
//!
//! Vor `verify --online` holt jeder Job `refs/minds/*` ausdrücklich
//! (`git fetch origin '+refs/minds/*:refs/minds/*'`); läuft er in derselben
//! Pipeline wie der Anker-Job, dann mit `needs: [minds-anchor]`.
//!
//! **Derselbe Schlüssel signiert Replay-Records** (EA-18b, Namespace
//! `minds-anchor`). Der signierende Replay führt gemergten Code aus, der den
//! Schlüssel lesen kann — damit ließen sich auch Gegenzeichnungen fälschen.
//! Die Grenze ist dieselbe wie dort: Signiert wird nur über reviewtem Code
//! (geschützter Branch, Merge nach Review). Eine eigene Namespace für
//! Erstsicht-Gegenzeichnungen trennte beides — eine Spec-Änderung.
//!
//! Die eigentliche Grenze bleibt, wem der Prüfer glaubt: In
//! `allowed_signers` steht für `minds-anchor` nur der CI-Schlüssel, auf
//! genau diesen Namespace beschränkt — **ein Schlüssel je Projekt** (die
//! Gegenzeichnung bindet das Projekt nur als Text; ein geteilter Schlüssel
//! ließe eine Sicht aus einem anderen Projekt hier zählen). Er liegt als
//! **geschützte** Variable, auf die Umgebung des Anker-Jobs beschränkt:
//! Die übrigen Jobs der geschützten Pipeline führen gemergten Code aus
//! (Tests, `build.rs`). Auf den signierenden Branch gelangt Code nur per
//! Merge (Push-Optionen wie `ci.variable` sind keine reviewte Eingabe);
//! Tag-Pipelines signieren nie.
//!
//! Ein Ref, den dieser Schlüssel nicht gültig signiert hat, verhindert die
//! Gegenzeichnung seines Seals für immer (die erste Sicht gewinnt). Der Lauf
//! endet dann mit 4 — laut, nicht grün; die übrigen Refs liegen trotzdem.
//!
//! Anders als `minds replay` führt `anchor` nichts aus dem Repository aus;
//! es liest nur Refs und signiert, was es selbst formuliert.
//!
//! # Was nie ausgegeben wird
//!
//! Der Pfad des Schlüssels (auch nicht in der Meldung von `ssh-keygen`, die
//! ihn nennen kann — [`Signer::sign`] ersetzt ihn) und der GitLab-Token (er
//! geht nur über stdin an curl, [`minds_gitlab::Project`]).
//!
//! Exit-Codes: 0 gegengezeichnet bzw. gespiegelt oder nichts Neues,
//! 4 operativer Fehler.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use minds_core::ContentHash;
use minds_core::first_sight::{FirstSight, format_at, is_project_path, parse_pipeline};
use minds_store::{FirstSightRef, StoreError};

use crate::context::Context;
use crate::replay_cmd::KEY_ENV;
use crate::text::sanitize;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Das Präfix jeder Ausgabezeile.
const LABEL: &str = "anchor   ";

/// Höchstgröße der Schlüsseldatei, die gelesen wird (nur ihr Anfang zählt).
const MAX_KEY_FILE: u64 = 64 * 1024;

/// So viele Commits umfasst der Bereich einer Pipeline höchstens.
const MAX_COMMITS: usize = 10_000;

/// So viele Seals zeichnet ein Lauf höchstens gegen — mehr ist ein Fehler.
const MAX_SEALS: usize = 5_000;

/// Der Principal der eigenen Signer-Datei (nur für die Prüfung belegter
/// Refs gegen den eigenen Schlüssel).
const SELF: &str = "minds-anchor-self";

/// Führt `minds anchor` (bzw. `--mirror`) aus.
pub fn run(mirror: bool) -> ExitCode {
    let var = |name: &str| std::env::var(name).ok();
    let result = if mirror {
        mirror_run(&var)
    } else {
        anchor(&var)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds anchor: {}", sanitize(&err.to_string()));
            ExitCode::from(4)
        }
    }
}

/// Die Identität der Pipeline aus der Umgebung von GitLab-CI.
#[derive(PartialEq, Eq)]
struct Pipeline {
    /// `CI_PROJECT_PATH`.
    project: String,
    /// `CI_PIPELINE_ID`.
    id: u64,
    /// `CI_COMMIT_SHA` — muss HEAD sein.
    sha: String,
    /// `CI_COMMIT_BEFORE_SHA`, roh — ob ein brauchbarer Commit, entscheidet
    /// [`range_seals`] (und sagt es, wenn nicht).
    before: Option<String>,
    /// `CI_COMMIT_BRANCH` — signiert wird nur in Branch-Pipelines.
    branch: String,
    /// `MINDS_ANCHOR_KEY_FILE`, roh — nie ausgegeben.
    key: PathBuf,
}

/// Von Hand: Ein `{:?}` zeigt den Schlüsselpfad nie.
impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("project", &self.project)
            .field("id", &self.id)
            .field("sha", &self.sha)
            .field("before", &self.before)
            .field("branch", &self.branch)
            .field("key", &"[key]")
            .finish()
    }
}

/// Ein Git-Objektname, der kein Null-Commit ist (`0000…` heißt bei GitLab
/// „kein Vorgänger").
fn commit_name(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && value.bytes().any(|b| b != b'0')
}

/// Liest und prüft die CI-Variablen und die Lage der Pipeline. Nennt im
/// Fehler alle fehlenden Variablen auf einmal, und nie einen Wert.
fn pipeline(var: &dyn Fn(&str) -> Option<String>) -> Result<Pipeline, String> {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    let mut missing = Vec::new();
    if var("GITLAB_CI").as_deref() != Some("true") {
        missing.push("GITLAB_CI=true");
    }
    for name in [
        "CI_PROJECT_PATH",
        "CI_PIPELINE_ID",
        "CI_COMMIT_SHA",
        KEY_ENV,
    ] {
        if set(name).is_none() {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "missing CI variables: {} — minds anchor runs only in GitLab CI with the \
             protected anchor key, never with a developer key",
            missing.join(", ")
        ));
    }
    // Dieselben Bedingungen wie beim signierenden `minds replay`: Wo der
    // Branch des Merge Requests die CI-Konfiguration stellt oder ein
    // Aufrufer Variablen mitbringt, kommt der Schlüssel nicht hin.
    if crate::replay_cmd::review_pipeline(var) {
        return Err(
            "refusing to sign in a merge request, trigger or API pipeline — its \
             configuration or variables are not reviewed; anchor on a protected branch"
                .into(),
        );
    }
    if var("CI_COMMIT_REF_PROTECTED").as_deref() != Some("true") {
        return Err(
            "refusing to sign: this pipeline does not run on a protected ref \
             (CI_COMMIT_REF_PROTECTED)"
                .into(),
        );
    }
    match var("CI_PIPELINE_SOURCE").as_deref() {
        Some("push") => {}
        source => {
            return Err(format!(
                "refusing to sign for the pipeline source {} — anchor in push pipelines \
                 of a protected branch",
                sanitize(source.unwrap_or("(unset)"))
            ));
        }
    }
    let project = set("CI_PROJECT_PATH").unwrap_or_default();
    if !is_project_path(&project) || !project.contains('/') {
        return Err("CI_PROJECT_PATH is not a GitLab project path".into());
    }
    let id = set("CI_PIPELINE_ID")
        .as_deref()
        .and_then(parse_pipeline)
        .ok_or("CI_PIPELINE_ID is not a pipeline id")?;
    let sha = set("CI_COMMIT_SHA").unwrap_or_default();
    if !commit_name(&sha) {
        return Err("CI_COMMIT_SHA is not a commit id".into());
    }
    // Nur Branch-Pipelines: Die Konfiguration einer Tag-Pipeline stammt aus
    // dem getaggten Commit, und wer einen geschützten Tag setzen darf, ist
    // nicht zwingend, wer auf den Branch mergen darf.
    let branch = set("CI_COMMIT_BRANCH").ok_or(
        "refusing to sign: not a branch pipeline (CI_COMMIT_BRANCH) — tag pipelines never sign",
    )?;
    // Nur der Default-Branch: Dorthin zählt `verify --online` einen Merge
    // als gemergt — eine Note an einem Merge Request in einen anderen
    // Branch läse niemand.
    if set("CI_DEFAULT_BRANCH").as_deref() != Some(branch.as_str()) {
        return Err(
            "refusing to sign: only the default branch is anchored (CI_COMMIT_BRANCH is not \
             CI_DEFAULT_BRANCH)"
                .into(),
        );
    }
    Ok(Pipeline {
        project,
        id,
        sha,
        before: set("CI_COMMIT_BEFORE_SHA"),
        branch,
        key: PathBuf::from(set(KEY_ENV).unwrap_or_default()),
    })
}

/// Der Zugang für die Note: `MINDS_GITLAB_TOKEN` und allein `CI_SERVER_URL`
/// — `MINDS_GITLAB_URL` könnte ein Aufrufer setzen und den Token an seinen
/// Server lenken.
fn mirror_access(
    var: &dyn Fn(&str) -> Option<String>,
    project: &str,
) -> Fallible<minds_gitlab::Project> {
    let server = var("CI_SERVER_URL")
        .filter(|url| !url.trim().is_empty())
        .ok_or("CI_SERVER_URL is not set — needed for the merge request note")?;
    let base = crate::intent_issue::base_url(Some(&server))?;
    Ok(minds_gitlab::Project::new(
        &base,
        &project.replace('/', "%2F"),
        MIRROR_TOKEN_ENV,
    )?)
}

/// Der Token für die Note — ein eigener, nie `MINDS_GITLAB_TOKEN`: Mit dem prüft
/// `verify --online` auch in Merge-Request-Pipelines, wo der Agent ihn lesen
/// kann. Wer die Note schreiben darf, darf sie auch löschen; deshalb gehört
/// dieser Token einem eigenen Bot, als geschützte Variable nur für den
/// Anker-Job.
const MIRROR_TOKEN_ENV: &str = "MINDS_ANCHOR_GITLAB_TOKEN";

/// Prüft, dass der Spiegel-Token gesetzt ist und nicht der von `verify`.
fn mirror_token(var: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let token = var(MIRROR_TOKEN_ENV)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            format!("{MIRROR_TOKEN_ENV} is not set — needed for the merge request note")
        })?;
    if var(crate::intent_issue::TOKEN_ENV).is_some_and(|other| other.trim() == token.trim()) {
        return Err(format!(
            "{MIRROR_TOKEN_ENV} equals {} — the note needs its own bot token, which merge \
             request pipelines never see",
            crate::intent_issue::TOKEN_ENV
        ));
    }
    Ok(())
}

/// `curl` unter einem absoluten `PATH`-Eintrag außerhalb des Checkouts —
/// der Token geht über stdin an genau dieses Programm.
fn resolve_curl(var: &dyn Fn(&str) -> Option<String>, ctx: &Context) -> Fallible<PathBuf> {
    let checkout = ctx.repo.workdir().unwrap_or(&ctx.root).to_path_buf();
    Ok(var("PATH")
        .as_deref()
        .and_then(|path| crate::replay_cmd::find_program("curl", path, &checkout))
        .ok_or("curl was not found under an absolute PATH entry outside the checkout")?)
}

/// Prüft Lage, Redaction und HEAD; öffnet Repository und Schlüssel.
fn prepare(var: &dyn Fn(&str) -> Option<String>) -> Fallible<(Pipeline, Context, Signer)> {
    let pipeline = pipeline(var)?;
    // Der Projektpfad steht im signierten Text und in einem gesyncten Ref:
    // Er muss die (strenge) Default-Redaction unverändert passieren.
    let redaction = minds_redact::RedactionConfig::default().pipeline()?;
    if !crate::replay_cmd::clean(&redaction, &pipeline.project) {
        return Err("CI_PROJECT_PATH does not pass the redaction policy — not signed".into());
    }
    let ctx = Context::open()?;
    let head = ctx.repo.head()?.commit().ok_or("HEAD has no commit")?;
    if head.to_string() != pipeline.sha {
        return Err(
            "refusing to sign: HEAD is not the commit of this pipeline (CI_COMMIT_SHA)".into(),
        );
    }
    let signer = Signer::resolve(&pipeline.key, var, &ctx)?;
    Ok((pipeline, ctx, signer))
}

/// Die Seals der Sessions, die die Commits dieser Pipeline per Trailer
/// nennen — sortiert. Rückverweis (`evidence.json`) und Namensraum in
/// **einem** Lauf über die Seals, nicht einmal je Session.
///
/// Der Bereich ist `CI_COMMIT_BEFORE_SHA..HEAD`; kennt der Klon den
/// Vorgänger nicht (neuer Branch, Force-Push, flacher Klon), der erste
/// Elternteil — und das wird gesagt: Was davor kam, bleibt sonst still
/// ungezeichnet. Mehr als [`MAX_COMMITS`] Commits oder [`MAX_SEALS`] Seals
/// sind ein Fehler, nie still gekürzt.
fn range_seals(ctx: &Context, pipeline: &Pipeline) -> Fallible<Range> {
    let head = ctx.repo.head()?.commit().ok_or("HEAD has no commit")?;
    // Nur der Null-Commit heißt „kein Vorgänger"; alles andere, was kein
    // Commit-Name ist, ist ein Fehler — nie still der erste Elternteil.
    if let Some(raw) = pipeline.before.as_deref()
        && !commit_name(raw)
        && raw.bytes().any(|b| b != b'0')
    {
        return Err("CI_COMMIT_BEFORE_SHA is not a commit id".into());
    }
    let before = pipeline
        .before
        .as_deref()
        .filter(|sha| commit_name(sha))
        .and_then(|sha| sha.parse::<minds_git::CommitId>().ok());
    let fallback = |why: &str| -> Fallible<Vec<minds_git::CommitId>> {
        println!("{LABEL}CI_COMMIT_BEFORE_SHA {why} — only HEAD's first-parent range is anchored");
        Ok(ctx
            .repo
            .commits_since(head, ctx.repo.first_parent(head)?, MAX_COMMITS)?)
    };
    let commits = match (before, pipeline.before.is_some()) {
        // Nicht im Klon oder hinter der Grenze eines flachen Klons: Die
        // Commits davor blieben für immer ungezeichnet (der nächste Bereich
        // beginnt danach) — ein Fehler, keine stille Kürzung.
        (Some(base), _) => ctx
            .repo
            .commits_since(head, Some(base), MAX_COMMITS)
            .map_err(|err| match err {
                minds_git::GitError::RangeTooLarge { .. } => err.to_string(),
                _ => "CI_COMMIT_BEFORE_SHA is not reachable in this clone — fetch the whole \
                      history (GIT_DEPTH: 0)"
                    .to_owned(),
            })?,
        (None, true) => fallback("names no previous commit (a new branch?)")?,
        (None, false) => fallback("is not set")?,
    };
    let mut sessions = BTreeSet::new();
    let mut trailered = Vec::new();
    for commit in commits {
        let named = ctx.repo.session_ids_of(commit)?;
        if !named.is_empty() {
            trailered.push(commit.to_string());
        }
        sessions.extend(named);
    }
    let mut seals = BTreeSet::new();
    for session in &sessions {
        seals.extend(ctx.store.seals_of(*session)?);
    }
    let named: BTreeSet<String> = sessions.iter().map(ToString::to_string).collect();
    for seal in ctx.store.list_seals()? {
        // Nur hash-gültige Seals: ein veränderter wird ohnehin nicht
        // gegengezeichnet.
        let Ok(Some(text)) = ctx.store.seal_text(&seal) else {
            continue;
        };
        if let Ok(parsed) = minds_core::evidence::Seal::parse(&text)
            && let minds_core::evidence::SealOutcome::Stored { session } = &parsed.outcome
            && named.contains(session)
        {
            seals.insert(seal);
        }
    }
    if seals.len() > MAX_SEALS {
        return Err(format!(
            "the sessions of this push name {} seals, more than the {MAX_SEALS} anchored per run",
            seals.len()
        )
        .into());
    }
    Ok(Range {
        seals: seals.into_iter().collect(),
        commits: trailered,
    })
}

/// Was der Bereich einer Pipeline umfasst.
struct Range {
    /// Die Seals, sortiert.
    seals: Vec<ContentHash>,
    /// Die Commits mit Session-Trailer — `--mirror` fragt nach ihren Merge
    /// Requests.
    commits: Vec<String>,
}

fn anchor(var: &dyn Fn(&str) -> Option<String>) -> Fallible<()> {
    crate::replay_cmd::harden_process();
    // Erst alles prüfen, dann schreiben: Ohne CI-Identität oder Schlüssel
    // entsteht kein Ref.
    let (pipeline, ctx, signer) = prepare(var)?;
    let seals = range_seals(&ctx, &pipeline)?.seals;
    let at = format_at(jiff::Timestamp::now());
    let (mut new, mut kept, mut occupied) = (0usize, 0usize, 0usize);
    for seal in seals {
        match ctx.store.first_sight(&seal) {
            Ok(FirstSightRef::Absent) => {}
            Ok(raw) => {
                match signer.own(&seal, &raw) {
                    Own::Yes(_) => kept += 1,
                    Own::Foreign(pipeline) => {
                        occupied += 1;
                        println!(
                            "{LABEL}kept {seal}: its first-sight ref holds a countersignature \
                             not made with this anchor key (claims pipeline #{pipeline}) — never \
                             overwritten"
                        );
                    }
                    Own::Unreadable => {
                        occupied += 1;
                        println!(
                            "{LABEL}kept {seal}: its first-sight ref holds no readable \
                             countersignature — never overwritten"
                        );
                    }
                    Own::CheckFailed(err) => {
                        return Err(format!(
                            "the first-sight ref of {seal} cannot be checked: {}",
                            sanitize(&err)
                        )
                        .into());
                    }
                }
                continue;
            }
            // Ein einzelner Ref, der sich nicht lesen lässt, hält den Lauf
            // für die übrigen Seals nicht auf.
            Err(_) => {
                occupied += 1;
                println!(
                    "{LABEL}kept {seal}: its first-sight ref cannot be read — never overwritten"
                );
                continue;
            }
        }
        match ctx.store.seal_text(&seal) {
            Ok(Some(_)) => {}
            Ok(None) => continue,
            Err(StoreError::SealMismatch { .. }) => {
                println!("{LABEL}skipped {seal}: the stored seal does not hash to its id");
                continue;
            }
            Err(err) => return Err(err.into()),
        }
        let first = FirstSight {
            seal: seal.clone(),
            project: pipeline.project.clone(),
            pipeline: pipeline.id,
            at: at.clone(),
        };
        let signature = signer.sign(&first.to_text()?)?;
        if ctx.store.put_first_sight(&first, &signature)? {
            println!("{LABEL}+ {seal}");
            new += 1;
        } else {
            // Ein paralleler Lauf war schneller — seine Sicht gilt.
            kept += 1;
        }
    }
    let foreign = if occupied > 0 {
        format!(", {occupied} occupied by foreign or unreadable refs")
    } else {
        String::new()
    };
    println!(
        "{LABEL}{new} new, {kept} already anchored{foreign} — pipeline #{}, {at}",
        pipeline.id
    );
    // Ein belegter Ref verhindert die Gegenzeichnung dieses Seals für immer
    // — laut, nicht als grüner Lauf (die übrigen Refs liegen trotzdem).
    if occupied > 0 {
        return Err(format!(
            "{occupied} seal(s) of this push cannot be anchored: their first-sight ref is \
             occupied by something this key did not sign"
        )
        .into());
    }
    Ok(())
}

/// `minds anchor --mirror`: die Note an die Merge Requests — nach dem Push.
fn mirror_run(var: &dyn Fn(&str) -> Option<String>) -> Fallible<()> {
    crate::replay_cmd::harden_process();
    mirror_token(var)?;
    let (pipeline, ctx, signer) = prepare(var)?;
    let project = mirror_access(var, &pipeline.project)?.with_curl(resolve_curl(var, &ctx)?);
    // Jede gültige Gegenzeichnung dieses Schlüssels zu einem Seal dieses
    // Pushs — auch eine aus einer früheren Pipeline (eine Session über zwei
    // Merge Requests): `verify --online` liest nur die Merge Requests des
    // geprüften Commits. Was jemand anders ablegte, gibt die Note nie wieder.
    let mut entries = Vec::new();
    let range = range_seals(&ctx, &pipeline)?;
    for seal in &range.seals {
        // Ein Ref, der sich nicht lesen lässt, fehlte sonst still in der
        // Note — und seine spätere Löschung fiele nie auf.
        let raw = ctx.store.first_sight(seal).map_err(|err| {
            format!(
                "the first-sight ref of {seal} cannot be read: {}",
                sanitize(&err.to_string())
            )
        })?;
        let own = signer.own(seal, &raw);
        if let Own::CheckFailed(err) = &own {
            return Err(format!(
                "the first-sight ref of {seal} cannot be checked: {}",
                sanitize(err)
            )
            .into());
        }
        if let Own::Yes(anchor) = own
            && anchor.project == pipeline.project
            && let FirstSightRef::Present {
                text: Some(text),
                signature: Some(signature),
            } = raw
        {
            entries.push(minds_gitlab::anchor::Entry { text, signature });
        }
    }
    if entries.is_empty() {
        println!("{LABEL}nothing anchored for this push — no merge request note");
        return Ok(());
    }
    let report = project
        .mirror_anchors(&range.commits, &pipeline.branch, pipeline.id, &entries)
        .map_err(|err| format!("merge request note not posted: {err}"))?;
    if report.results.is_empty() && report.not_merged == 0 {
        // Ein direkter Push ohne Merge Request: Diese Anker schützt keine
        // Note vor dem Löschen — laut, nicht still.
        eprintln!(
            "minds anchor: warning: no merge request merged this push into {} — no note; these \
             anchors are not protected against deletion",
            sanitize(&pipeline.branch)
        );
    }
    if report.not_merged > 0 {
        // Kein späterer Lauf holt diese Note nach (sein Bereich ist ein
        // anderer) — nie still grün.
        return Err(format!(
            "{} merge request(s) contain this commit but are not merged into {} yet — no note \
             posted; rerun --mirror once GitLab shows them merged",
            report.not_merged,
            sanitize(&pipeline.branch)
        )
        .into());
    }
    let mut failed = false;
    for (mr, result) in report.results {
        match result {
            Ok(m) => {
                if m.posted > 0 {
                    println!("{LABEL}MR !{mr}: note posted ({} seal(s))", m.posted);
                } else {
                    println!("{LABEL}MR !{mr}: note already there");
                }
                if m.foreign_markers > 0 {
                    println!(
                        "{LABEL}MR !{mr}: {} note(s) by another author carry an anchor marker \
                         — ignored",
                        m.foreign_markers
                    );
                }
            }
            Err(err) => {
                failed = true;
                eprintln!(
                    "minds anchor: MR !{mr}: note not posted: {}",
                    sanitize(&err)
                );
            }
        }
    }
    if failed {
        return Err("not every merge request note was posted".into());
    }
    Ok(())
}

/// Stammt ein belegter Ref von diesem Schlüssel?
#[derive(Debug, PartialEq, Eq)]
enum Own {
    /// Lesbar, dieser Seal, gültig signiert von diesem Schlüssel.
    Yes(FirstSight),
    /// Lesbar, aber nicht gültig von diesem Schlüssel signiert — mit der
    /// behaupteten Pipeline.
    Foreign(u64),
    /// Kein lesbarer Text dieses Seals oder keine Signatur.
    Unreadable,
    /// Die Prüfung selbst scheiterte (Prozess, Tempdatei) — kein Urteil.
    CheckFailed(String),
}

/// Der Signaturschlüssel und das `ssh-keygen`, das ihn benutzt.
struct Signer {
    /// Die Schlüsseldatei, aufgelöst.
    file: PathBuf,
    /// Die Schreibweisen des Pfads, die in einer Meldung ersetzt werden.
    spellings: Vec<String>,
    /// `ssh-keygen` unter einem absoluten `PATH`-Eintrag außerhalb des
    /// Checkouts.
    program: PathBuf,
    /// Der öffentliche Schlüssel (SSH-Wire-Format).
    public: Vec<u8>,
    /// Eine private Signer-Datei nur mit dem eigenen Schlüssel, beschränkt
    /// auf `minds-anchor` — lebt so lange wie der Signer.
    own_signers: tempfile::TempDir,
}

impl Signer {
    /// Der Schlüssel aus [`KEY_ENV`]: eine lesbare **private** Schlüsseldatei
    /// außerhalb des Checkouts und des Git-Verzeichnisses (beides kann ein
    /// Agent beschreiben), dazu ein `ssh-keygen` mit `-Y`.
    fn resolve(key: &Path, var: &dyn Fn(&str) -> Option<String>, ctx: &Context) -> Fallible<Self> {
        let unreadable = || format!("{KEY_ENV} does not name a readable key file");
        let file = std::fs::canonicalize(key).map_err(|_| unreadable())?;
        if !file.is_file() {
            return Err(unreadable().into());
        }
        let checkout = ctx.repo.workdir().unwrap_or(&ctx.root).to_path_buf();
        let inside = [
            checkout.clone(),
            ctx.repo.git_dir().to_path_buf(),
            ctx.repo.common_dir().to_path_buf(),
        ]
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .any(|dir| file.starts_with(dir));
        if inside {
            return Err(format!("{KEY_ENV} points into the repository — refusing to sign").into());
        }
        if !private_key_file(&file) {
            return Err(format!(
                "{KEY_ENV} does not name a private key file — never signing through ssh-agent"
            )
            .into());
        }
        let program = var("PATH")
            .as_deref()
            .and_then(|path| crate::replay_cmd::find_ssh_keygen(path, &checkout))
            .ok_or("ssh-keygen was not found under an absolute PATH entry outside the checkout")?;
        if !minds_attest::ssh_keygen_available_at(&program) {
            return Err("ssh-keygen with -Y sign is not available".into());
        }
        let spellings = spellings(key, &file);
        let line = minds_attest::ssh_public_key_with(&program, &file).map_err(|err| {
            format!(
                "the anchor key cannot be read: {}",
                scrub(&err.to_string(), &spellings)
            )
        })?;
        let public = minds_attest::public_key_blob(&line)
            .ok_or("the anchor key has no readable public key")?;
        let own_signers = tempfile::tempdir()?;
        let mut words = line.split_whitespace();
        let (kind, blob) = (
            words.next().unwrap_or_default(),
            words.next().unwrap_or_default(),
        );
        std::fs::write(
            own_signers.path().join("allowed_signers"),
            format!(
                "{SELF} namespaces=\"{}\" {kind} {blob}\n",
                minds_attest::NS_ANCHOR
            ),
        )?;
        Ok(Self {
            file,
            spellings,
            program,
            public,
            own_signers,
        })
    }

    /// Signiert `text` unter `minds-anchor` — nur mit der Datei, nie über
    /// einen ssh-agent. Eine Meldung von `ssh-keygen` kann den Pfad des
    /// Schlüssels nennen — er wird ersetzt, bevor sie irgendwo erscheint.
    fn sign(&self, text: &str) -> Result<String, String> {
        minds_attest::ssh_sign_ns_file_only(
            &self.program,
            text,
            &self.file,
            minds_attest::NS_ANCHOR,
        )
        .map_err(|err| {
            format!(
                "the anchor key cannot sign: {}",
                scrub(&err.to_string(), &self.spellings)
            )
        })
    }

    /// Ob der Ref von `seal` eine **gültige** Gegenzeichnung dieses
    /// Schlüssels trägt. Erst in-process über den Schlüssel, den die
    /// Signatur nennt (sortiert Fremdes ohne Prozess aus), dann mit
    /// `ssh-keygen -Y verify` gegen die eigene Signer-Datei — eine Signatur,
    /// die nur den öffentlichen CI-Schlüssel nennt, gilt nicht.
    fn own(&self, seal: &ContentHash, raw: &FirstSightRef) -> Own {
        let FirstSightRef::Present {
            text: Some(text),
            signature: Some(signature),
        } = raw
        else {
            return Own::Unreadable;
        };
        let Ok(anchor) = FirstSight::parse(text) else {
            return Own::Unreadable;
        };
        if anchor.seal != *seal {
            return Own::Unreadable;
        }
        let ours =
            minds_attest::signature_public_key(signature).as_deref() == Some(&self.public[..]);
        if !ours {
            return Own::Foreign(anchor.pipeline);
        }
        match minds_attest::ssh_verify_ns_with(
            &self.program,
            text,
            signature,
            &self.own_signers.path().join("allowed_signers"),
            SELF,
            minds_attest::NS_ANCHOR,
        ) {
            Ok(true) => Own::Yes(anchor),
            Ok(false) => Own::Foreign(anchor.pipeline),
            Err(err) => Own::CheckFailed(scrub(&err.to_string(), &self.spellings)),
        }
    }
}

/// Beginnt die Datei wie ein privater Schlüssel (`-----BEGIN … PRIVATE
/// KEY-----`)? Eine öffentliche Zeile (`ssh-ed25519 AAAA…`) oder ein
/// `PUBLIC KEY`-Block nicht.
fn private_key_file(file: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    if std::fs::File::open(file)
        .and_then(|f| f.take(MAX_KEY_FILE).read_to_end(&mut head))
        .is_err()
    {
        return false;
    }
    let first = head.split(|b| *b == b'\n').next().unwrap_or_default();
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    first.starts_with(b"-----BEGIN ") && first.ends_with(b"PRIVATE KEY-----")
}

/// Die Schreibweisen des Schlüsselpfads, die in Meldungen ersetzt werden:
/// der aufgelöste Pfad und die übergebene Schreibweise, wenn sie absolut ist
/// oder lang genug, um kein gewöhnliches Wort zu sein. Längere zuerst — ein
/// kürzerer Pfad kann Präfix eines längeren sein.
pub(crate) fn spellings(given: &Path, resolved: &Path) -> Vec<String> {
    let mut out: Vec<String> = [given, resolved]
        .iter()
        .filter(|path| path.is_absolute() || path.as_os_str().len() > 3)
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|spelling| !spelling.is_empty())
        .collect();
    out.sort_by_key(|spelling| std::cmp::Reverse(spelling.len()));
    out.dedup();
    out
}

/// Ersetzt jede Schreibweise des Schlüsselpfads durch `[key]`.
pub(crate) fn scrub(message: &str, spellings: &[String]) -> String {
    spellings.iter().fold(message.to_owned(), |out, spelling| {
        out.replace(spelling, "[key]")
    })
}

#[cfg(test)]
mod tests;
