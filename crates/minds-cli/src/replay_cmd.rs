//! `minds replay` (EA-18b): In CI, auf dem Checkout eines Commits, die
//! entscheidenden Test- und Benchmark-Befehle seiner Sessions **sicher**
//! wiederholen, mit dem Bericht vergleichen und einen Replay-Record ablegen.
//!
//! Das ist die einzige Stelle, an der Minds aufgezeichnete Befehle ausführt.
//! Die Regeln, alle fail-closed:
//!
//! - **Keine Shell.** Das argv geht direkt an [`std::process::Command`] —
//!   EA-18a speichert nur argv aus einfachen Kommandos, hier wird nichts
//!   mehr zerlegt.
//! - **Allowlist aus der reviewten Policy.** `.minds/replay.json` wird aus
//!   dem **Baum des Commits** gelesen, nicht aus dem Arbeitsbaum (ein
//!   früherer Befehl könnte ihn ändern). Was sie nicht erlaubt, wird
//!   übersprungen und gemeldet, nie gestartet
//!   ([`minds_reader::replay::allows`]).
//! - **Geleerte Umgebung.** Nur `PATH` (nur absolute Einträge), `HOME`,
//!   `CARGO_*`, `RUSTUP_*` (ohne Zugangsdaten-Namen) und die Namen aus der
//!   Policy ([`child_env`]).
//! - **Arbeitsverzeichnis im Checkout.** Das repo-relative `cwd` der
//!   Session, lexikalisch geprüft und nach Auflösung aller Symlinks noch
//!   einmal ([`workdir`]); nie im Git-Verzeichnis.
//! - **Zeitlimits.** Je Befehl und für den Lauf; bei Ablauf wird die ganze
//!   Prozessgruppe beendet. Die Ausgabe wird nur bis kurz nach dem Ende
//!   gelesen — ein Prozess, der sich ablöst und die Pipe hält, blockiert
//!   den Lauf nicht; stimmen die Zahlen, ist er eine Lücke (`skipped`).
//!
//! # Sicherheitsannahme: Signiert wird nur reviewter Code
//!
//! Die Grenzen oben halten **das argv** im Zaum, nicht den Code, den es
//! startet: `cargo test` führt den Code des geprüften Commits aus (Tests,
//! `build.rs`), unter demselben Benutzer wie `minds`. Die geleerte
//! Umgebung ist Verteidigung in der Tiefe, keine Grenze gegen diesen Code —
//! er kann die Umgebung des CI-Jobs anderswo lesen (`/proc` der Shell, die
//! `minds` startete) und jede Datei des Benutzers, also auch den Schlüssel.
//! Daraus folgt:
//!
//! - **Signiert wird nur in Pipelines über reviewtem Code** (geschützter
//!   Branch, Schlüssel als geschützte Variable). Die Prüfungen hier sind
//!   Verteidigung in der Tiefe — die Grenze ist, wer den Schlüssel bekommt:
//!   GitLab eine **geschützte** Variable (nur für geschützte Refs), GitHub
//!   ein **Environment-Secret** mit Deployment-Regeln auf geschützte Refs
//!   (ein Repository-Secret bekäme jeder Workflow auf jedem Branch, ganz
//!   ohne `minds`). Ein Schlüssel und ein Principal je Projekt. Auch auf
//!   einem geschützten Ref sind Projekt-Variablen und Push-Optionen
//!   (`git push -o ci.variable=…`) keine reviewte Eingabe: Push auf den
//!   signierenden Branch nur per Merge. Signierende Läufe brauchen
//!   ephemere, projekteigene Runner — Cargo liest `.cargo/config.toml` auch
//!   aus Verzeichnissen über dem Checkout und aus `$HOME`. In einer Merge-Request-
//!   bzw. Pull-Request-Pipeline verweigert `minds replay` das Signieren
//!   ([`signing_key`]); dort läuft es mit `--unsigned` — angezeigt, nie für
//!   A3 gezählt.
//! - `ssh-keygen` wird **vor** dem ersten Befehl aufgelöst (absoluter Pfad),
//!   damit ein Test kein eigenes `ssh-keygen` in den `PATH` legen kann, das
//!   danach den Schlüssel bekäme.
//! - Unter Linux ist der Prozess nicht dumpbar ([`harden_process`]): Kinder
//!   desselben Benutzers lesen weder seine Umgebung noch seinen Speicher.
//!
//! Was entschieden wird, rechnet `minds_reader::replay` (W5); hier wird nur
//! ausgeführt und abgelegt. Die Prozesse startet ein [`Spawner`] — in Tests
//! ein Fake, der festhält, dass nichts Unerlaubtes je gestartet wurde.
//!
//! Exit-Codes: 0 alles reproduziert oder übersprungen, 2 mindestens ein
//! `claim not reproduced`, 4 operativer Fehler — 2 geht 4 vor.
//!
//! Nur unter Unix ([`NOT_SUPPORTED`] sonst): Die Ausgabe eines Laufs muss
//! ein Strom in Reihenfolge sein, und das Programm darf nur im gefilterten
//! `PATH` gesucht werden — beides gibt `std::process` unter Windows nicht her.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use minds_capture::ExecReport;
use minds_capture::exec_outcome::{interpret, recognize};
use minds_core::replay::{
    MAX_POLICY, POLICY_PATH, REPLAY_SCHEMA, ReplayEnvironment, ReplayPolicy, ReplayRecord,
    ReplayResult, ReplayVerdict,
};
use minds_core::{ExecClass, RedactionCounts, Session, SessionId};
use minds_reader::replay::{self, INTERPRETATION_VERSION, Run, Skip};
use minds_store::StoreError;

use crate::context::Context;
use crate::text::sanitize;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Die Umgebungsvariable mit dem Pfad des CI-Schlüssels (`minds-anchor`).
pub(crate) const KEY_ENV: &str = "MINDS_ANCHOR_KEY_FILE";

/// Das Präfix jeder Ausgabezeile.
const LABEL: &str = "replay   ";

/// So viel Ausgabe je Strom wird behalten; der Rest wird verworfen (aber
/// gelesen, damit der Prozess nicht an einer vollen Pipe hängt).
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// Davon der Anfang; der Rest ist das Ende des Stroms, wo die
/// Zusammenfassungen der Runner stehen.
const OUTPUT_HEAD: usize = 1024 * 1024;

/// So lange schläft die Warteschleife zwischen zwei Blicken auf den Prozess.
const POLL: Duration = Duration::from_millis(20);

/// So lange wird nach dem Ende eines Befehls noch auf seine Ausgabe
/// gewartet. Hält danach noch ein Prozess die Pipe, hat er sich abgelöst.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// Variablen, die ein Windows-Prozess zum Starten braucht (nur dort).
#[cfg(windows)]
const WINDOWS_BASE: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "USERPROFILE",
    "TEMP",
    "TMP",
    "PATHEXT",
];

/// Die Meldung außerhalb von Unix (siehe [`replay`]).
const NOT_SUPPORTED: &str = "minds replay is not supported on this platform — it needs Unix \
                             (one ordered output stream, program lookup only in the filtered PATH)";

/// Führt `minds replay` aus.
pub fn run(rev: Option<&str>, unsigned: bool) -> ExitCode {
    match replay(rev, unsigned) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("minds replay: {}", sanitize(&err.to_string()));
            ExitCode::from(4)
        }
    }
}

fn replay(rev: Option<&str>, unsigned: bool) -> Fallible<ExitCode> {
    // Nur Unix: Dort laufen stdout und stderr als ein Strom in ihrer
    // Reihenfolge (der Parser braucht cargos Kopfzeilen vor den Ergebnissen),
    // und `argv[0]` wird nur im gefilterten PATH gesucht. Unter Windows sucht
    // std zusätzlich im PATH des Elternprozesses — ein `cargo.exe` aus dem
    // Checkout liefe trotz Filter.
    if !cfg!(unix) {
        return Err(NOT_SUPPORTED.into());
    }
    // Unsigniert läuft unreviewter Code (Merge Request) — liegt der
    // Schlüssel trotzdem in der Umgebung, könnte er ihn lesen und danach
    // Records und Gegenzeichnungen (EA-19) fälschen. Nie zusammen.
    if unsigned && std::env::var_os(KEY_ENV).is_some_and(|value| !value.is_empty()) {
        return Err(format!(
            "refusing an unsigned replay while {KEY_ENV} is set — the replayed code could \
             read the anchor key; unset it in this job"
        )
        .into());
    }
    harden_process();
    let ctx = Context::open()?;
    let checkout = ctx
        .repo
        .workdir()
        .ok_or("replay needs a checkout, not a bare repository")?
        .to_path_buf();
    let head = ctx.repo.head()?.commit().ok_or("HEAD has no commit")?;
    // Der Schlüssel vor jedem Befehl: Ohne ihn soll nichts laufen, dessen
    // Ergebnis dann nicht signiert werden kann.
    let key = if unsigned {
        None
    } else {
        Some(signing_key(
            &|name| std::env::var(name).ok(),
            &checkout,
            &head.to_string(),
        )?)
    };
    let (commit, sessions) = crate::verify_cmd::sessions_of_revision(&ctx, rev.unwrap_or("HEAD"))?;
    if commit != head {
        return Err(format!(
            "replay runs on a checkout of the commit: HEAD is {}, not {}",
            &head.to_string()[..12],
            &commit.to_string()[..12]
        )
        .into());
    }
    let (policy, policy_blob) = load_policy(&ctx.repo, commit)?;
    // Auch die Redaction-Policy aus dem Commit, nicht aus dem Arbeitsbaum —
    // ein Befehl könnte ihn ändern. Und nie schwächer als der strenge
    // Default: Im Merge Request schreibt der Agent sie mit (dieselbe
    // Untergrenze wie beim Witness).
    let redaction =
        match ctx
            .repo
            .read_blob_bounded(commit, crate::config::REDACT_CONFIG, MAX_POLICY)?
        {
            Some((_, bytes)) => crate::config::parse_redaction(&bytes)?,
            None => minds_redact::RedactionConfig::default(),
        }
        .floored_at_default()
        .pipeline()?;
    let git_dirs: Vec<PathBuf> = [ctx.repo.git_dir(), ctx.repo.common_dir()]
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    let env = child_env(&policy, std::env::vars_os(), &checkout, &|value: &str| {
        clean(&redaction, value)
    });
    if !env.iter().any(|(name, _)| name == "PATH") {
        // Ohne PATH suchte libc in ihrem Default — alte glibc auch im cwd.
        return Err("no absolute PATH entry outside the checkout is left for the replay".into());
    }
    let inputs = Inputs {
        checkout: &checkout,
        policy: &policy,
        env,
        git_dirs,
        tracked: Some(tracked_dirs(&ctx.repo, commit)?),
        redaction: Some(&redaction),
    };
    let environment = ci_environment(&redaction, |name| std::env::var(name).ok());
    // Ein Projekt, das die Redaction nicht unverändert passiert, fehlt —
    // statt jeden Record (samt Befund) beim Ablegen zu kippen.
    let project =
        ci_project(|name| std::env::var(name).ok()).filter(|project| clean(&redaction, project));
    if key.is_some() && project.is_none() {
        // Ohne Projekt ließe sich ein hier signierter Record in einem
        // fremden Projekt ablegen (Ids sind in Forks gleich).
        return Err("refusing to sign: no CI project identity \
                    (CI_SERVER_HOST/CI_PROJECT_PATH or GITHUB_SERVER_URL/GITHUB_REPOSITORY)"
            .into());
    }
    let mut budget = Budget::new(Duration::from_secs(policy.timeouts.total_s));
    let mut spawner = System;

    // Signiert wird nur für Sessions, die der Commit selbst per Trailer
    // nennt (Teil des reviewten Commits). Eine Verknüpfung allein aus dem
    // Store-Index kann jeder mit Push-Recht auf `refs/minds/*` anlegen — und
    // damit wählen, welcher Code in der signierenden Pipeline läuft.
    let trailered: std::collections::BTreeSet<SessionId> =
        ctx.repo.session_ids_of(commit)?.into_iter().collect();
    // Alle Sessions vor dem ersten Befehl laden: Was danach im Store
    // geschieht, beeinflusst diesen Lauf nicht mehr. Eine, die nicht lädt,
    // fällt allein heraus (Exit 4 am Ende).
    let mut refused = false;
    let mut loaded = Vec::new();
    let mut index_only = 0;
    let mut forgotten = 0;
    let linked = sessions.len();
    // Auch unsigniert auf einem geschützten Ref: Dort liegen geschützte
    // Variablen in der Umgebung des Jobs.
    let protected = ["CI_COMMIT_REF_PROTECTED", "GITHUB_REF_PROTECTED"]
        .iter()
        .any(|name| std::env::var(name).as_deref() == Ok("true"));
    for (id, _) in sessions {
        if (key.is_some() || protected) && !trailered.contains(&id) {
            println!(
                "{LABEL}session {id} is linked only via the store index — not replayed in a \
                 signing run or on a protected ref"
            );
            index_only += 1;
            continue;
        }
        match ctx.store.get(id) {
            Ok(Some(session)) => loaded.push((id, session)),
            Ok(None) => {
                println!("{LABEL}session {id} is not in the store — nothing to replay");
                // Der Commit nennt sie selbst: nicht geholt oder entfernt —
                // ein Lauf, der sie still ausließe, wäre zu grün.
                if trailered.contains(&id) {
                    refused = true;
                }
            }
            Err(StoreError::Forgotten { .. }) => {
                println!("{LABEL}session {id} was forgotten — nothing to replay");
                // Ein Tombstone ist unsigniert: Nennt der Commit die Session
                // selbst, ist ihr Fehlen ein Befund, kein grüner Lauf.
                if trailered.contains(&id) {
                    refused = true;
                } else {
                    forgotten += 1;
                }
            }
            Err(err) => {
                eprintln!(
                    "minds replay: session {id} not replayed: {}",
                    sanitize(&err.to_string())
                );
                refused = true;
            }
        }
    }
    if loaded.is_empty() && linked > index_only + forgotten && !refused {
        // Verknüpft, aber nicht lesbar — etwa `refs/minds/*` nicht geholt.
        // Ein grüner Lauf ohne Wiederholung wäre irreführend.
        return Err("no linked session could be loaded — fetch refs/minds/* first".into());
    }
    let mut records = Vec::new();
    for (id, session) in &loaded {
        // Ein operativer Fehler trifft nur diese Session: Records der
        // übrigen — auch ein `claim not reproduced` — bleiben erhalten.
        let mut record = match replay_session(
            &inputs,
            *id,
            session,
            &commit.to_string(),
            &mut spawner,
            &mut budget,
        ) {
            Ok(record) => record,
            Err(err) => {
                eprintln!(
                    "minds replay: session {id} not replayed: {}",
                    sanitize(&err.to_string())
                );
                refused = true;
                continue;
            }
        };
        record.environment = environment.clone();
        record.policy = policy_blob.clone();
        record.project = project.clone();
        records.push(record);
    }

    // Der Exit-Status aus **allen** Records, vor der Prüfung: Ein Urteil ist
    // ein Wort aus eigenem Vokabular, kein Text aus der Session. Verwirft die
    // (womöglich strengere) Policy einen Record, bleibt sein `claim not
    // reproduced` trotzdem Exit 2.
    let status = exit_status(&records);
    // Erst prüfen, dann zeigen: Was die Redaction nicht ablegen ließe,
    // erscheint auch nicht im (womöglich öffentlichen) CI-Log. Ein
    // Record, der nicht durchgeht, fällt allein heraus — die übrigen
    // Sessions bekommen ihren; der Lauf endet dann mit 4 (oder 2).
    let mut scanned = Vec::with_capacity(records.len());
    for record in records {
        let session = record.session.clone();
        match redaction.scan_replay(record) {
            Ok(record) => scanned.push(record),
            Err(err) => {
                eprintln!(
                    "minds replay: record for session {} not stored: {err}",
                    sanitize(&session)
                );
                refused = true;
            }
        }
    }
    let records: Vec<ReplayRecord> = scanned.iter().map(|r| r.record().clone()).collect();
    for line in summary_lines(&records) {
        println!("{line}");
    }
    // Jeden Record signieren, bevor einer abgelegt wird: Scheitert eine
    // Signatur, liegt nichts. (Scheitert das Ablegen selbst mittendrin,
    // können die schon geschriebenen liegen bleiben — jeder ist für sich
    // vollständig.)
    // Ein `claim not reproduced` geht jedem operativen Fehler vor — auch
    // einem beim Signieren oder Ablegen: Wer Exit 4 als Infrastruktur-
    // Rauschen toleriert, soll keinen Befund übersehen (der Fehler steht
    // auf stderr).
    let stored = (|| -> Fallible<()> {
        let mut prepared = Vec::with_capacity(scanned.len());
        for record in &scanned {
            let text = String::from_utf8(record.record().canonical_bytes()?)?;
            let signature = key
                .as_ref()
                .map(|key| {
                    minds_attest::ssh_sign_ns_with(
                        &key.program,
                        &text,
                        &key.file,
                        minds_attest::NS_ANCHOR,
                    )
                })
                .transpose()?;
            prepared.push((record, signature));
        }
        for (record, signature) in prepared {
            let id = ctx.store.put_replay(record)?;
            if let Some(signature) = &signature {
                ctx.store.put_replay_signature(&id, signature)?;
            }
            println!(
                "{LABEL}record {id} ({})",
                if signature.is_some() {
                    "signed, minds-anchor"
                } else {
                    "unsigned"
                }
            );
        }
        Ok(())
    })();
    if let Err(err) = stored {
        eprintln!("minds replay: {}", sanitize(&err.to_string()));
        refused = true;
    }
    if refused && status == 0 {
        return Ok(ExitCode::from(4));
    }
    Ok(ExitCode::from(status))
}

/// Die Verzeichnisse, die der Commit trägt (`.` und jeder Vorfahr einer
/// getrackten Datei) — nur dort darf ein Befehl laufen. Ein Verzeichnis
/// aus einem CI-Cache (`target/`, `node_modules/`) enthält Code, den
/// niemand reviewt hat.
fn tracked_dirs(
    repo: &minds_git::Repo,
    commit: minds_git::CommitId,
) -> Fallible<std::collections::BTreeSet<String>> {
    let mut dirs = std::collections::BTreeSet::from([".".to_owned()]);
    for path in repo.list_blobs(repo.tree_of(commit)?)? {
        let mut parent = path.as_str();
        while let Some((dir, _)) = parent.rsplit_once('/') {
            if !dirs.insert(dir.to_owned()) {
                break;
            }
            parent = dir;
        }
    }
    Ok(dirs)
}

/// Der Signaturschlüssel und das `ssh-keygen`, das ihn benutzen darf.
struct SigningKey {
    /// Die Schlüsseldatei aus [`KEY_ENV`].
    file: PathBuf,
    /// `ssh-keygen`, aufgelöst **bevor** irgendein Befehl lief.
    program: PathBuf,
}

/// Der Schlüssel aus [`KEY_ENV`] — nur außerhalb von Merge-/Pull-Request-
/// Pipelines (siehe Modul-Doku, „Sicherheitsannahme"), vorhanden, und ein
/// `ssh-keygen` mit `-Y` unter einem absoluten Pfad.
fn signing_key(
    var: &dyn Fn(&str) -> Option<String>,
    checkout: &Path,
    head: &str,
) -> Fallible<SigningKey> {
    if review_pipeline(var) {
        return Err(
            "refusing to sign in a merge/pull request pipeline — the replayed code \
                    is not reviewed yet and could read the key; run `minds replay --unsigned` \
                    here and sign in a pipeline on a protected branch"
                .into(),
        );
    }
    let file = var(KEY_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("{KEY_ENV} is not set — pass --unsigned for an unsigned replay record")
        })?;
    // Positiv belegt statt nur „kein Merge Request": Signiert wird nur auf
    // einem geschützten Ref — GitLab `CI_COMMIT_REF_PROTECTED`, GitHub
    // `GITHUB_REF_PROTECTED`. Überall sonst (jeder Branch, lokal) unsigniert.
    // Die Variablen genau einer erkannten Plattform — sonst überschriebe
    // etwa ein im Job gesetztes `CI_COMMIT_SHA` auf GitHub die echte SHA.
    let gitlab = var("GITLAB_CI").as_deref() == Some("true");
    let github = var("GITHUB_ACTIONS").as_deref() == Some("true");
    let (protected_var, sha_var) = match (gitlab, github) {
        (true, false) => ("CI_COMMIT_REF_PROTECTED", "CI_COMMIT_SHA"),
        (false, true) => ("GITHUB_REF_PROTECTED", "GITHUB_SHA"),
        _ => {
            return Err(
                "refusing to sign: not on a protected ref of exactly one recognised CI \
                 (GITLAB_CI / GITHUB_ACTIONS) — pass --unsigned"
                    .into(),
            );
        }
    };
    if var(protected_var).as_deref() != Some("true") {
        return Err(
            "refusing to sign: this pipeline does not run on a protected ref \
                    (CI_COMMIT_REF_PROTECTED / GITHUB_REF_PROTECTED) — pass --unsigned"
                .into(),
        );
    }
    // Und der Checkout ist genau der Commit dieses Refs: Ein Job im Kontext
    // des geschützten Branches, der einen anderen Commit auscheckt (der
    // „pwn request" über `issue_comment`), signiert nicht.
    if var(sha_var).as_deref() != Some(head) {
        return Err(
            "refusing to sign: HEAD is not the commit of the protected ref \
                    (CI_COMMIT_SHA / GITHUB_SHA) — pass --unsigned"
                .into(),
        );
    }
    // Nur Auslöser, deren Checkout der Ref selbst ist und die keine
    // Variablen eines Aufrufers mitbringen — eine Allowlist, fehlend heißt
    // nein. GitLab `web`/`api`/`trigger` setzen Variablen, die bis in die
    // Umgebung der Befehle reisen (`CARGO_*_RUNNER`).
    let (source_var, allowed): (&str, &[&str]) = if gitlab {
        // Auch Schedules bringen eigene Variablen mit.
        ("CI_PIPELINE_SOURCE", &["push"])
    } else {
        (
            "GITHUB_EVENT_NAME",
            &["push", "workflow_dispatch", "schedule"],
        )
    };
    match var(source_var) {
        Some(source) if allowed.contains(&source.as_str()) => {}
        source => {
            return Err(format!(
                "refusing to sign for the pipeline source {} — pass --unsigned",
                sanitize(source.as_deref().unwrap_or("(unset)"))
            )
            .into());
        }
    }
    let real = std::fs::canonicalize(&file)
        .map_err(|_| format!("{KEY_ENV} does not name a readable key file"))?;
    if !real.is_file() {
        return Err(format!("{KEY_ENV} does not name a readable key file").into());
    }
    let checkout_real = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    if real.starts_with(&checkout_real) {
        return Err(format!("{KEY_ENV} points into the checkout — refusing to sign").into());
    }
    let program = var("PATH")
        .as_deref()
        .and_then(|path| find_ssh_keygen(path, checkout))
        .ok_or("ssh-keygen was not found under an absolute PATH entry outside the checkout")?;
    // Geprüft wird genau das Programm, das später signiert.
    if !minds_attest::ssh_keygen_available_at(&program) {
        return Err("ssh-keygen with -Y sign is not available".into());
    }
    // Und es signiert mit diesem Schlüssel wirklich (Passphrase, Rechte der
    // Datei) — bevor ein Befehl läuft, dessen Ergebnis sonst nicht signiert
    // werden könnte.
    minds_attest::ssh_sign_ns_with(
        &program,
        "minds-replay-probe\n",
        &real,
        minds_attest::NS_ANCHOR,
    )
    .map_err(|err| {
        // Die Meldung von `ssh-keygen` kann den Pfad nennen — ersetzt.
        let spellings = crate::anchor_cmd::spellings(&file, &real);
        format!(
            "the anchor key cannot sign: {}",
            crate::anchor_cmd::scrub(&err.to_string(), &spellings)
        )
    })?;
    // Signiert wird mit dem aufgelösten Pfad — kein Symlink dazwischen.
    Ok(SigningKey {
        file: real,
        program,
    })
}

/// Läuft diese Pipeline über Code, den noch niemand reviewt hat (Merge-
/// oder Pull-Request)? Verteidigung in der Tiefe: Im Merge Request ist auch
/// die CI-Konfiguration fremd und kann diese Variablen löschen — die
/// eigentliche Grenze ist der Schlüssel als **geschützte** Variable.
pub(crate) fn review_pipeline(var: &dyn Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    set("CI_MERGE_REQUEST_IID")
        || set("CI_EXTERNAL_PULL_REQUEST_IID")
        || matches!(
            var("CI_PIPELINE_SOURCE").as_deref(),
            Some(
                "merge_request_event"
                    | "external_pull_request_event"
                    // Vom Aufrufer gesetzte Variablen (Trigger, API,
                    // Downstream) sind Laufzeit-Eingabe, keine reviewte
                    // Konfiguration — und reisen bis in die Umgebung der
                    // Befehle (`CARGO_*`).
                    | "trigger"
                    | "api"
                    | "pipeline"
                    | "parent_pipeline"
            )
        )
        || matches!(
            var("GITHUB_EVENT_NAME").as_deref(),
            Some(
                "pull_request"
                    | "pull_request_target"
                    | "pull_request_review"
                    | "pull_request_review_comment"
                    // Basis-Kontext mit Secrets, oft über dem Code des PR.
                    | "workflow_run"
            )
        )
}

/// Das erste `ssh-keygen` unter einem absoluten `PATH`-Eintrag außerhalb
/// des Checkouts, kanonisch.
pub(crate) fn find_ssh_keygen(path: &str, checkout: &Path) -> Option<PathBuf> {
    find_program("ssh-keygen", path, checkout)
}

/// Das erste Programm `name` (unter Windows `name.exe`) unter einem
/// absoluten `PATH`-Eintrag außerhalb des Checkouts, kanonisch.
pub(crate) fn find_program(name: &str, path: &str, checkout: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let checkout_real = std::fs::canonicalize(checkout).ok();
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&name))
        .filter(|candidate| candidate.is_file())
        .filter_map(|found| std::fs::canonicalize(found).ok())
        .find(|real| {
            !real.starts_with(checkout)
                && !checkout_real
                    .as_deref()
                    .is_some_and(|c| real.starts_with(c))
        })
}

/// Unter Linux: Der Prozess wird nicht dumpbar — Kinder desselben
/// Benutzers lesen dann weder `/proc/<pid>/environ` noch seinen Speicher.
/// Verteidigung in der Tiefe, siehe Modul-Doku.
pub(crate) fn harden_process() {
    #[cfg(target_os = "linux")]
    // SAFETY: `prctl(PR_SET_DUMPABLE, 0)` ändert nur ein Attribut dieses
    // Prozesses; die übrigen Argumente werden ignoriert.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

/// Die Policy aus dem Baum von `commit` samt ihrer Blob-Id — fehlt sie, ist
/// nichts erlaubt; ist sie kaputt, ist das ein Abbruch (nie „keine").
fn load_policy(
    repo: &minds_git::Repo,
    commit: minds_git::CommitId,
) -> Fallible<(ReplayPolicy, Option<String>)> {
    match repo.read_blob_bounded(commit, POLICY_PATH, MAX_POLICY)? {
        Some((blob, bytes)) => Ok((ReplayPolicy::parse(&bytes)?, Some(blob))),
        None => {
            println!("{LABEL}no {POLICY_PATH} in this commit — nothing is allowlisted");
            Ok((ReplayPolicy::none(), None))
        }
    }
}

/// 0 alles reproduziert oder übersprungen, 2 ein `claim not reproduced`.
fn exit_status(records: &[ReplayRecord]) -> u8 {
    let mismatch = records
        .iter()
        .flat_map(|r| &r.results)
        .any(|r| r.verdict == ReplayVerdict::NotReproduced);
    if mismatch { 2 } else { 0 }
}

// ---------------------------------------------------------------------------
// Ein Lauf über eine Session
// ---------------------------------------------------------------------------

/// Was für jeden Befehl gleich ist.
struct Inputs<'a> {
    /// Die Wurzel des Checkouts.
    checkout: &'a Path,
    /// Die Policy aus dem Commit.
    policy: &'a ReplayPolicy,
    /// Die Umgebung jedes Prozesses ([`child_env`]).
    env: Vec<(String, String)>,
    /// Das Git-Verzeichnis (und das geteilte), kanonisch — nie ein cwd.
    git_dirs: Vec<PathBuf>,
    /// Die Verzeichnisse, die der Commit trägt ([`tracked_dirs`]) — nur
    /// dort läuft ein Befehl. `None` nur in Tests ohne Repository.
    tracked: Option<std::collections::BTreeSet<String>>,
    /// Die Redaction: Ein argv, das sie nicht unverändert passiert, wird
    /// gar nicht erst ausgeführt (sonst stünde ein Geheimnis in der
    /// Prozesstabelle). `None` nur in Tests.
    redaction: Option<&'a minds_redact::RedactionPipeline>,
}

/// Das Zeitbudget des ganzen Laufs.
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn new(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
        }
    }

    /// Die verbleibende Zeit — `None`, wenn sie aufgebraucht ist.
    fn remaining(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    }
}

/// Wiederholt die entscheidenden Befehle einer Session und baut ihren
/// Record (ohne `environment`). Ein Programm, das nicht startet, macht nur
/// seinen Befehl zu `skipped`; jeder andere Fehler beim Ausführen ist
/// operativ (Exit 4).
fn replay_session(
    inputs: &Inputs<'_>,
    id: SessionId,
    session: &Session,
    commit: &str,
    spawner: &mut dyn Spawner,
    budget: &mut Budget,
) -> std::io::Result<ReplayRecord> {
    let checkout = std::fs::canonicalize(inputs.checkout)?;
    let per_command = Duration::from_secs(inputs.policy.timeouts.per_command_s);
    let mut results = Vec::new();
    for d in replay::decisive(session) {
        let argv = &d.outcome.command;
        let result = |verdict, observed, reason: Option<String>| ReplayResult {
            turn: d.turn,
            call: d.call,
            argv: argv.clone(),
            expected: replay::expected(d.outcome),
            observed,
            verdict,
            reason,
        };
        let skip = |why: Skip| result(ReplayVerdict::Skipped, None, Some(why.as_str().into()));

        // Texte, die die Redaction nicht passieren (argv, Runner,
        // Bench-Namen und -Einheiten): Der Befehl wird weder ausgeführt noch
        // in den Record aufgenommen. Stünde er dort, lehnte `scan_replay`
        // den ganzen Record ab — und mit ihm ein `claim not reproduced`
        // anderer Befehle derselben Session. Fehlt er, deckt der Record die
        // Session nicht ab (`replay covers n-1 of n`): kein A3, fail-closed.
        if let Some(redaction) = inputs.redaction
            && !std::iter::once(argv.join(" "))
                .chain(argv.iter().cloned())
                .chain(std::iter::once(d.outcome.runner.clone()))
                .chain(d.outcome.benches.iter().map(|b| b.name.clone()))
                .chain(d.outcome.benches.iter().map(|b| b.unit.clone()))
                .all(|text| clean(redaction, &text))
        {
            eprintln!(
                "minds replay: turn {} call {} not replayed: {}",
                d.turn,
                d.call,
                Skip::Redactable.as_str()
            );
            continue;
        }
        if !replay::allows(inputs.policy, argv) {
            results.push(skip(Skip::NotAllowlisted));
            continue;
        }
        // Derselbe Runner wie beim Checkpoint — sonst deutete derselbe
        // Parser eine andere Ausgabe.
        let Some(runner) = recognize(argv)
            .filter(|r| r.as_str() == d.outcome.runner && r.class() == d.outcome.class)
        else {
            results.push(skip(Skip::RunnerMismatch));
            continue;
        };
        let dir = match workdir(
            &checkout,
            &inputs.git_dirs,
            inputs.tracked.as_ref(),
            d.outcome.cwd.as_deref(),
        ) {
            Ok(dir) => dir,
            Err(why) => {
                results.push(skip(why));
                continue;
            }
        };
        let Some(left) = budget.remaining() else {
            results.push(skip(Skip::TotalTimeout));
            continue;
        };
        // Kürzt das Restbudget das Zeitlimit, ist ein Abbruch keine Aussage
        // über die Behauptung — dann übersprungen, nie `not reproduced`.
        let cut_by_budget = left < per_command;
        // Ein Programm, das nicht startet (nicht installiert), ist eine
        // Lücke dieses Befehls — kein Abbruch des ganzen Laufs, den sonst ein
        // untergeschobener Befehl für alle Sessions verhindern könnte.
        let ran = match spawner.spawn(&Invocation {
            argv,
            cwd: &dir,
            env: &inputs.env,
            timeout: per_command.min(left),
        }) {
            Ok(ran) => ran,
            // Jeder Fehler beim Starten oder Warten trifft nur diesen Befehl
            // (der Spawner hat seine Prozesse dann beendet).
            Err(_) => {
                results.push(skip(Skip::NotStarted));
                continue;
            }
        };
        if ran.timed_out && cut_by_budget {
            results.push(skip(Skip::TotalTimeout));
            continue;
        }
        let parsed = interpret(
            runner,
            argv.clone(),
            &ExecReport {
                output: ran.output,
                exit_code: ran.exit_code,
            },
        );
        let observed = replay::observed(
            d.outcome,
            &Run {
                exit_code: ran.exit_code,
                parsed,
                timed_out: ran.timed_out,
            },
        );
        // Lücken dieses Laufs, keine Belege gegen die Behauptung:
        // - Ein abgeschnittener Mittelteil kann ganze Testbinaries aus der
        //   Summe nehmen — er stuft aber nur einen Zähl-Befund herab; ein
        //   falscher Exit-Code, ein Timeout, ein Signal hängen nicht an der
        //   Ausgabe und bleiben `claim not reproduced`.
        // - Ein Prozess, der die Ausgabe hält, macht nur einen bestätigten
        //   Lauf zur Lücke; ein klarer Fehlschlag bleibt einer.
        let (verdict, reason) = replay::compare(d.outcome, &observed, inputs.policy);
        let exit_consistent = !observed.timed_out
            && match (d.outcome.exit_code, observed.exit_code) {
                (_, None) => false,
                (Some(recorded), Some(seen)) => recorded == seen,
                (None, Some(seen)) => seen == 0 || d.outcome.tests.is_some_and(|t| t.failed > 0),
            };
        let gap = if ran.truncated && exit_consistent {
            Some(Skip::OutputTruncated)
        } else if ran.lingering && verdict == ReplayVerdict::Reproduced {
            Some(Skip::ProcessLingered)
        } else {
            None
        };
        if let Some(gap) = gap {
            results.push(result(
                ReplayVerdict::Skipped,
                Some(observed),
                Some(gap.as_str().into()),
            ));
            continue;
        }
        results.push(result(verdict, Some(observed), reason));
    }
    Ok(ReplayRecord {
        kind: minds_core::replay::REPLAY_KIND.into(),
        policy: None,
        project: None,
        schema: REPLAY_SCHEMA,
        commit: commit.to_owned(),
        session: id.to_string(),
        interpretation_version: INTERPRETATION_VERSION,
        results,
        environment: ReplayEnvironment::default(),
    })
}

/// Das Arbeitsverzeichnis im (kanonischen) Checkout: erst lexikalisch
/// ([`replay::confine_cwd`]), dann nach Auflösung aller Symlinks noch einmal
/// unter der Wurzel — ein Symlink `crates/x -> /` im Commit führt nicht
/// hinaus. Nie im Git-Verzeichnis, auch nicht über einen Symlink
/// (`tools -> .git`): Geprüft wird auch der aufgelöste Ort, gegen jede
/// `.git`-Komponente und gegen die kanonischen `git_dirs`.
fn workdir(
    checkout: &Path,
    git_dirs: &[PathBuf],
    tracked: Option<&std::collections::BTreeSet<String>>,
    cwd: Option<&str>,
) -> Result<PathBuf, Skip> {
    let relative = replay::confine_cwd(cwd)?;
    // Nur ein Verzeichnis, das der Commit trägt: Was ein CI-Cache oder ein
    // früherer Lauf hinterließ, hat niemand reviewt.
    if let (Some(tracked), Some(cwd)) = (tracked, cwd)
        && !tracked.contains(cwd)
    {
        return Err(Skip::CwdNotTracked);
    }
    if relative
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err(Skip::CwdOutsideCheckout);
    }
    let real = std::fs::canonicalize(checkout.join(&relative)).map_err(|_| Skip::CwdMissing)?;
    let inside = real
        .strip_prefix(checkout)
        .map_err(|_| Skip::CwdOutsideCheckout)?;
    if inside
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
        || git_dirs.iter().any(|dir| real.starts_with(dir))
    {
        return Err(Skip::CwdOutsideCheckout);
    }
    // Auch der aufgelöste Ort muss ein getracktes Verzeichnis sein — ein
    // Symlink, den ein früherer Befehl anlegte, führt nicht in `target/`.
    if let Some(tracked) = tracked {
        let parts: Option<Vec<&str>> = inside
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect();
        let key = match parts {
            Some(parts) if parts.is_empty() => ".".to_owned(),
            Some(parts) => parts.join("/"),
            None => return Err(Skip::CwdNotTracked),
        };
        if !tracked.contains(&key) {
            return Err(Skip::CwdNotTracked);
        }
    }
    if !real.is_dir() {
        return Err(Skip::CwdMissing);
    }
    Ok(real)
}

// ---------------------------------------------------------------------------
// Umgebung
// ---------------------------------------------------------------------------

/// Die Umgebung eines Replay-Prozesses: `PATH` (nur absolute Einträge
/// außerhalb des Checkouts — sonst fände `argv[0]` ein `cargo` aus dem
/// Commit), `HOME`, `CARGO_*` und `RUSTUP_*` und die Namen aus der Policy,
/// exakt; unter Windows dazu die Variablen, ohne die kein Prozess startet.
/// Nie: Zugangsdaten-, CI-, SSH- und Minds-Variablen
/// ([`minds_core::replay::sensitive_env_name`]) — auch nicht, wenn die
/// Policy sie nennt —, und kein Wert mit Zugangsdaten in einer URL
/// (`https://user:pw@…`). Alles andere fehlt. Sortiert.
fn child_env(
    policy: &ReplayPolicy,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    checkout: &Path,
    clean_value: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let checkout_real = std::fs::canonicalize(checkout).ok();
    let mut out: Vec<(String, String)> = vars
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter_map(|(name, value)| {
            // Unter Unix heißt sie genau `PATH` (`path` sähe das Kind nie).
            if name == "PATH" || (cfg!(windows) && name.eq_ignore_ascii_case("PATH")) {
                let roots = std::iter::once(checkout).chain(checkout_real.as_deref());
                let kept = path_entries(&value, &roots.collect::<Vec<_>>())?;
                return Some((name, kept));
            }
            if minds_core::replay::sensitive_env_name(&name)
                || runs_code(&name)
                || url_with_userinfo(&value)
                || !clean_value(&value)
                || points_into(&value, checkout, checkout_real.as_deref())
                || relative_path(&name, &value)
            {
                return None;
            }
            let base = name == "HOME" || windows_base(&name);
            let listed = policy.env.contains(&name);
            let toolchain = name.starts_with("CARGO_") || name.starts_with("RUSTUP_");
            (base || listed || toolchain).then_some((name, value))
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Ist der Wert ein relativer Pfad in einer Pfad-Variable (`CARGO_HOME`,
/// `CARGO_TARGET_DIR`) — das Kind löste ihn gegen sein cwd auf, also im
/// Checkout — oder überhaupt ein Pfad in `RUSTUP_TOOLCHAIN`?
fn relative_path(name: &str, value: &str) -> bool {
    let relative = !Path::new(value).is_absolute();
    if name == "HOME" || name.ends_with("_HOME") || name.ends_with("_DIR") {
        return relative;
    }
    // `RUSTUP_TOOLCHAIN` ist ein Name (`stable`) — oder ein Pfad zu einer
    // eigenen Toolchain, also zu fremden `rustc`/`cargo`: nie, auch absolut.
    let _ = relative;
    name == "RUSTUP_TOOLCHAIN" && value.contains(['/', '\\'])
}

/// Zeigt ein (absoluter) Pfadwert in den Checkout? `CARGO_HOME`,
/// `RUSTUP_HOME`, `CARGO_TARGET_DIR` aus einem CI-Cache im Projekt brächten
/// Programme und Konfiguration mit, die niemand reviewt hat.
fn points_into(value: &str, checkout: &Path, checkout_real: Option<&Path>) -> bool {
    let path = Path::new(value);
    if !path.is_absolute() {
        return false;
    }
    let real = std::fs::canonicalize(path).ok();
    [Some(checkout), checkout_real]
        .into_iter()
        .flatten()
        .any(|root| path.starts_with(root) || real.as_deref().is_some_and(|r| r.starts_with(root)))
}

/// Startet die Variable Programme oder tauscht Quellen — `CARGO_TARGET_
/// <triple>_RUNNER`, `*_RUSTC_WRAPPER`, `*_LINKER`, `RUSTFLAGS`, eigene
/// Registries, ein anderer Toolchain-Server? Solche Werte kommen bei Bedarf
/// aus `.cargo/config.toml` im reviewten Commit, nie aus der Umgebung des
/// Jobs (Pipeline-Variablen sind Laufzeit-Eingabe).
fn runs_code(name: &str) -> bool {
    const PARTS: &[&str] = &[
        "RUNNER",
        "WRAPPER",
        "LINKER",
        "RUSTC",
        "RUSTFLAGS",
        "RUSTDOC",
    ];
    const PREFIXES: &[&str] = &[
        "CARGO_SOURCE_",
        "CARGO_REGISTRIES_",
        "CARGO_REGISTRY_",
        "CARGO_ALIAS_",
        "CARGO_UNSTABLE_",
    ];
    PARTS.iter().any(|part| name.contains(part))
        || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || (name.starts_with("CARGO_TARGET_") && name != "CARGO_TARGET_DIR")
        || matches!(
            name,
            "RUSTUP_DIST_SERVER"
                | "RUSTUP_DIST_ROOT"
                | "RUSTUP_UPDATE_ROOT"
                | "RUSTUP_OVERRIDE_UNIX_FALLBACK_SETTINGS"
        )
}

/// Gehört `name` zu den Variablen, ohne die ein Windows-Prozess nicht
/// startet? Außerhalb von Windows nie.
fn windows_base(name: &str) -> bool {
    #[cfg(windows)]
    return WINDOWS_BASE
        .iter()
        .any(|base| name.eq_ignore_ascii_case(base));
    #[cfg(not(windows))]
    {
        let _ = name;
        false
    }
}

/// Trägt `value` Zugangsdaten in URL-Form — `scheme://user:pw@host/…`
/// oder ohne Schema `user:pw@host:port` (curl-Proxy-Form)? Bewusst
/// großzügig (ein `/`, `?` oder `#` im Passwort zählt mit): Ein Wert zu
/// viel fehlt dem Prozess, ein Passwort zu viel erreichte ihn.
fn url_with_userinfo(value: &str) -> bool {
    value
        .split_whitespace()
        .any(|token| match token.find("://") {
            Some(at) => token[at + 3..].contains('@'),
            None => token
                .split_once('@')
                .is_some_and(|(user, _)| user.contains(':')),
        })
}

/// `PATH` ohne relative, leere und Einträge im Checkout — `None`, wenn
/// nichts bleibt.
fn path_entries(value: &str, checkout: &[&Path]) -> Option<String> {
    let kept: Vec<PathBuf> = std::env::split_paths(value)
        .filter(|entry| entry.is_absolute())
        .filter(|entry| {
            let real = std::fs::canonicalize(entry).ok();
            !checkout.iter().any(|root| {
                entry.starts_with(root) || real.as_deref().is_some_and(|r| r.starts_with(root))
            })
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    std::env::join_paths(kept).ok()?.into_string().ok()
}

/// Wo der Replay lief — nur eng geprüfte Werte: das CI-System, die
/// Pipeline-Id (nur Ziffern) und das Image (enger Zeichensatz, und nur,
/// wenn die Redaction nichts daran findet).
fn ci_environment(
    redaction: &minds_redact::RedactionPipeline,
    var: impl Fn(&str) -> Option<String>,
) -> ReplayEnvironment {
    let digits = |value: String| {
        (!value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit()))
            .then_some(value)
    };
    let image = |value: String| {
        // `user:pw@registry/…`: Ein `@` vor dem ersten `/` ist Userinfo,
        // kein Digest (`img:1.2@sha256:…` steht hinter dem Pfad).
        let host = value.split('/').next().unwrap_or_default();
        let shaped = !value.is_empty()
            && value.len() <= 256
            && !host.contains('@')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._/:@-".contains(&b));
        (shaped && clean(redaction, &value)).then_some(value)
    };
    if var("GITLAB_CI").as_deref() == Some("true") {
        ReplayEnvironment {
            ci: Some("gitlab".into()),
            pipeline: var("CI_PIPELINE_ID").and_then(digits),
            image: var("CI_JOB_IMAGE").and_then(image),
        }
    } else if var("GITHUB_ACTIONS").as_deref() == Some("true") {
        ReplayEnvironment {
            ci: Some("github".into()),
            pipeline: var("GITHUB_RUN_ID").and_then(digits),
            image: None,
        }
    } else {
        ReplayEnvironment::default()
    }
}

/// Das Projekt, in dessen Pipeline der Replay läuft — Host und Pfad, eng
/// geprüft (keine Userinfo). `None` außerhalb von GitLab/GitHub.
fn ci_project(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let project = if var("GITLAB_CI").as_deref() == Some("true") {
        format!("{}/{}", var("CI_SERVER_HOST")?, var("CI_PROJECT_PATH")?)
    } else if var("GITHUB_ACTIONS").as_deref() == Some("true") {
        let server = var("GITHUB_SERVER_URL")?;
        let host = server.split_once("://").map_or(server.as_str(), |(_, h)| h);
        format!(
            "{}/{}",
            host.trim_end_matches('/'),
            var("GITHUB_REPOSITORY")?
        )
    } else {
        return None;
    };
    let shaped = project.len() <= 256
        && !project.contains('@')
        && project
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:-".contains(&b));
    shaped.then_some(project)
}

/// Findet die Redaction nichts in `text`?
pub(crate) fn clean(redaction: &minds_redact::RedactionPipeline, text: &str) -> bool {
    let out = redaction.redact(text);
    out.counts == RedactionCounts::default() && out.invalid_findings == 0
}

// ---------------------------------------------------------------------------
// Ausgabe
// ---------------------------------------------------------------------------

/// Die Zusammenfassung über alle Records des Laufs:
///
/// ```text
/// replay   3/3 decisive test runs reproduced (cargo test -p sort …)
/// replay   claim not reproduced (tests: recorded 12 passed, observed 11 passed): cargo test
/// replay   1 skipped (not allowlisted): python bench.py
/// ```
fn summary_lines(records: &[ReplayRecord]) -> Vec<String> {
    let results: Vec<&ReplayResult> = records.iter().flat_map(|r| &r.results).collect();
    if results.is_empty() {
        return vec![format!("{LABEL}no decisive test or bench runs")];
    }
    let executed: Vec<&ReplayResult> = results
        .iter()
        .copied()
        .filter(|r| r.verdict != ReplayVerdict::Skipped)
        .collect();
    let mut lines = Vec::new();
    match executed.first() {
        None => lines.push(format!("{LABEL}no decisive run executed")),
        Some(first) => {
            let reproduced = executed
                .iter()
                .filter(|r| r.verdict == ReplayVerdict::Reproduced)
                .count();
            let tests = executed.iter().any(|r| r.expected.class == ExecClass::Test);
            let benches = executed
                .iter()
                .any(|r| r.expected.class == ExecClass::Bench);
            let kind = match (tests, benches) {
                (true, true) => "test and bench",
                (false, true) => "bench",
                _ => "test",
            };
            lines.push(format!(
                "{LABEL}{reproduced}/{} decisive {kind} runs reproduced ({}{})",
                executed.len(),
                shown(&first.argv),
                if executed.len() > 1 { " …" } else { "" }
            ));
        }
    }
    for r in executed
        .iter()
        .filter(|r| r.verdict == ReplayVerdict::NotReproduced)
    {
        // Wie die Zeile der übersprungenen: Grund in Klammern, dann argv.
        lines.push(format!(
            "{LABEL}claim not reproduced ({}): {}",
            sanitize(r.reason.as_deref().unwrap_or("differs")),
            shown(&r.argv)
        ));
    }
    let mut groups: Vec<(&str, Vec<&ReplayResult>)> = Vec::new();
    for r in results
        .iter()
        .filter(|r| r.verdict == ReplayVerdict::Skipped)
    {
        let reason = r.reason.as_deref().unwrap_or("skipped");
        match groups.iter_mut().find(|(g, _)| *g == reason) {
            Some((_, members)) => members.push(r),
            None => groups.push((reason, vec![r])),
        }
    }
    for (reason, members) in groups {
        lines.push(format!(
            "{LABEL}{} skipped ({}): {}{}",
            members.len(),
            sanitize(reason),
            shown(&members[0].argv),
            if members.len() > 1 { " …" } else { "" }
        ));
    }
    lines
}

/// Ein argv für das Terminal: entschärft, höchstens 80 Zeichen.
fn shown(argv: &[String]) -> String {
    let line = sanitize(&argv.join(" "));
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line
    }
}

// ---------------------------------------------------------------------------
// Prozesse
// ---------------------------------------------------------------------------

/// Ein auszuführender Befehl.
pub(crate) struct Invocation<'a> {
    /// Das argv, `argv[0]` der Programmname.
    pub argv: &'a [String],
    /// Das (geprüfte) Arbeitsverzeichnis.
    pub cwd: &'a Path,
    /// Die vollständige Umgebung.
    pub env: &'a [(String, String)],
    /// Das Zeitlimit.
    pub timeout: Duration,
}

/// Was ein Prozess ergab.
pub(crate) struct Ran {
    /// stdout und stderr als ein Strom, wie Claude Code sie aufzeichnet
    /// (Unix; sonst stdout, dann stderr).
    pub output: String,
    /// Der Exit-Code; `None` bei Signal oder Abbruch.
    pub exit_code: Option<i32>,
    /// Nach Ablauf des Zeitlimits abgebrochen.
    pub timed_out: bool,
    /// Nach dem Ende hielt noch ein (abgelöster) Prozess die Ausgabe offen;
    /// `output` ist dann, was bis dahin ankam.
    pub lingering: bool,
    /// Die Ausgabe überstieg [`MAX_OUTPUT`]; ihr Mittelteil fehlt.
    pub truncated: bool,
}

/// Startet Prozesse — in Tests ersetzt, um zu belegen, was **nicht**
/// gestartet wurde.
pub(crate) trait Spawner {
    /// Führt `invocation` aus und wartet auf ihr Ende.
    fn spawn(&mut self, invocation: &Invocation<'_>) -> std::io::Result<Ran>;
}

/// Der echte Spawner: `Command` ohne Shell, geleerte Umgebung, stdin zu,
/// eigene Prozessgruppe (Unix), Zeitlimit.
struct System;

impl Spawner for System {
    fn spawn(&mut self, invocation: &Invocation<'_>) -> std::io::Result<Ran> {
        let [program, args @ ..] = invocation.argv else {
            return Err(std::io::Error::other("empty argv"));
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(invocation.env.iter().map(|(k, v)| (k, v)))
            .current_dir(invocation.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Eine eigene Gruppe: Bei Ablauf trifft das Beenden auch die
            // Test-Binaries, die `cargo` gestartet hat.
            command.process_group(0);
            // Ein Strom wie `2>&1`: cargo schreibt die Kopfzeilen
            // (`Running …`) nach stderr, libtest die Ergebnisse nach stdout —
            // der Parser braucht beide in ihrer Reihenfolge, so wie Claude
            // Code sie aufzeichnet. Kein zweites Pipe-Paar im Elternprozess
            // (kein Fenster, in dem ein paralleler Fork es erbte): stderr
            // wird im Kind auf dessen stdout gelegt.
            command.stderr(Stdio::null());
            // SAFETY: Die Closure läuft im Kind zwischen fork und exec,
            // nachdem std stdin/stdout/stderr eingerichtet hat; sie ruft nur
            // `dup2`, das async-signal-safe ist, und alloziert nicht.
            unsafe {
                command.pre_exec(|| {
                    if libc::dup2(libc::STDOUT_FILENO, libc::STDERR_FILENO) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn()?;
        // Die Pipe-Enden des Elternprozesses gehören jetzt dem Kind.
        drop(command);
        let stdout = child.stdout.take().map(drain);
        let stderr = child.stderr.take().map(drain);
        let deadline = Instant::now() + invocation.timeout;
        let mut timed_out = false;
        let status = loop {
            let polled = match child.try_wait() {
                Ok(polled) => polled,
                Err(err) => {
                    // Kein Prozess läuft weiter, während signiert wird.
                    kill_tree(&mut child);
                    return Err(std::io::Error::other(err));
                }
            };
            if let Some(status) = polled {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                kill_tree(&mut child);
                break child.wait().map_err(std::io::Error::other)?;
            }
            std::thread::sleep(POLL);
        };
        // Auch nach regulärem Ende: Ein zurückgelassener Enkel der Gruppe
        // hielte die Pipes offen.
        #[cfg(unix)]
        kill_group(child.id());
        // Gelesen wird nur noch kurz: Ein Prozess, der sich abgelöst hat
        // (`setsid`, Windows ohne Gruppe), hielte die Pipe sonst ewig.
        let until = Instant::now() + PIPE_GRACE;
        let (mut output, mut lingering, mut truncated) =
            stdout.map(|d| d.collect(until)).unwrap_or_default();
        // Unix: stderr ist leer (im Kind auf stdout gelegt).
        if let Some((err, held, cut)) = stderr.map(|d| d.collect(until)) {
            output.push_str(&err);
            lingering |= held;
            truncated |= cut;
        }
        Ok(Ran {
            output,
            exit_code: if timed_out { None } else { status.code() },
            timed_out,
            lingering,
            truncated,
        })
    }
}

/// Ein Strom, den ein eigener Thread liest.
struct Drain {
    /// Was bisher ankam: die ersten [`OUTPUT_HEAD`] Bytes und die letzten
    /// bis [`MAX_OUTPUT`] — die Zusammenfassungen der Runner stehen am Ende.
    kept: std::sync::Arc<std::sync::Mutex<Kept>>,
    /// Meldet das Ende des Stroms (EOF oder Fehler).
    done: std::sync::mpsc::Receiver<()>,
}

/// Anfang und Ende eines Stroms.
#[derive(Default)]
struct Kept {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    dropped: bool,
}

impl Kept {
    fn push(&mut self, bytes: &[u8]) {
        let room = OUTPUT_HEAD.saturating_sub(self.head.len());
        let (head, rest) = bytes.split_at(bytes.len().min(room));
        self.head.extend_from_slice(head);
        self.tail.extend(rest);
        let limit = MAX_OUTPUT - OUTPUT_HEAD;
        if self.tail.len() > limit {
            let excess = self.tail.len() - limit;
            self.tail.drain(..excess);
            self.dropped = true;
        }
    }

    fn text(&self) -> String {
        let mut bytes = self.head.clone();
        if self.dropped {
            // Eine eigene Zeile: Der Parser liest Zeilen, keine halben.
            bytes.extend_from_slice(b"\n");
        }
        bytes.extend(self.tail.iter());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Drain {
    /// Wartet bis `until` auf das Ende des Stroms und gibt zurück, was
    /// ankam — und ob er da noch offen war (dann bleibt der Thread
    /// zurück, blockiert an einer Pipe, die ein abgelöster Prozess hält).
    fn collect(self, until: Instant) -> (String, bool, bool) {
        let wait = until.saturating_duration_since(Instant::now());
        let open = self.done.recv_timeout(wait).is_err();
        let (text, dropped) = self
            .kept
            .lock()
            .map(|kept| (kept.text(), kept.dropped))
            .unwrap_or_default();
        (text, open, dropped)
    }
}

/// Liest einen Strom in einem eigenen Thread; behalten werden Anfang und
/// Ende ([`Kept`]), der Rest wird gelesen und verworfen.
fn drain(mut stream: impl Read + Send + 'static) -> Drain {
    let kept = std::sync::Arc::new(std::sync::Mutex::new(Kept::default()));
    let (tx, done) = std::sync::mpsc::channel();
    let sink = std::sync::Arc::clone(&kept);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 64 * 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
                Ok(n) => {
                    if let Ok(mut kept) = sink.lock() {
                        kept.push(&buffer[..n]);
                    }
                }
            }
        }
        let _ = tx.send(());
    });
    Drain { kept, done }
}

/// Beendet den Prozess samt Gruppe (Unix) bzw. den Prozess (sonst).
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    kill_group(child.id());
    let _ = child.kill();
}

/// `SIGKILL` an die Prozessgruppe `pgid` (= pid des Gruppenführers). Eine
/// leere Gruppe (`ESRCH`) ist kein Fehler.
#[cfg(unix)]
fn kill_group(pgid: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(pgid)
        && pgid > 1
    {
        // SAFETY: `kill` liest nur seine beiden Ganzzahl-Argumente; eine
        // negative pid adressiert die Gruppe, die `process_group(0)` für
        // genau dieses Kind angelegt hat.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests;
