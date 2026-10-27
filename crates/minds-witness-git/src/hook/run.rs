//! Ablauf des Hook-Binarys (Spec S2 §11, Spec S3 §3). Unterbefehle:
//!
//! - `pre-receive <config>`: prüft (S2-Prüfkette), signiert **nie** und
//!   schreibt keinen Ref; nach `Accept` legt er den Übergabe-Datensatz an.
//! - `post-receive <config>`: bestätigt nur die Ref-Updates, die Git laut
//!   stdin ausgeführt hat, und nur für den Datensatz desselben
//!   receive-pack-Prozesses; erst dann Quittungen. Exit immer 0.
//! - `reconcile <config>`: verwaiste Sperren, offene Bestätigungen, Ablauf.
//! - `install <config>`: schreibt beide Hooks (mit `exec`).
//! - `stats <log.jsonl>`: Laufzeitmessung (Spec S3 §3.4).
//!
//! Auf stderr steht nur die feste Ablehnungsmeldung oder die
//! Warnungsausgabe aus [`super::warn`], nie ein Ereignis, eine Diagnose, ein
//! Fehlertext oder ein Panic-Payload. Ereignisse und Diagnosen gehen als
//! JSON-Zeilen nach `<state_dir>/log.jsonl`.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Instant;

use minds_git::quarantine::{ReceiveEnv, ServerRepo};
use minds_witness_core::{
    CommitId, CommitInfo, CommitRange, Decision, HashStr, LogEvent, Mode, ObjectAccess,
    ObjectError, PushContext, Pusher, SealBlob, ServerInfo, Signer, Timestamp, TrustSnapshot,
    WitnessIdentity, matches_pattern, pre_receive,
};
use serde::Serialize;

use super::config::HookConfig;
use super::pending::{Handoff, PENDING_SCHEMA, PendingRecord};
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
const MSG_USAGE: &str = "minds-witness: Aufruf: minds-witness-hook pre-receive|post-receive|reconcile|install <config> | stats <log>";
const MSG_HANDOFF: &str = "minds-witness: Warnung: Quittung kann nicht vorbereitet werden.";

/// Einstieg: `args[1]` ist der Unterbefehl, `args[2]` der Pfad der
/// Konfiguration (bzw. bei `stats` der Log-Datei). Ein Panic außerhalb des
/// Kerns endet im pre-receive mit dem festen Satz und Exit 1, im
/// post-receive und in `reconcile` mit Exit 0 (beide können einen Push
/// nicht mehr ablehnen).
pub fn main_with(args: &[String]) -> i32 {
    // Der Panic-Hook schreibt nichts. Panics im Kern meldet die
    // Warnungsausgabe als `Unknown`, alle anderen der feste Satz unten.
    std::panic::set_hook(Box::new(|_| {}));
    let (Some(sub), Some(path)) = (args.get(1), args.get(2)) else {
        say(&[MSG_USAGE.to_owned()]);
        return EXIT_REJECT;
    };
    let path = Path::new(path).to_owned();
    match sub.as_str() {
        "pre-receive" => match catch_unwind(AssertUnwindSafe(|| pre_receive_main(&path))) {
            Ok(code) => code,
            Err(_) => {
                say(&[MSG_INTERNAL.to_owned()]);
                EXIT_REJECT
            }
        },
        "post-receive" => {
            let _ = catch_unwind(AssertUnwindSafe(|| super::post::post_receive_main(&path)));
            0
        }
        "reconcile" => catch_unwind(AssertUnwindSafe(|| super::post::reconcile_main(&path)))
            .unwrap_or(EXIT_REJECT),
        "install" => super::post::install_main(&path),
        "stats" => super::stats::stats_main(&path),
        _ => {
            say(&[MSG_USAGE.to_owned()]);
            EXIT_REJECT
        }
    }
}

pub(crate) fn say(lines: &[String]) {
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

pub(crate) struct Log {
    pub(crate) lines: Vec<String>,
}

impl Log {
    pub(crate) fn event(&mut self, phase: &str, e: &LogEvent) {
        if let Ok(s) = serde_json::to_string(&LogLine {
            phase,
            body: LogBody::Event(e),
        }) {
            self.lines.push(s);
        }
    }

    pub(crate) fn diag(
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

    /// Laufzeitmessung eines pre-receive (Spec S3 §3.4): Zähler und Dauern,
    /// keine Pfade, keine Inhalte.
    fn push_stats(
        &mut self,
        budget: &Budget,
        started: Instant,
        guard: std::time::Duration,
        pre: std::time::Duration,
        updates: &[minds_witness_core::RefUpdate],
    ) {
        let times: Vec<u64> = budget
            .process_times()
            .iter()
            .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
            .collect();
        let line = serde_json::json!({
            "phase": "pre",
            "push_stats": {
                "deadline_used_permille": budget.deadline_used_permille(),
                "evidence_bytes": budget.bytes(),
                "guard_us": u64::try_from(guard.as_micros()).unwrap_or(u64::MAX),
                "pre_receive_us": u64::try_from(pre.as_micros()).unwrap_or(u64::MAX),
                "ref_updates": updates.len(),
                "ssh_keygen_processes": budget.processes(),
                "ssh_keygen_us": times,
                "total_us": u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            }
        });
        self.lines.push(line.to_string());
    }

    pub(crate) fn flush(&self, out: &Path) {
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

pub(crate) fn now() -> Option<Timestamp> {
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

/// Hash des laufenden Binarys (Feld `witness.binary`).
pub(crate) fn binary_hash() -> Option<HashStr> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(exe).ok()?;
    Some(HashStr::of_bytes(&bytes))
}

/// Zeugen-Identität aus dem im pre-receive festgehaltenen Binary-Hash: Die
/// Quittung nennt das Binary, das geprüft hat, auch wenn bis zur Ablage
/// ein anderes installiert wurde.
pub(crate) fn identity(signer: &Signer, binary: &HashStr) -> WitnessIdentity {
    WitnessIdentity {
        binary: binary.clone(),
        key: signer.public_key(),
    }
}

fn pre_receive_main(path: &Path) -> i32 {
    let started = Instant::now();
    // Konfiguration.
    let Ok(config) = HookConfig::load(path) else {
        say(&[MSG_CONFIG.to_owned()]);
        return EXIT_REJECT;
    };
    let mut log = Log { lines: Vec::new() };
    let code = run_with(&config, &mut log, started);
    log.flush(&config.state_dir);
    code
}

/// Der receive-pack-Prozess dieses Hooks (Push-Kennung, Spec S3 §3.1).
pub(crate) fn push_identity() -> (u32, Option<String>) {
    #[cfg(unix)]
    let pid = std::os::unix::process::parent_id();
    #[cfg(not(unix))]
    let pid = 0;
    (pid, crate::process::start_time(pid))
}

/// `GIT_DIR` des Hooks muss das konfigurierte Repository sein.
pub(crate) fn same_repository(config: &HookConfig, git_dir: &Path) -> bool {
    match (
        std::fs::canonicalize(git_dir),
        std::fs::canonicalize(&config.repository),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn run_with(config: &HookConfig, log: &mut Log, started: Instant) -> i32 {
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
    let _ = valid_from;
    let handoff = Handoff::new(&config.state_dir, &seed);
    // Erster Schritt der Übergabe: Altlasten derselben PID beiseite (sie
    // stammen von einem früheren, beendeten receive-pack).
    let (rp_pid, rp_start) = push_identity();
    // Ohne vollständiges Aussortieren könnte ein alter Datensatz derselben
    // PID neben dem neuen liegen; dann wird kein neuer geschrieben.
    let handoff_usable = match handoff.move_stale(rp_pid) {
        Ok(moved) => {
            for m in moved {
                log.diag("handoff", "pending_stale", None, Some(&m), None);
            }
            true
        }
        Err(_) if mode == Mode::Enforce => {
            say(&[MSG_INTERNAL.to_owned()]);
            return EXIT_REJECT;
        }
        Err(_) => {
            log.diag("handoff", "handoff_unavailable", None, None, None);
            false
        }
    };
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
    if !same_repository(config, &env.git_dir) {
        say(&[MSG_CONFIG.to_owned()]);
        return EXIT_REJECT;
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
    let fsck_time = budget.remaining().unwrap_or(limits.deadline);
    match crate::fsck::effective_within(&config.git, repo.git_dir(), fsck_time) {
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
    let guard_started = Instant::now();
    if let Err(e) =
        Guard::new(&repo, &verifier, &budget, &trust, &limits, &config.project).check(&updates)
    {
        say(&[e.message()]);
        return EXIT_REJECT;
    }
    let guard_time = guard_started.elapsed();

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
            kind: config.server_kind(),
            time: time.clone(),
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
    let pre_started = Instant::now();
    let pre = pre_receive(&ctx, &config.policy, objects, &inspector);
    let pre_time = pre_started.elapsed();
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
        log.push_stats(&budget, started, guard_time, pre_time, &updates);
        say(std::slice::from_ref(message));
        return EXIT_REJECT;
    }

    // Schritt 7 (Spec S3 §3.1): Übergabe-Datensatz statt Quittung. Der
    // Trust-Snapshot entsteht aus genau den Bytes, mit denen geprüft wurde.
    let snapshot = TrustSnapshot::new(
        config.allowed_witness_principals.clone(),
        &config.policy,
        &config.project,
        config.require_separated_witness,
        trust.signers_text(),
    );
    let snapshot_bytes = snapshot.to_bytes();
    let binary = binary_hash();
    let record = binary.map(|witness_binary| PendingRecord {
        created: time.as_str().to_owned(),
        findings: pre.findings.clone(),
        nonce: nonce(),
        rp_pid,
        rp_start,
        schema: PENDING_SCHEMA.to_owned(),
        trust: HashStr::of_bytes(&snapshot_bytes),
        trust_snapshot: String::from_utf8(snapshot_bytes).unwrap_or_default(),
        updates: updates.clone(),
        witness_binary,
    });
    let handoff_ok = match record {
        _ if !handoff_usable => false,
        None => {
            log.diag("handoff", "identity_unavailable", None, None, None);
            false
        }
        Some(record) => match handoff.write_pending(&record) {
            Ok(name) => {
                log.diag("handoff", "pending_written", None, Some(&name), None);
                true
            }
            Err(_) => {
                log.diag("handoff", "pending_failed", None, None, None);
                false
            }
        },
    };
    log.push_stats(&budget, started, guard_time, pre_time, &updates);
    if !handoff_ok && mode == Mode::Enforce {
        // Ohne Datensatz entstünde für ein angenommenes Update nie eine
        // Quittung: im Enforce-Modus fail-closed.
        say(&[MSG_INTERNAL.to_owned()]);
        return EXIT_REJECT;
    }

    // Schritt 8: Warnungen. Ein Panic hier endet im Observe-Modus nicht mit
    // einer Blockade (§10), im Enforce-Modus fail-closed.
    let after = catch_unwind(AssertUnwindSafe(|| {
        let evaluated: Vec<String> = updates
            .iter()
            .filter(|u| {
                !u.new.is_null()
                    && config
                        .policy
                        .evaluate_refs
                        .iter()
                        .any(|p| matches_pattern(p, &u.ref_name))
            })
            .map(|u| u.ref_name.clone())
            .collect();
        let mut lines = warn::lines(
            &pre,
            None,
            config.policy.coverage_threshold_percent,
            config.policy.max_commits_per_push,
            &evaluated,
        );
        if !handoff_ok {
            lines.push(MSG_HANDOFF.to_owned());
        }
        lines
    }));
    let lines = match after {
        Ok(lines) => lines,
        Err(_) if mode == Mode::Observe => vec![
            "minds-witness: Warnung: Prüfung nicht abgeschlossen (Unknown)".to_owned(),
            "minds-witness: Push wird von minds nicht blockiert. Nachweis unvollständig; Details in der Quittung.".to_owned(),
        ],
        Err(_) => {
            say(&[MSG_INTERNAL.to_owned()]);
            return EXIT_REJECT;
        }
    };
    say(&lines);
    0
}

fn nonce() -> String {
    let mut seed = Vec::new();
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    seed.extend_from_slice(&now.as_nanos().to_le_bytes());
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut buf = [0u8; 16];
        if f.read_exact(&mut buf).is_ok() {
            seed.extend_from_slice(&buf);
        }
    }
    blake3::hash(&seed).to_hex()[..32].to_owned()
}
