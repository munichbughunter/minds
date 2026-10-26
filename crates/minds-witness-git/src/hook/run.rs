//! Ablauf des Hook-Binarys (Spec S2 §11). Auf stderr steht nur die feste
//! Ablehnungsmeldung oder die Warnungsausgabe aus [`super::warn`], nie ein
//! Ereignis, eine Diagnose, ein Fehlertext oder ein Panic-Payload.
//! Ereignisse und Diagnosen gehen als JSON-Zeilen nach `<out>/log.jsonl`.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Instant;

use minds_git::quarantine::{ReceiveEnv, ServerRepo};
use minds_witness_core::{
    CommitId, CommitInfo, CommitRange, Decision, HashStr, LogEvent, Mode, ObjectAccess,
    ObjectError, PostOutcome, PushContext, Pusher, SealBlob, ServerInfo, ServerKind, Signer,
    Timestamp, WitnessIdentity, post_receive, pre_receive,
};
use serde::Serialize;

use super::config::HookConfig;
use super::store::DirReceiptStore;
use super::warn;
use crate::access::GitObjectAccess;
use crate::budget::Budget;
use crate::guard::{Guard, MAX_STDIN_LINE, parse_stdin};
use crate::inspector::{GitSealInspector, InspectorConfig};
use crate::trust::{TrustConfig, Verifier};

/// Exit-Code einer Ablehnung.
pub const EXIT_REJECT: i32 = 1;

const MSG_CONFIG: &str = "minds-witness: Konfiguration ungültig";
const MSG_REPO: &str = "minds-witness: Repository erfüllt die Voraussetzungen nicht";
const MSG_FSCK: &str = "minds-witness: receive.fsckObjects ist nicht wirksam";
const MSG_INTERNAL: &str = "minds-witness: interner Fehler";

/// Einstieg: `args[1]` ist der Pfad der Konfiguration. Jeder Panic außerhalb
/// des Kerns endet mit dem festen Satz und Exit 1.
pub fn main_with(args: &[String]) -> i32 {
    // Schritt 1: Der Panic-Hook schreibt nichts. Panics im Kern meldet die
    // Warnungsausgabe als `Unknown`, alle anderen der feste Satz unten.
    std::panic::set_hook(Box::new(|_| {}));
    let Some(path) = args.get(1) else {
        say(&[MSG_CONFIG.to_owned()]);
        return EXIT_REJECT;
    };
    let path = Path::new(path).to_owned();
    match catch_unwind(AssertUnwindSafe(|| run(&path))) {
        Ok(code) => code,
        Err(_) => {
            say(&[MSG_INTERNAL.to_owned()]);
            EXIT_REJECT
        }
    }
}

fn say(lines: &[String]) {
    let mut err = std::io::stderr().lock();
    for l in lines {
        let _ = writeln!(err, "{l}");
    }
}

/// Eine Zeile in `<out>/log.jsonl`.
#[derive(Serialize)]
struct LogLine<'a> {
    phase: &'a str,
    #[serde(flatten)]
    body: LogBody<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum LogBody<'a> {
    Event(&'a LogEvent),
    Diagnostic {
        diagnostic: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        ref_name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<&'a str>,
    },
}

struct Log {
    lines: Vec<String>,
}

impl Log {
    fn event(&mut self, phase: &str, e: &LogEvent) {
        if let Ok(s) = serde_json::to_string(&LogLine {
            phase,
            body: LogBody::Event(e),
        }) {
            self.lines.push(s);
        }
    }

    fn diag(
        &mut self,
        phase: &str,
        reason: &str,
        ref_name: Option<&str>,
        id: Option<&str>,
        principal: Option<&str>,
    ) {
        if let Ok(s) = serde_json::to_string(&LogLine {
            phase,
            body: LogBody::Diagnostic {
                diagnostic: reason,
                ref_name,
                id,
                principal,
            },
        }) {
            self.lines.push(s);
        }
    }

    fn flush(&self, out: &Path) {
        if self.lines.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(out);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out.join("log.jsonl"))
        {
            for l in &self.lines {
                let _ = writeln!(f, "{l}");
            }
        }
    }
}

/// Testschalter `panic_in`: panickt in der benannten Methode.
struct Panicking<'a> {
    inner: &'a dyn ObjectAccess,
    method: &'a str,
    text: String,
    fired: Cell<bool>,
}

impl Panicking<'_> {
    fn maybe(&self, name: &str) {
        if self.method == name && !self.fired.replace(true) {
            std::panic::panic_any(self.text.clone());
        }
    }
}

impl ObjectAccess for Panicking<'_> {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        self.maybe("commit");
        self.inner.commit(id)
    }

    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        self.maybe("new_commits");
        self.inner.new_commits(old, new, limit)
    }

    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError> {
        self.maybe("changed_paths");
        self.inner.changed_paths(id)
    }

    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        self.maybe("seal_for_commit");
        self.inner.seal_for_commit(id)
    }
}

fn now() -> Option<Timestamp> {
    let secs = jiff::Timestamp::now().as_second();
    let ts = jiff::Timestamp::from_second(secs).ok()?;
    Timestamp::parse(&ts.to_string()).ok()
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn identity(signer: &Signer) -> Option<WitnessIdentity> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(exe).ok()?;
    Some(WitnessIdentity {
        binary: HashStr::of_bytes(&bytes),
        key: signer.public_key(),
    })
}

fn run(path: &Path) -> i32 {
    // Schritt 2: Konfiguration.
    let Ok(config) = HookConfig::load(path) else {
        say(&[MSG_CONFIG.to_owned()]);
        return EXIT_REJECT;
    };
    let mut log = Log { lines: Vec::new() };
    let code = run_with(&config, &mut log);
    log.flush(&config.out_dir);
    code
}

fn run_with(config: &HookConfig, log: &mut Log) -> i32 {
    let limits = config.limits();
    let budget = Budget::new(
        limits.max_ssh_keygen_processes,
        limits.max_evidence_bytes_per_push,
        Some(Instant::now() + limits.deadline),
    );
    let mode = config.policy.mode.clone();
    let (Ok(seed), Ok(valid_from)) = (config.seed(), Timestamp::parse(&config.key_valid_from))
    else {
        say(&[MSG_CONFIG.to_owned()]);
        return EXIT_REJECT;
    };
    let signer = Signer::from_seed(seed, valid_from);
    let Some(time) = now() else {
        say(&[MSG_INTERNAL.to_owned()]);
        return EXIT_REJECT;
    };

    // Schritt 3: Snapshot, Trust-Datei, wirksames fsck, stdin, Vorprüfung.
    let Ok(mut env) = ReceiveEnv::from_process() else {
        say(&[MSG_REPO.to_owned()]);
        return EXIT_REJECT;
    };
    if config.test_switches.skip_quarantine {
        env.quarantine = None;
    }
    let repo = match ServerRepo::open_for_receive(&env, limits.repo_limits()) {
        Ok(r) => r,
        Err(e) => {
            log.diag("open", &e.to_string(), None, None, None);
            say(&[MSG_REPO.to_owned()]);
            return EXIT_REJECT;
        }
    };
    let trust = match TrustConfig::load(
        &config.signers_file,
        &config.ssh_keygen,
        &config.allowed_witness_principals,
        config.require_separated_witness,
        limits.ssh_keygen_timeout,
    ) {
        Ok(t) => t,
        Err(e) => {
            log.diag("config", &e.to_string(), None, None, None);
            say(&[MSG_CONFIG.to_owned()]);
            return EXIT_REJECT;
        }
    };
    match crate::fsck::effective(&config.git, repo.git_dir()) {
        Ok(true) => {}
        Ok(false) | Err(_) if mode == Mode::Enforce => {
            say(&[MSG_FSCK.to_owned()]);
            return EXIT_REJECT;
        }
        Ok(false) | Err(_) => log.diag("fsck", "fsck_not_effective", None, None, None),
    }
    let max_stdin = (limits.max_refs_per_push + 1) * MAX_STDIN_LINE;
    let mut input = String::new();
    if std::io::stdin()
        .lock()
        .take(max_stdin as u64)
        .read_to_string(&mut input)
        .is_err()
    {
        say(&["minds-witness: interne Vorprüfung fehlgeschlagen".to_owned()]);
        return EXIT_REJECT;
    }
    let updates = match parse_stdin(&input, limits.max_refs_per_push) {
        Ok(u) => u,
        Err(e) => {
            say(&[e.message()]);
            return EXIT_REJECT;
        }
    };
    let verifier = Verifier::new(&trust, &budget);
    if let Err(e) =
        Guard::new(&repo, &verifier, &budget, &trust, &limits, &config.project).check(&updates)
    {
        say(&[e.message()]);
        return EXIT_REJECT;
    }

    // Schritt 4: PushContext.
    let host = config
        .project
        .split_once('/')
        .map_or(config.project.as_str(), |(h, _)| h)
        .to_owned();
    let ctx = PushContext {
        repo: config.project.clone(),
        updates: updates.clone(),
        pusher: Pusher {
            id: env_or("GL_ID", "local"),
            username: env_or("GL_USERNAME", "local"),
        },
        server: ServerInfo {
            host,
            kind: ServerKind::Memory,
            time,
        },
    };

    // Schritt 5: pre_receive.
    let access = GitObjectAccess::new(&repo, &updates, &config.git, &budget, &trust, &limits);
    let mut inspector_config = InspectorConfig::new(&config.project);
    inspector_config.max_binding_bytes = limits.max_binding_bytes;
    inspector_config.max_seals_per_binding = limits.max_seals_per_binding;
    inspector_config.max_observations_bytes = limits.max_observations_bytes;
    let inspector = GitSealInspector::new(&access, &verifier, &budget, inspector_config);
    let panicking;
    let objects: &dyn ObjectAccess = match &config.test_switches.panic_in {
        Some(method) => {
            panicking = Panicking {
                inner: &access,
                method,
                text: config.test_switches.panic_text.clone().unwrap_or_default(),
                fired: Cell::new(false),
            };
            &panicking
        }
        None => &access,
    };
    let pre = pre_receive(&ctx, &config.policy, objects, &inspector);
    for e in &pre.events {
        log.event("pre", e);
    }
    for d in access.diagnostics() {
        log.diag("access", &d, None, None, None);
    }
    for d in inspector.diagnostics() {
        log.diag(
            "inspector",
            d.reason,
            Some(&d.ref_name),
            d.id.as_deref(),
            d.principal.as_deref(),
        );
    }

    // Schritt 6: Reject.
    if let Decision::Reject { message, .. } = &pre.decision {
        say(std::slice::from_ref(message));
        return EXIT_REJECT;
    }

    // Schritt 7: Quittungen, Warnungen.
    let post = match identity(&signer) {
        Some(identity) => {
            let mut store = DirReceiptStore::new(config.out_dir.clone());
            let post = post_receive(&pre.findings, &identity, &signer, &mut store);
            for e in &post.events {
                log.event("post", e);
            }
            Some(post)
        }
        None => Some(PostOutcome {
            stored: 0,
            skipped_existing: 0,
            failed: u32::try_from(pre.findings.len()).unwrap_or(u32::MAX),
            events: Vec::new(),
        }),
    };
    let lines = warn::lines(
        &pre,
        post.as_ref(),
        config.policy.coverage_threshold_percent,
        config.policy.max_commits_per_push,
    );
    say(&lines);
    0
}
