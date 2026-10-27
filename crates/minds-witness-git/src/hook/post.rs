//! post-receive, `reconcile` und `install` (Spec S3 §3.2, §3.3).
//!
//! post-receive kann einen Push nicht mehr ablehnen: Exit immer 0, Fehler
//! gehen ins Log und werden von `reconcile` nachgeholt. Quittungen entstehen
//! nur aus einer Bestätigung, also nur für Ref-Updates, die Git laut stdin
//! ausgeführt hat, und nur aus dem Datensatz desselben receive-pack-Prozesses.
//! Trust-Snapshot und Findings sind immer die des pre-receive, unverändert.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use minds_git::quarantine::ReceiveEnv;
use minds_witness_core::{RefUpdate, Signer, Timestamp, post_receive_with_trust};

use super::config::HookConfig;
use super::pending::{CONFIRMED_SCHEMA, Confirmation, Handoff};
use super::run::{Log, identity, now, push_identity, same_repository, say};
use crate::guard::{MAX_STDIN_LINE, parse_stdin};
use crate::receipt_store::{GitReceiptStore, MIN_ORPHAN_AGE};

/// post-receive: bestätigen, ablegen, eine begrenzte Zahl offener
/// Bestätigungen nachholen.
pub fn post_receive_main(path: &Path) {
    let Ok(config) = HookConfig::load(path) else {
        return;
    };
    let mut log = Log { lines: Vec::new() };
    post_with(&config, &mut log);
    log.flush(&config.state_dir);
}

struct Ctx<'a> {
    config: &'a HookConfig,
    handoff: Handoff,
    signer: Signer,
    store: GitReceiptStore,
}

fn context(config: &HookConfig) -> Option<Ctx<'_>> {
    let seed = config.seed().ok()?;
    let valid_from = Timestamp::parse(&config.key_valid_from).ok()?;
    Some(Ctx {
        config,
        handoff: Handoff::new(&config.state_dir, &seed),
        signer: Signer::from_seed(seed, valid_from),
        store: GitReceiptStore::new(
            &config.git,
            &config.repository,
            Duration::from_secs(config.store_timeout_secs),
        ),
    })
}

fn post_with(config: &HookConfig, log: &mut Log) {
    let Some(ctx) = context(config) else {
        log.diag("post", "configuration_unusable", None, None, None);
        return;
    };
    let Ok(env) = ReceiveEnv::from_process() else {
        log.diag("post", "environment_unusable", None, None, None);
        return;
    };
    if !same_repository(config, &env.git_dir) {
        log.diag("post", "repository_mismatch", None, None, None);
        return;
    }
    let limits = config.limits();
    let mut input = String::new();
    let max = (limits.max_refs_per_push + 1) * MAX_STDIN_LINE;
    if std::io::stdin()
        .lock()
        .take(max as u64)
        .read_to_string(&mut input)
        .is_err()
    {
        log.diag("post", "stdin_unreadable", None, None, None);
        return;
    }
    let Ok(performed) = parse_stdin(&input, limits.max_refs_per_push) else {
        log.diag("post", "stdin_unreadable", None, None, None);
        return;
    };

    let (rp_pid, rp_start) = push_identity();
    let candidates = match ctx.handoff.candidates(rp_pid, rp_start.as_deref()) {
        Ok(c) => c,
        Err(_) => {
            log.diag("post", "handoff_unavailable", None, None, None);
            return;
        }
    };
    // Nicht bestätigt: Ref-Updates sind ausgeführt, für sie entsteht aber
    // keine Quittung (kein Nachholen möglich, eigene Warnung).
    let mut unconfirmed = 0u32;
    let failed = match candidates.as_slice() {
        [] => {
            log.diag("post", "receipt_without_pending", None, None, None);
            0
        }
        [(name, record)] => {
            let confirmed: Vec<RefUpdate> = record
                .updates
                .iter()
                .filter(|u| performed.contains(u))
                .cloned()
                .collect();
            let confirmation = Confirmation {
                confirmed_at: now().map(|t| t.as_str().to_owned()).unwrap_or_default(),
                confirmed_updates: confirmed,
                pending: record.clone(),
                schema: CONFIRMED_SCHEMA.to_owned(),
            };
            match ctx.handoff.confirm(name, &confirmation) {
                Ok(true) => {
                    log.diag("post", "pending_confirmed", None, Some(name), None);
                    store_confirmation(&ctx, name, &confirmation, log)
                }
                Ok(false) => {
                    log.diag("post", "already_confirmed", None, Some(name), None);
                    0
                }
                Err(_) => {
                    log.diag("post", "confirm_failed", None, Some(name), None);
                    unconfirmed =
                        u32::try_from(confirmation.confirmed_findings().len()).unwrap_or(u32::MAX);
                    0
                }
            }
        }
        many => {
            for (name, _) in many {
                let _ = ctx.handoff.to_stale(name);
                log.diag("post", "pending_ambiguous", None, Some(name), None);
            }
            0
        }
    };
    if failed > 0 {
        say(&[format!(
            "minds-witness: Warnung: {failed} Quittung(en) nicht abgelegt; werden nachgeholt."
        )]);
    }
    if unconfirmed > 0 {
        say(&[format!(
            "minds-witness: Warnung: {unconfirmed} Quittung(en) nicht bestätigt; keine Quittung."
        )]);
    }
    let _ = reconcile_with(&ctx, log, Some(config.reconcile_batch));
}

/// Legt Snapshot und Quittungen einer Bestätigung ab. Vollständig abgelegt
/// → Bestätigung entfernt; sonst bleibt sie für `reconcile`. Liefert die
/// Zahl der nicht abgelegten Quittungen.
fn store_confirmation(ctx: &Ctx<'_>, name: &str, c: &Confirmation, log: &mut Log) -> u32 {
    let failed = store_once(ctx, name, c, log);
    if failed > 0 {
        // Hinten anstellen: Ein dauerhaft scheiternder Eintrag blockiert
        // die Charge nicht.
        let _ = ctx.handoff.defer(name);
    }
    failed
}

fn store_once(ctx: &Ctx<'_>, name: &str, c: &Confirmation, log: &mut Log) -> u32 {
    let findings = c.confirmed_findings();
    let total = u32::try_from(findings.len()).unwrap_or(u32::MAX);
    if findings.is_empty() {
        let _ = ctx.handoff.remove_confirmation(name);
        log.diag(
            "store",
            "confirmation_without_findings",
            None,
            Some(name),
            None,
        );
        return 0;
    }
    // Der Snapshot ist genau der des pre-receive; sein Hash muss der
    // gesicherte `trust`-Wert sein.
    match ctx.store.put_trust(c.pending.trust_snapshot.as_bytes()) {
        Ok((hash, _)) if hash == c.pending.trust => {}
        Ok(_) => {
            log.diag("store", "trust_mismatch", None, Some(name), None);
            return total;
        }
        Err(minds_witness_core::StoreError::Conflict(_)) => {
            log.diag("store", "trust_conflict", None, Some(name), None);
            return total;
        }
        Err(_) => {
            log.diag("store", "trust_store_failed", None, Some(name), None);
            return total;
        }
    }
    // S1 §8: Der Adapter prüft unmittelbar vor dem Signieren, ob der
    // Schlüssel jetzt signieren darf.
    let authorized = now().is_some_and(|t| *ctx.signer.valid_from() <= t);
    if !authorized {
        log.diag("store", "key_not_yet_valid", None, Some(name), None);
        return total;
    }
    let identity = identity(&ctx.signer, &c.pending.witness_binary);
    let mut store = ctx.store.clone();
    let outcome = post_receive_with_trust(
        &findings,
        &identity,
        &ctx.signer,
        &mut store,
        &c.pending.trust,
    );
    for e in &outcome.events {
        log.event("post", e);
    }
    if outcome.failed == 0 {
        let _ = ctx.handoff.remove_confirmation(name);
        log.diag("store", "confirmation_done", None, Some(name), None);
    }
    outcome.failed
}

/// Ergebnis eines `reconcile`-Laufs.
#[derive(Debug, Default)]
struct Reconciled {
    findings_left: u32,
    problems: u32,
}

fn reconcile_with(ctx: &Ctx<'_>, log: &mut Log, limit: Option<usize>) -> Reconciled {
    let mut out = Reconciled::default();
    match ctx.store.clear_stale_locks(MIN_ORPHAN_AGE) {
        Ok(report) => {
            if report.busy {
                log.diag("reconcile", "lock_recovery_busy", None, None, None);
            }
            for r in &report.removed {
                log.diag("reconcile", "stale_lock_removed", Some(r), None, None);
            }
            for r in &report.kept_unknown {
                log.diag("reconcile", "stale_lock_unknown", Some(r), None, None);
                out.problems += 1;
            }
            // Während eines laufenden Schreibers nicht als verwaist belegbar:
            // bleibt, wird aber als Problem gemeldet, solange es besteht.
            for r in &report.kept_busy {
                log.diag("reconcile", "stale_lock_unprovable", Some(r), None, None);
                out.problems += 1;
            }
        }
        Err(_) => {
            log.diag("reconcile", "lock_recovery_failed", None, None, None);
            out.problems += 1;
        }
    }
    match ctx.handoff.confirmations() {
        Ok((open, bad)) => {
            for name in &bad {
                log.diag("reconcile", "confirmation_invalid", None, Some(name), None);
                out.problems += 1;
            }
            for (name, c) in open.iter().take(limit.unwrap_or(usize::MAX)) {
                out.findings_left += store_confirmation(ctx, name, c, log);
            }
        }
        Err(_) => {
            log.diag("reconcile", "handoff_unavailable", None, None, None);
            out.problems += 1;
        }
    }
    match ctx
        .handoff
        .expire(Duration::from_secs(ctx.config.pending_ttl_secs))
    {
        Ok(expired) => {
            for e in &expired {
                log.diag("reconcile", "pending_expired", None, Some(e), None);
            }
        }
        Err(_) => out.problems += 1,
    }
    out
}

/// `reconcile`: für Betreiber oder Timer. Exit 0 ohne Befund, 2 bei
/// offenen Quittungen oder Befunden (unbekannte Sperren, ungültige
/// Bestätigungen), 1 bei ungültiger Konfiguration. Ausgabe: eine feste
/// Zusammenfassung auf stdout.
pub fn reconcile_main(path: &Path) -> i32 {
    let Ok(config) = HookConfig::load(path) else {
        say(&["minds-witness: Konfiguration ungültig".to_owned()]);
        return 1;
    };
    let Some(ctx) = context(&config) else {
        say(&["minds-witness: Konfiguration ungültig".to_owned()]);
        return 1;
    };
    let mut log = Log { lines: Vec::new() };
    let r = reconcile_with(&ctx, &mut log, None);
    log.flush(&config.state_dir);
    println!(
        "minds-witness reconcile: {} Quittung(en) offen, {} Befund(e); Details in log.jsonl",
        r.findings_left, r.problems
    );
    if r.findings_left > 0 || r.problems > 0 {
        2
    } else {
        0
    }
}

/// Markierung in den installierten Hooks.
pub const HOOK_MARKER: &str = "# minds-witness-hook";

/// `install`: schreibt `pre-receive` und `post-receive` des konfigurierten
/// Repositorys. Das Binary wird mit `exec` gestartet, damit sein
/// Elternprozess receive-pack ist (Push-Kennung, Spec S3 §3.1). Fremde
/// Hooks werden nie überschrieben.
pub fn install_main(path: &Path) -> i32 {
    let Ok(config) = HookConfig::load(path) else {
        say(&["minds-witness: Konfiguration ungültig".to_owned()]);
        return 1;
    };
    let (Ok(exe), Ok(cfg)) = (
        std::env::current_exe().and_then(std::fs::canonicalize),
        std::fs::canonicalize(path),
    ) else {
        say(&["minds-witness: Pfade nicht bestimmbar".to_owned()]);
        return 1;
    };
    let (exe, cfg) = (exe.to_string_lossy(), cfg.to_string_lossy());
    if exe.contains('\'') || cfg.contains('\'') {
        say(&["minds-witness: Pfade mit Apostroph werden nicht unterstützt".to_owned()]);
        return 1;
    }
    let hooks = config.repository.join("hooks");
    if std::fs::create_dir_all(&hooks).is_err() {
        say(&["minds-witness: Hook-Verzeichnis nicht beschreibbar".to_owned()]);
        return 1;
    }
    for sub in ["pre-receive", "post-receive"] {
        let file = hooks.join(sub);
        if let Ok(existing) = std::fs::read_to_string(&file)
            && !existing.contains(HOOK_MARKER)
        {
            say(&[format!(
                "minds-witness: fremder Hook {sub} vorhanden, nichts geändert"
            )]);
            return 1;
        }
        let body = format!("#!/bin/sh\n{HOOK_MARKER}\nexec '{exe}' {sub} '{cfg}'\n");
        if std::fs::write(&file, body).is_err() {
            say(&["minds-witness: Hook nicht schreibbar".to_owned()]);
            return 1;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755));
        }
    }
    println!("minds-witness: Hooks installiert");
    0
}
