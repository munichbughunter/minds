//! `minds verify --receipt [<rev>] [--remote <r>] [--receipt-key <key> |
//! --receipt-key-file <pfad>] [--require-state <zustand>]
//! [--require-protected-anchor]` (Spec S3-MVP §6.3): Ein unabhängiger Prüfer
//! rechnet die signierte Server-Quittung eines Commits samt historischem
//! Trust-Snapshot und Evidence nach — auch aus einem frischen Clone.
//!
//! Ablauf: Vertrauensanker bestimmen ([`anchor`]); mit `--remote` die
//! Evidence in Tracking-Refs beschaffen ([`fetch`]); je Quittung Signatur
//! gegen `keys.json` **und** den Anker, bei `/2` den Snapshot prüfen (nie
//! Rückfall auf `/1`) und nachrechnen ([`recompute`]).
//!
//! Exit-Codes passend zu `minds verify`: 0 Zustand `Verified`; 1 Signatur,
//! Snapshot, Anker oder Nachrechnung falsch; 2 Zustand schlechter als
//! `--require-state` (Standard `Verified`); 3 keine Quittung, Evidence fehlt
//! oder nicht prüfbar; 4 Betriebsfehler. Mehrere Quittungen: das
//! Schlechteste gewinnt (4 > 1 > 3 > 2 > 0).
//!
//! Die Ausgabe nennt Zustände nach ADR-PRODUCT-001 (`Verified`, `Partial`,
//! `Unverified`, `Invalid`), dieselben Wörter wie die Warnung des Servers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use minds_witness_core::{
    CommitId, CoverageStatus, Finding, PolicyVerdict, Receipt, ReceiptStore, TrustSnapshot,
    VerifyError, display_ref, verify_receipt, verify_receipt_with_trust,
};
use minds_witness_git::hook::warn::{self, State};
use minds_witness_git::inspector::{EVIDENCE_PREFIX, OBSERVATIONS_PREFIX};
use minds_witness_git::receipt_store::GitReceiptStore;

pub(crate) mod anchor;
mod fetch;
mod recompute;

use anchor::{AnchorError, Class, Sources};
use fetch::{FetchError, Remote};
use recompute::{Counts, Outcome, Tracked};

/// Exit 0: `Verified`.
const VERIFIED: u8 = 0;
/// Exit 1: etwas ist falsch.
const WRONG: u8 = 1;
/// Exit 2: Zustand schlechter als verlangt.
const BELOW: u8 = 2;
/// Exit 3: fehlt oder nicht prüfbar.
const MISSING: u8 = 3;
/// Exit 4: Betriebsfehler.
const ERROR: u8 = 4;

/// Frist der lesenden Git-Prozesse der Ablage.
const STORE_TIME: Duration = Duration::from_secs(30);

/// Die Argumente.
pub(crate) struct Options<'a> {
    pub(crate) rev: Option<&'a str>,
    pub(crate) remote: Option<&'a str>,
    pub(crate) key: Option<&'a str>,
    pub(crate) key_file: Option<&'a str>,
    pub(crate) require_state: Option<&'a str>,
    pub(crate) require_protected_anchor: bool,
}

/// Rang eines Zustands; kleiner ist besser.
fn rank(state: State) -> u8 {
    match state {
        State::Verified => 0,
        State::Partial => 1,
        State::Unverified => 2,
        State::Invalid => 3,
        State::Unknown => 4,
    }
}

fn parse_state(word: &str) -> Option<State> {
    [
        State::Verified,
        State::Partial,
        State::Unverified,
        State::Invalid,
    ]
    .into_iter()
    .find(|s| s.word() == word)
}

/// Schwere eines Exit-Codes für „das Schlechteste gewinnt“.
fn severity(code: u8) -> u8 {
    match code {
        ERROR => 4,
        WRONG => 3,
        MISSING => 2,
        BELOW => 1,
        _ => 0,
    }
}

fn worst(a: u8, b: u8) -> u8 {
    if severity(b) > severity(a) { b } else { a }
}

/// Einstieg aus `main`.
pub(crate) fn run(opts: &Options<'_>) -> ExitCode {
    let mut out = std::io::stdout().lock();
    // Auch in einem Bare-Repository (Prüfung auf dem Server selbst): Die
    // Wurzel ist das Arbeitsverzeichnis, Git findet das Repository.
    let Ok(root) = std::env::current_dir() else {
        let _ = writeln!(
            out,
            "minds verify --receipt: Arbeitsverzeichnis nicht lesbar"
        );
        return ExitCode::from(ERROR);
    };
    let key_file = opts.key_file.map(PathBuf::from);
    let sources = Sources {
        key: opts.key,
        key_file: key_file.as_deref(),
        default_file: Path::new(anchor::DEFAULT_KEY_FILE),
        config: global_config(&root),
    };
    ExitCode::from(verify(&root, opts, &sources, &mut out))
}

/// `git config --global --get minds.witness.receiptKey`.
fn global_config(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--global", "--get", anchor::CONFIG_KEY])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let value = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !value.is_empty()).then_some(value)
}

/// Die Prüfung; liefert den Exit-Code.
pub(crate) fn verify(
    root: &Path,
    opts: &Options<'_>,
    sources: &Sources<'_>,
    out: &mut dyn Write,
) -> u8 {
    let mut say = |line: &str| {
        let _ = writeln!(out, "{line}");
    };
    let required = match opts.require_state.map(parse_state) {
        None => State::Verified,
        Some(Some(state)) => state,
        Some(None) => {
            say(
                "minds verify --receipt: --require-state erwartet Verified, Partial, Unverified oder Invalid",
            );
            return ERROR;
        }
    };
    let anchor = match anchor::resolve(sources) {
        Ok(Some(a)) => a,
        Ok(None) => {
            say(&format!(
                "NICHT PRÜFBAR: kein Vertrauensanker (--receipt-key, --receipt-key-file, {} oder git config --global {})",
                anchor::DEFAULT_KEY_FILE,
                anchor::CONFIG_KEY
            ));
            return MISSING;
        }
        Err(AnchorError::Contradiction) => {
            say("Vertrauensanker widersprüchlich");
            return WRONG;
        }
        Err(AnchorError::Unusable(why)) => {
            say(&format!(
                "Vertrauensanker nicht verwendbar: {}",
                crate::text::sanitize(&why)
            ));
            return ERROR;
        }
    };
    if opts.require_protected_anchor && anchor.class != Class::Protected {
        say(&anchor.line());
        say("NICHT PRÜFBAR: --require-protected-anchor verlangt einen geschützten Vertrauensanker");
        return MISSING;
    }
    // Jede Ausgabe nennt den Anker: die Blöcke je Quittung selbst, ein
    // vorzeitiges Ende hier davor.
    let anchor_line = anchor.line();

    let (Some(git), Some(git_dir)) = (crate::bind_cmd::which_git(), crate::bind_cmd::git_dir(root))
    else {
        say(&anchor_line);
        say("minds verify --receipt: git oder Git-Verzeichnis nicht gefunden");
        return ERROR;
    };
    let remote = match opts.remote {
        None => None,
        Some(name) => match Remote::named(root, name) {
            Ok(r) => Some(r),
            Err(FetchError::NotNamed) => {
                say(&anchor_line);
                say(&format!(
                    "minds verify --receipt: {} ist kein benannter Remote (eine URL bestimmt keine Zielnamen)",
                    crate::text::sanitize(name)
                ));
                return ERROR;
            }
            Err(FetchError::Failed(why)) => {
                say(&anchor_line);
                say(&format!(
                    "minds verify --receipt: Remote nicht lesbar ({why})"
                ));
                return ERROR;
            }
        },
    };
    if let Some(r) = &remote
        && let Err(e) = r.witness()
    {
        say(&anchor_line);
        say(&fetch_failed(r, &e));
        return ERROR;
    }

    // Der Commit, notfalls vom Remote.
    let rev = opts.rev.unwrap_or("HEAD");
    let full_hex = matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit());
    let mut commit = resolve_commit(root, rev);
    if commit.is_none()
        && full_hex
        && let Some(r) = &remote
    {
        if let Err(e) = r.branches() {
            say(&anchor_line);
            say(&fetch_failed(r, &e));
            return ERROR;
        }
        commit = resolve_commit(root, rev);
    }
    let Some(commit) = commit else {
        if full_hex {
            say(&anchor_line);
            say(&format!("Commit {} nicht abrufbar", &rev[..12]));
            return MISSING;
        }
        say(&anchor_line);
        say(&format!(
            "minds verify --receipt: Revision {} nicht auflösbar",
            crate::text::sanitize(rev)
        ));
        return ERROR;
    };
    let c12 = commit.as_str()[..12].to_owned();

    let store = match &remote {
        None => GitReceiptStore::new(&git, &git_dir, STORE_TIME),
        Some(r) => match GitReceiptStore::reader(
            &git,
            &git_dir,
            STORE_TIME,
            &format!("{}witness/", r.tracking()),
        ) {
            Ok(s) => s,
            Err(_) => {
                say(&anchor_line);
                say("minds verify --receipt: Remote-Name taugt nicht als Tracking-Namensraum");
                return ERROR;
            }
        },
    };
    let keys_list = match store.list(&commit) {
        Ok(list) => list,
        Err(e) => {
            say(&anchor_line);
            say(&format!(
                "minds verify --receipt: Quittungen nicht lesbar ({})",
                crate::text::sanitize(&e.to_string())
            ));
            return ERROR;
        }
    };
    if keys_list.is_empty() {
        say(&anchor_line);
        say(&format!("NICHT PRÜFBAR: keine Quittung für Commit {c12}"));
        return MISSING;
    }

    // Bindungen, Seals und Observations, nur für diesen Commit.
    if let Some(r) = &remote
        && let Err(e) = fetch_evidence(r, &git_dir, commit.as_str())
    {
        say(&anchor_line);
        say(&fetch_failed(r, &e));
        return ERROR;
    }
    let repo = match crate::bind_cmd::open_view(&git_dir) {
        Ok(repo) => repo,
        Err(e) => {
            say(&anchor_line);
            say(&format!(
                "minds verify --receipt: {}",
                crate::text::sanitize(&e)
            ));
            return ERROR;
        }
    };
    let view = Tracked::new(&repo, remote.as_ref().map(Remote::tracking));
    let has_keys = match store.has_keys() {
        Ok(found) => found,
        Err(_) => {
            say(&anchor_line);
            say("minds verify --receipt: Schlüsselliste nicht lesbar");
            return ERROR;
        }
    };
    let keys = if has_keys {
        match store.public_keys() {
            Ok(keys) => keys,
            Err(_) => {
                say(&anchor_line);
                say("minds verify --receipt: Schlüsselliste nicht lesbar");
                return ERROR;
            }
        }
    } else {
        Vec::new()
    };
    // Das erwartete Projekt (`git config minds.project`, wie beim Binden):
    // Eine Quittung für ein anderes Projekt mit demselben Commit zählt nie.
    let project = match crate::bind_cmd::expected_project(root) {
        Ok(p) => p,
        Err(e) => {
            say(&anchor_line);
            say(&format!(
                "minds verify --receipt: {}",
                crate::text::sanitize(&e)
            ));
            return ERROR;
        }
    };
    let env = Env {
        root,
        anchor: &anchor,
        has_keys,
        keys: &keys,
        store: &store,
        repo: &repo,
        view: &view,
        remote: remote.as_ref().map(Remote::name),
        project: project.as_deref(),
        required,
    };
    let mut code = VERIFIED;
    for (i, key) in keys_list.iter().enumerate() {
        if i > 0 {
            say("");
        }
        let bytes = match store.get(key) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => {
                say(&format!("Quittung {}: nicht lesbar", key.id.as_str()));
                code = worst(code, ERROR);
                continue;
            }
        };
        let (lines, one) = check(&env, &commit, key.id.as_str(), &bytes);
        for line in lines {
            say(&line);
        }
        code = worst(code, one);
    }
    code
}

fn fetch_failed(r: &Remote<'_>, e: &FetchError) -> String {
    let why = match e {
        FetchError::NotNamed => "kein benannter Remote",
        FetchError::Failed(why) => why,
    };
    format!(
        "minds verify --receipt: Beschaffung vom Remote {} gescheitert ({why})",
        crate::text::sanitize(r.name())
    )
}

/// Löst `rev` zu einem vorhandenen Commit auf.
fn resolve_commit(root: &Path, rev: &str) -> Option<CommitId> {
    if rev.starts_with('-') {
        return None;
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{rev}^{{commit}}"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    CommitId::parse(String::from_utf8(out.stdout).ok()?.trim()).ok()
}

/// Schritt 2 und 3 der Beschaffung für `commit`.
fn fetch_evidence(r: &Remote<'_>, git_dir: &Path, commit: &str) -> Result<(), FetchError> {
    r.bindings(commit)?;
    let open = || {
        crate::bind_cmd::open_view(git_dir).map_err(|_| FetchError::Failed("repository unreadable"))
    };
    let repo = open()?;
    let view = Tracked::new(&repo, Some(r.tracking()));
    let cands = recompute::candidates_of(&repo, &view, commit)
        .map_err(|_| FetchError::Failed("binding refs unreadable"))?;
    let mut seals = std::collections::BTreeSet::new();
    for cand in cands.iter().take(recompute::max_candidates()) {
        if let Ok(Some(b)) = recompute::binding_of(&view, cand) {
            seals.extend(
                b.seals()
                    .iter()
                    .map(|id| format!("{EVIDENCE_PREFIX}{}", &id[3..])),
            );
        }
    }
    let present = r.exact(&seals)?;
    let repo = open()?;
    let view = Tracked::new(&repo, Some(r.tracking()));
    let mut observations = std::collections::BTreeSet::new();
    for name in &present {
        let id = format!("b3-{}", &name[EVIDENCE_PREFIX.len()..]);
        if let Ok(Some(obs)) = recompute::observations_of(&view, &id) {
            observations.insert(format!("{OBSERVATIONS_PREFIX}{}", &obs[3..]));
        }
    }
    r.exact(&observations)?;
    Ok(())
}

/// Was jede Quittung braucht.
struct Env<'a> {
    root: &'a Path,
    anchor: &'a anchor::Anchor,
    has_keys: bool,
    keys: &'a [minds_witness_core::PublicKeyEntry],
    store: &'a GitReceiptStore,
    repo: &'a minds_git::quarantine::ServerRepo,
    view: &'a Tracked<'a>,
    remote: Option<&'a str>,
    /// `git config minds.project`, sofern gesetzt.
    project: Option<&'a str>,
    required: State,
}

fn finding_of(r: &Receipt) -> Finding {
    Finding {
        commit: r.commit.clone(),
        coverage: r.coverage.clone(),
        policy: r.policy.clone(),
        push: r.push.clone(),
        pusher: r.pusher.clone(),
        repo: r.repo.clone(),
        seal: r.seal.clone(),
        server: r.server.clone(),
    }
}

fn mode_word(r: &Receipt) -> &'static str {
    match r.policy.mode {
        minds_witness_core::Mode::Observe => "observe",
        minds_witness_core::Mode::Enforce => "enforce",
    }
}

/// Die Zeile `Zustand: …` und der Zustand.
fn state_line(r: &Receipt, snapshot: Option<&TrustSnapshot>) -> (State, String) {
    let state = warn::state(&finding_of(r));
    let c = &r.coverage;
    let fail = r.policy.result == PolicyVerdict::Fail;
    let threshold = snapshot.map(|s| s.policy.coverage_threshold_percent);
    let counts = if c.status == CoverageStatus::MergeCommit {
        "Merge ohne eigene Änderungen".to_owned()
    } else {
        match threshold.and_then(|t| warn::percent(c.paths_covered, c.paths_in_diff, fail, t)) {
            Some(p) => format!(
                "{}/{} Dateien nachgewiesen ({p} %)",
                c.paths_covered, c.paths_in_diff
            ),
            None => format!(
                "{}/{} Dateien nachgewiesen",
                c.paths_covered, c.paths_in_diff
            ),
        }
    };
    let policy = match (fail, threshold) {
        (false, Some(t)) => format!("Policy erfüllt ({}, Schwelle {t} %)", mode_word(r)),
        (true, Some(t)) => format!("Policy nicht erfüllt ({}, Schwelle {t} %)", mode_word(r)),
        (false, None) => format!("Policy erfüllt ({})", mode_word(r)),
        (true, None) => format!("Policy nicht erfüllt ({})", mode_word(r)),
    };
    let truncated = if r.push.truncated {
        ", unvollständige Prüfung (zu viele neue Commits)"
    } else {
        ""
    };
    (
        state,
        format!("  Zustand: {}, {counts}{truncated}, {policy}", state.word()),
    )
}

/// Die Quittung liegt nicht unter ihrer eigenen Id.
fn misplaced() -> String {
    "  Ablage: Quittung liegt unter fremdem Namen (Id weicht ab)".to_owned()
}

fn signature_line(e: &VerifyError) -> String {
    let why = match e {
        VerifyError::UnknownKey(_) => "Schlüssel nicht in der Schlüsselliste",
        VerifyError::KeyNotValidAt { .. } => "Schlüssel zum Serverzeitpunkt nicht gültig",
        VerifyError::BadSignature => "Signatur passt nicht",
        VerifyError::NotCanonical => "Quittung nicht kanonisch",
        VerifyError::Malformed(_) | VerifyError::UnknownSchema(_) => "Quittung nicht lesbar",
        VerifyError::TrustSnapshotRequired | VerifyError::NoTrustContext => "Schema passt nicht",
        VerifyError::TrustSnapshotMalformed(_) | VerifyError::TrustMismatch => {
            "Trust-Snapshot passt nicht"
        }
    };
    format!("  Signatur: ungültig ({why})")
}

/// Prüft eine Quittung; liefert die Ausgabezeilen und den Exit-Code.
fn check(env: &Env<'_>, commit: &CommitId, id: &str, bytes: &[u8]) -> (Vec<String>, u8) {
    let mut lines = Vec::new();
    let receipt = match Receipt::from_bytes(bytes) {
        Ok(r) => r,
        Err(e) => {
            lines.push(format!("Quittung {id}"));
            lines.push(format!("  {}", env.anchor.line()));
            lines.push(signature_line(&e));
            lines.push("  Zustand: nicht festgestellt".to_owned());
            return (lines, WRONG);
        }
    };
    lines.push(format!(
        "Quittung {id} für {} {} ({}, Server {})",
        display_ref(&receipt.push.ref_name),
        &receipt.commit.as_str()[..receipt.commit.as_str().len().min(12)],
        receipt.server.time.as_str(),
        crate::text::sanitize(&receipt.server.host)
    ));
    lines.push(format!("  {}", env.anchor.line()));
    let undetermined = |mut lines: Vec<String>, code: u8| {
        lines.push("  Zustand: nicht festgestellt".to_owned());
        (lines, code)
    };
    if receipt.commit != *commit {
        lines.push("  Signatur: nicht geprüft (Quittung nennt einen anderen Commit)".to_owned());
        return undetermined(lines, WRONG);
    }
    let shown_project = crate::text::sanitize(&receipt.repo);
    match env.project {
        Some(expected) if expected != receipt.repo => {
            lines.push(format!(
                "  Projekt: {shown_project} — erwartet {expected} (minds.project)"
            ));
            return undetermined(lines, WRONG);
        }
        Some(_) => lines.push(format!("  Projekt: {shown_project} (wie minds.project)")),
        None => lines.push(format!(
            "  Projekt: {shown_project} (minds.project nicht gesetzt, nicht abgeglichen)"
        )),
    }
    if !env.anchor.keys.contains(receipt.witness.key.as_str()) {
        lines.push(format!(
            "  Signatur: ungültig (Schlüssel {} ist nicht der Vertrauensanker)",
            receipt.witness.key.as_str()
        ));
        return undetermined(lines, WRONG);
    }
    if !env.has_keys {
        lines.push("  Signatur: nicht geprüft".to_owned());
        lines.push("  fehlt: Schlüsselliste (refs/minds/witness/keys)".to_owned());
        return undetermined(lines, MISSING);
    }
    let key_line = |v: &minds_witness_core::VerifiedReceipt| {
        format!(
            "  Signatur: gültig (Schlüssel {}, gültig seit {})",
            v.key.key.as_str(),
            v.key.valid_from.as_str()
        )
    };

    // Schema /1: Signatur nach S1, keine Nachrechnung.
    let Some(trust) = receipt.trust.clone() else {
        return match verify_receipt(bytes, env.keys) {
            Ok(v) => {
                lines.push(key_line(&v));
                if receipt.id().as_str() != id {
                    lines.push(misplaced());
                    return undetermined(lines, WRONG);
                }
                lines.push("  Vertrauenskontext: nicht enthalten (Schema /1)".to_owned());
                lines.push("  Nachrechnung: nicht möglich (Schema /1)".to_owned());
                let (_, state) = state_line(&receipt, None);
                lines.push(format!("{state} (laut Quittung, nicht nachgerechnet)"));
                (lines, MISSING)
            }
            Err(e) => {
                lines.push(signature_line(&e));
                undetermined(lines, WRONG)
            }
        };
    };

    // Schema /2: nur mit Snapshot, nie als /1.
    let snapshot_bytes = match env.store.trust_bytes(&trust) {
        Ok(Some(b)) => b,
        Ok(None) => {
            lines.push(
                "  Signatur: nicht geprüft (ohne Trust-Snapshot kein Rückfall auf Schema /1)"
                    .to_owned(),
            );
            lines.push(format!("  fehlt: Trust-Snapshot {}", trust.as_str()));
            return undetermined(lines, MISSING);
        }
        Err(_) => {
            lines.push("  Vertrauenskontext: nicht lesbar".to_owned());
            return undetermined(lines, ERROR);
        }
    };
    let (verified, snapshot) = match verify_receipt_with_trust(bytes, env.keys, &snapshot_bytes) {
        Ok(ok) => ok,
        Err(e @ (VerifyError::TrustSnapshotMalformed(_) | VerifyError::TrustMismatch)) => {
            // Die Signatur ist zu diesem Zeitpunkt geprüft (Reihenfolge in
            // `verify_receipt_with_trust`).
            lines.push("  Signatur: gültig".to_owned());
            lines.push(format!(
                "  Vertrauenskontext: {} passt nicht zur signierten Quittung ({})",
                trust.as_str(),
                match e {
                    VerifyError::TrustMismatch => "Hash weicht ab",
                    _ => "Snapshot nicht lesbar",
                }
            ));
            return undetermined(lines, WRONG);
        }
        Err(e) => {
            lines.push(signature_line(&e));
            return undetermined(lines, WRONG);
        }
    };
    lines.push(key_line(&verified));
    if receipt.id().as_str() != id {
        lines.push(misplaced());
        return undetermined(lines, WRONG);
    }
    lines.push(format!(
        "  Vertrauenskontext: {} vorhanden, Hash stimmt mit der signierten Quittung ({})",
        trust.as_str(),
        if snapshot.require_separated_witness {
            "require_separated_witness = true"
        } else {
            "require_separated_witness = false: auch nicht getrennte Witness-Principals anerkannt"
        }
    ));

    let Some(program) = crate::verify_cmd::conformance::spec_program(env.root) else {
        lines.push(
            "  Nachrechnung: nicht möglich (kein root-eigenes ssh-keygen gefunden)".to_owned(),
        );
        return undetermined(lines, ERROR);
    };
    let outcome = recompute::recompute(env.repo, env.view, &program, &receipt, &snapshot);
    let source = match env.remote {
        Some(r) => format!("vom Remote {} beschafft", crate::text::sanitize(r)),
        None => "aus den lokalen Refs".to_owned(),
    };
    let counted = |c: &Counts| {
        format!(
            "  Evidence: {source} ({} {}, {} {}, {} Observations)",
            c.bindings,
            if c.bindings == 1 {
                "Bindung"
            } else {
                "Bindungen"
            },
            c.seals,
            if c.seals == 1 { "Seal" } else { "Seals" },
            c.observations
        )
    };
    match outcome {
        Outcome::Matches { counts, note } => {
            lines.push(counted(&counts));
            lines.push("  Nachrechnung: stimmt (seal.hash, Abdeckung, Policy)".to_owned());
            if let Some(note) = note {
                lines.push(format!("  {note}"));
            }
            let (state, line) = state_line(&receipt, Some(&snapshot));
            lines.push(line);
            let code = if rank(state) <= rank(env.required) {
                VERIFIED
            } else {
                BELOW
            };
            (lines, code)
        }
        Outcome::Differs(what) => {
            lines.push(format!("  Evidence: {source}"));
            lines.push(format!("  Nachrechnung: weicht ab ({what})"));
            undetermined(lines, WRONG)
        }
        Outcome::Missing(items) => {
            lines.push(format!("  Evidence: {source}, unvollständig"));
            lines.extend(items.into_iter().map(|i| format!("  fehlt: {i}")));
            lines.push("  Nachrechnung: nicht möglich (Evidence fehlt)".to_owned());
            undetermined(lines, MISSING)
        }
        Outcome::NotRecomputable(why) => {
            lines.push(format!("  Evidence: {source}"));
            lines.push(format!("  Nachrechnung: nicht nachrechenbar ({why})"));
            undetermined(lines, MISSING)
        }
        Outcome::Failed(why) => {
            lines.push(format!(
                "  Nachrechnung: technischer Fehler ({})",
                crate::text::sanitize(&why)
            ));
            undetermined(lines, ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_combine_worst_first() {
        assert_eq!(worst(VERIFIED, BELOW), BELOW);
        assert_eq!(worst(BELOW, MISSING), MISSING);
        assert_eq!(worst(MISSING, WRONG), WRONG);
        assert_eq!(worst(WRONG, ERROR), ERROR);
        assert_eq!(worst(ERROR, VERIFIED), ERROR);
    }

    #[test]
    fn required_states_are_the_product_words() {
        for word in ["Verified", "Partial", "Unverified", "Invalid"] {
            assert_eq!(parse_state(word).unwrap().word(), word);
        }
        assert_eq!(parse_state("verified"), None);
        assert_eq!(parse_state("Unknown"), None);
        assert!(rank(State::Verified) < rank(State::Partial));
        assert!(rank(State::Partial) < rank(State::Unverified));
        assert!(rank(State::Unverified) < rank(State::Invalid));
    }
}
