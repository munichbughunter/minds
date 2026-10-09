//! `minds intent` — welche Anforderung, freigegeben von wem (EA-15).
//!
//! ```text
//! minds intent bind --file <pfad> [--scope <glob,glob>]
//! minds intent bind --issue <projekt#iid> [--scope <glob,glob>] [--gitlab-url <url>]
//!                   [--allow-confidential]
//! minds intent sign [<anchor-id>] [--key <pfad>] [--witness-home <dir>]
//! minds intent show [<anchor-id>]
//! minds intent list
//! ```
//!
//! # Binden
//!
//! `bind` baut aus einer Datei im Repository einen Intent-Anker (EA-14):
//! Quelle `file:<pfad>@<blob>`, wobei `<blob>` genau `git hash-object` der
//! gelesenen Bytes ist — auch für eine Fassung, die (noch) nicht committet
//! ist; dann warnt `bind`. Der Snapshot geht **nur** über `redact_intent`
//! in den Store (fail-closed: Findet die Policy in der Datei etwas, gibt es
//! keinen Anker, weil die Blob-Id sonst ein Orakel wäre).
//!
//! Mit `--issue` (EA-16) ist die Quelle ein GitLab-Issue in seiner heutigen
//! Fassung: `issue:<projekt>#<iid>@<updated_at>`, der Snapshot ist das
//! redigierte kanonische JSON `{"description":…,"title":…}`
//! ([`minds_redact::RedactionPipeline::redact_issue_intent`]). Instanz und
//! Token: [`crate::intent_issue`] — nie aus `.git/config`. Ob die Fassung
//! später noch existiert, prüft `minds verify --online`.
//!
//! # Signieren und Aktivieren
//!
//! `sign` signiert den Ankertext unter `minds-intent` und legt die Signatur
//! neben den Anker (`anchor.sig`). Ein FIDO-Schlüssel (`sk-…`) verlangt eine
//! Berührung: Ein Agent mit voller Shell kann dann keinen Intent freigeben.
//! Damit der Mensch die Aufforderung sieht, erbt `ssh-keygen` dafür stderr.
//! Gezeigt wird **vor** dem Signieren, was signiert wird — die Vorgabe
//! „zuletzt gebunden" stammt aus einer Datei im Git-Verzeichnis, die auch
//! der Agent schreiben kann.
//!
//! Danach wird aktiviert — und gesagt, auf welchem Weg:
//!
//! - **Witness:** über den Steuer-Socket im Witness-Home
//!   (`control/control.sock`, nur host-seitig erreichbar, EA-14). Das Home
//!   kommt aus `--witness-home`, sonst aus `MINDS_WITNESS_HOME` bzw. dem
//!   XDG-Pfad dieses Repositorys, sofern dort ein Steuer-Socket liegt.
//! - **Lokal (A1):** ohne Witness die Datei `<git-dir>/minds/intent/active`,
//!   die der lokale Checkpoint liest — eine schwächere, nicht verkettete
//!   Bindung.
//!
//! Nennt `MINDS_WITNESS_SOCKET` einen Witness, dessen Steuer-Socket hier
//! nicht erreichbar ist (die Agent-Seite), wird **nicht** auf die Datei
//! ausgewichen: Die Hooks melden dort an den Witness, der die Datei nie
//! liest — die Bindung ginge still verloren.
//!
//! Was der Intent für die Assurance bedeutet, rechnet erst `minds verify`
//! zur Lesezeit (W2): Hier wird nichts davon gespeichert.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use minds_core::ContentHash;
use minds_core::intent_anchor::{IntentSource, MAX_SNAPSHOT};
use minds_store::StoredIntent;

use crate::checkpoint::core::{ACTIVE_INTENT_FILE, open_regular};
use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Der zuletzt gebundene Anker — die Vorgabe für `sign` und `show`,
/// relativ zum Git-Verzeichnis. Eine Bequemlichkeit, kein Beleg.
const LAST_BOUND_FILE: &str = "minds/intent/last-bound";

/// Höchstgröße der Id-Dateien (`active`, `last-bound`).
const ID_FILE_MAX: u64 = 1024;

/// Breite der Beschriftung (`intent  `, `scope   `).
const LABEL: usize = 8;

/// Die Frist für die Aktivierung beim Witness.
const ACTIVATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Führt `minds intent` aus.
pub fn run(parsed: &crate::Parsed) -> ExitCode {
    match dispatch(parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds intent: {}", crate::text::sanitize(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

fn dispatch(parsed: &crate::Parsed) -> Fallible<()> {
    let command = parsed
        .positional(0)
        .ok_or("expected: intent bind, sign, show or list")?;
    // Jedes Flag nur dort, wo es etwas bedeutet — ein `--scope` an `sign`
    // stillschweigend zu übergehen hieße, der Aufrufer glaubte, er gelte.
    let allowed: &[&str] = match command {
        "bind" => &["--file", "--issue", "--scope", "--gitlab-url"],
        "sign" => &["--key", "--witness-home"],
        "show" | "list" => &[],
        other => {
            return Err(format!(
                "unknown subcommand \"{}\" — expected bind, sign, show or list",
                crate::text::sanitize(other)
            )
            .into());
        }
    };
    for flag in [
        "--file",
        "--issue",
        "--scope",
        "--gitlab-url",
        "--key",
        "--witness-home",
    ] {
        if parsed.value(flag).is_some() && !allowed.contains(&flag) {
            return Err(format!("{flag} does not apply to `minds intent {command}`").into());
        }
    }
    if parsed.has("--allow-confidential") && parsed.value("--issue").is_none() {
        return Err("--allow-confidential applies only to `minds intent bind --issue`".into());
    }
    let target = parsed.positional(1);
    if target.is_some() && matches!(command, "bind" | "list") {
        return Err(format!("`minds intent {command}` takes no anchor id").into());
    }
    match command {
        "bind" => match (parsed.value("--file"), parsed.value("--issue")) {
            (Some(file), None) => {
                if parsed.value("--gitlab-url").is_some() {
                    return Err("--gitlab-url applies only to --issue".into());
                }
                bind(file, parsed.value("--scope"))
            }
            (None, Some(issue)) => bind_issue(
                issue,
                parsed.value("--scope"),
                parsed.value("--gitlab-url"),
                parsed.has("--allow-confidential"),
            ),
            (Some(_), Some(_)) => Err("--file and --issue exclude each other".into()),
            (None, None) => Err("expected --file <path> or --issue <project#iid>".into()),
        },
        "sign" => sign(
            target,
            parsed.value("--key"),
            parsed.value("--witness-home"),
        ),
        "show" => show(target),
        _ => list(),
    }
}

// ---------------------------------------------------------------------------
// bind
// ---------------------------------------------------------------------------

fn bind(file: &str, scope: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    let (path, bytes) = read_requirement(&ctx.repo, &ctx.root, Path::new(file))?;
    let scope = parse_scope(scope)?;
    let blob = ctx.repo.blob_id_of(&bytes)?;
    let committed = in_head(&ctx.repo, &path, &blob);
    let pipeline = crate::config::load_redaction(&ctx.root)?.pipeline()?;
    let intent = pipeline.redact_intent(IntentSource::File { path, blob }, scope, bytes)?;
    let id = ctx.store.put_intent(&intent)?;
    if !committed {
        eprintln!("warning: requirement not committed — anchor refers to a working-tree version");
    }
    let summary = summary(intent.anchor());
    println!("{}", labeled("intent", &summary.intent));
    println!("{}", labeled("scope", &summary.scope));
    println!("{}", labeled("anchor", id.as_str()));
    // Der Anker liegt schon im Store: Scheitert die Vorgabe, bleibt die Id
    // oben stehen — `sign <id>` geht trotzdem.
    if let Err(err) = write_id_file(ctx.repo.git_dir(), LAST_BOUND_FILE, &id) {
        eprintln!(
            "warning: anchor stored, but the last-bound default was not updated: {}",
            crate::text::sanitize(&err.to_string())
        );
    }
    Ok(())
}

/// `bind --issue`: das Issue in seiner heutigen Fassung binden (EA-16).
fn bind_issue(
    issue: &str,
    scope: Option<&str>,
    gitlab_url: Option<&str>,
    allow_confidential: bool,
) -> Fallible<()> {
    let (project, iid) = minds_core::intent_anchor::parse_issue_ref(issue).ok_or_else(|| {
        format!(
            "--issue expects <group/project#iid> (e.g. team/minds#42), got \"{}\"",
            crate::text::sanitize(issue)
        )
    })?;
    let scope = parse_scope(scope)?;
    let ctx = Context::open()?;
    // Die Policy vor dem Netz: Ist sie kaputt, geht keine Anfrage hinaus.
    let pipeline = crate::config::load_redaction(&ctx.root)?.pipeline()?;
    let base = crate::intent_issue::base_url(gitlab_url)?;
    let access = crate::intent_issue::project(&base, &project)?;
    let snapshot = access
        .issue_snapshot(iid)
        .map_err(|err| format!("cannot read issue {project}#{iid}: {err}"))?;
    let web_url = snapshot.web_url;
    // Ein vertrauliches Issue ist nicht geheim im Sinne der Detektoren —
    // aber `refs/minds/` wird gesynct, auch an Remotes, die mehr Leute sehen
    // als GitLab erlaubt. Fail-closed: nur mit ausdrücklicher Zustimmung.
    if snapshot.confidential {
        if !allow_confidential {
            return Err(format!(
                "issue {project}#{iid} is confidential — its text would be stored under \
                 refs/minds/intents and pushed by minds sync; nothing was stored \
                 (pass --allow-confidential to bind it anyway)"
            )
            .into());
        }
        eprintln!(
            "warning: issue {project}#{iid} is confidential — its redacted text is stored \
             under refs/minds/intents and pushed by minds sync"
        );
    }
    let intent = pipeline.redact_issue_intent(
        minds_redact::IssueRef {
            project,
            iid,
            updated_at: snapshot.updated_at,
        },
        scope,
        snapshot.title,
        snapshot.description,
    )?;
    let id = ctx.store.put_intent(&intent)?;
    let counts = intent.audit().counts();
    if !intent.audit().is_clean() {
        eprintln!(
            "warning: the issue text contained {} secret(s) and {} personal value(s) — \
             the anchor binds the redacted version",
            counts.secrets, counts.pii
        );
    }
    let summary = summary(intent.anchor());
    println!("{}", labeled("intent", &summary.intent));
    println!("{}", labeled("scope", &summary.scope));
    println!("{}", labeled("anchor", id.as_str()));
    if !web_url.is_empty() {
        println!("{}", labeled("issue", &visible(&web_url)));
    }
    if let Err(err) = write_id_file(ctx.repo.git_dir(), LAST_BOUND_FILE, &id) {
        eprintln!(
            "warning: anchor stored, but the last-bound default was not updated: {}",
            crate::text::sanitize(&err.to_string())
        );
    }
    Ok(())
}

/// Ob `blob` die Fassung von `path` in `HEAD` ist. `<blob>` ist der Hash der
/// **ungefilterten** Bytes (`git hash-object --no-filters`): Mit Clean-
/// Filtern (`core.autocrlf`, LFS) weicht er auch für eine unveränderte
/// Datei ab — dann heißt es „nicht in HEAD", die schwächere Aussage.
pub(crate) fn in_head(repo: &minds_git::Repo, path: &str, blob: &str) -> bool {
    repo.head()
        .ok()
        .and_then(|head| head.commit())
        .and_then(|commit| {
            repo.read_blob_bounded(commit, path, MAX_SNAPSHOT as u64)
                .ok()
                .flatten()
        })
        .is_some_and(|(id, _)| id == blob)
}

/// Liest die Anforderungsdatei: ein reguläres File **im** Worktree (nach
/// Auflösen von Symlinks), nicht unter `.git` oder dem Git-Verzeichnis,
/// keine Zugangsdaten-Datei (`.env`, `.pgpass`, … — die Secretfile-Mauer),
/// nicht von `.gitignore` ausgeschlossen (W7), höchstens [`MAX_SNAPSHOT`]
/// Bytes. Gibt den repo-relativen Pfad (mit `/`) und die Bytes zurück.
fn read_requirement(
    repo: &minds_git::Repo,
    root: &Path,
    file: &Path,
) -> Fallible<(String, Vec<u8>)> {
    let absolute = std::env::current_dir()?.join(file);
    let canonical = absolute
        .canonicalize()
        .map_err(|err| format!("cannot read {}: {err}", file.display()))?;
    let root = root.canonicalize()?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| "the requirement file must lie inside this repository's worktree")?;
    let mut segments = Vec::new();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err("the requirement path is not repository-relative".into());
        };
        let segment = segment
            .to_str()
            .ok_or("the requirement path is not valid UTF-8")?;
        if segment.eq_ignore_ascii_case(".git") {
            return Err("a file inside .git is not a requirement".into());
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err("expected a file, not the repository root".into());
    }
    // Das Git-Verzeichnis liegt nicht immer unter `.git` (verlinkte
    // Worktrees, `--separate-git-dir`): nichts darunter.
    for git in [repo.git_dir(), repo.common_dir()] {
        if git
            .canonicalize()
            .is_ok_and(|git| canonical.starts_with(git))
        {
            return Err("a file inside the git directory is not a requirement".into());
        }
    }
    let path = segments.join("/");
    // Vor jedem Lesen: Was in `.env` steht, finden die Detektoren nicht
    // immer — es gehört nie in einen gesyncten Ref.
    // Auch über den absoluten Pfad: Wurzelt das Repository selbst in
    // `~/.docker` oder `~/.kube`, sieht der relative Pfad harmlos aus.
    if minds_redact::is_secret_file(&format!("/{path}"))
        || canonical.to_str().is_some_and(minds_redact::is_secret_file)
    {
        return Err("the requirement file is a credential file — refusing to store it".into());
    }
    if ignored(repo, &root, &path)? {
        return Err("the requirement file is ignored by .gitignore — refusing to store it".into());
    }
    // Ohne zu blockieren geöffnet: Ein gepflanztes FIFO hält `bind` nicht an.
    let mut handle = open_regular(&canonical)?;
    if !handle.metadata()?.is_file() {
        return Err("the requirement must be a regular file".into());
    }
    // Zwischen Auflösen und Öffnen könnte eine Pfadkomponente gegen einen
    // Symlink nach außen getauscht worden sein: Geöffnet sein muss genau die
    // Datei, die der aufgelöste Pfad jetzt noch nennt.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = handle.metadata()?;
        let named = std::fs::symlink_metadata(&canonical)?;
        // Ein Hardlink kann eine Datei von außerhalb unter einen
        // Worktree-Pfad stellen.
        if opened.nlink() != 1 {
            return Err("the requirement file has other hard links — refusing to store it".into());
        }
        if named.file_type().is_symlink()
            || (opened.dev(), opened.ino()) != (named.dev(), named.ino())
            || canonical.canonicalize()? != canonical
        {
            return Err("the requirement file changed while it was read".into());
        }
    }
    let mut bytes = Vec::new();
    (&mut handle)
        .take(MAX_SNAPSHOT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SNAPSHOT {
        return Err(format!(
            "the requirement file is larger than {} MiB",
            MAX_SNAPSHOT / (1024 * 1024)
        )
        .into());
    }
    Ok((path, bytes))
}

/// Höchstgröße einer Ignore-Regeldatei, die gelesen wird.
const MAX_RULE_BYTES: u64 = 1024 * 1024;

/// Höchstgröße des Index, der für „getrackt" gelesen wird.
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;

/// Ob `.gitignore` bzw. `info/exclude` den (nicht getrackten) Pfad
/// ausschließen — **im Prozess** ausgewertet wie beim Witness-Beobachter
/// ([`minds_git::ignore::IgnoreRules`]), nie über `git check-ignore`: Das
/// liest den Index und startet dabei `core.fsmonitor` aus der
/// `.git/config`, die der Agent schreiben kann — Befehle des Agenten liefen
/// sonst als der Mensch auf dem Host. Dieselbe Semantik wie beim
/// Beobachter (W7): Groß-/Kleinschreibung gefaltet, keine globale
/// Exclude-Datei. Eine Regeldatei, die sich nicht sicher lesen lässt, ist
/// ein Fehler (fail-closed).
fn ignored(repo: &minds_git::Repo, root: &Path, path: &str) -> Fallible<bool> {
    // Ein unlesbarer Index heißt: nichts gilt als getrackt — eher ignoriert
    // als abgelegt.
    let tracked: std::collections::BTreeSet<String> = repo
        .tracked_paths(MAX_INDEX_BYTES)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let exclude = read_rule_file(&repo.common_dir().join("info/exclude"))?;
    let mut rules =
        minds_git::ignore::IgnoreRules::new(exclude.as_deref(), std::sync::Arc::new(tracked));
    let parts: Vec<&str> = path.split('/').collect();
    // Flache Verzeichnisse zuerst: tiefere `.gitignore` gehen vor.
    for depth in 0..parts.len() {
        let dir = parts[..depth].join("/");
        if let Some(bytes) = read_rule_file(&root.join(&dir).join(".gitignore"))? {
            rules.add_gitignore(&dir, &bytes);
        }
    }
    Ok(rules.is_ignored(path, Some(false)))
}

/// Eine Ignore-Regeldatei: fehlt sie, `None`; ist sie kein reguläres File
/// (Symlink, FIFO) oder zu groß, ein Fehler.
fn read_rule_file(path: &Path) -> Fallible<Option<Vec<u8>>> {
    let file = match open_regular(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable_rules(path)),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_RULE_BYTES {
        return Err(unreadable_rules(path));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RULE_BYTES).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn unreadable_rules(path: &Path) -> Box<dyn std::error::Error> {
    format!(
        "cannot safely read {} — cannot tell whether the file is ignored",
        crate::text::sanitize(&path.display().to_string())
    )
    .into()
}

/// `--scope a,b` — tolerant gelesen (Leerraum um die Globs fällt weg),
/// kanonisch geschrieben. Ohne Flag: kein Bereich (`scope=-`).
fn parse_scope(raw: Option<&str>) -> Fallible<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(|glob| {
            let glob = glob.trim();
            if glob.is_empty() || glob == "-" {
                Err("--scope expects a comma-separated list of non-empty globs (not \"-\")".into())
            } else if glob.starts_with('!') {
                // Keine Verneinung: `minds verify` kennt sie nicht und
                // wertete den Bereich als nicht beurteilbar.
                Err("--scope does not support negated globs (\"!…\")".into())
            } else {
                Ok(glob.to_owned())
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// sign
// ---------------------------------------------------------------------------

fn sign(target: Option<&str>, key: Option<&str>, witness_home: Option<&str>) -> Fallible<()> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen not found — required to sign an intent".into());
    }
    let ctx = Context::open()?;
    let id = resolve_target(&ctx, target)?;
    // Nur ein belegter Anker (EA-14, `intent_proof`): Ein anderer würde vom
    // Witness wie vom lokalen Checkpoint ohnehin abgewiesen — die Signatur
    // wäre eine Freigabe, die nie gilt.
    // Geprüft, gezeigt und signiert wird **derselbe** gelesene Stand — der
    // Ref ist beschreibbar, ein zweites Lesen könnte einen anderen liefern.
    let stored = load(&ctx, &id)?;
    let pipeline = crate::config::load_redaction(&ctx.root)?.pipeline()?;
    crate::intent_proof::proven_stored(&stored, &ctx.repo, &pipeline)
        .map_err(|reason| format!("anchor {id} cannot be signed: {reason}"))?;
    // Der Weg der Aktivierung steht **vor** der Berührung fest: Ist er
    // verbaut, wird gar nicht erst signiert.
    let route = route(&ctx.root, witness_home)?;

    // Die Vorgabe „zuletzt gebunden" kann der Agent geschrieben haben —
    // etwa einen Anker über seine Fassung der Datei im Worktree mit
    // demselben Pfad, oder einen Prompt-Anker mit eigenem Text. Ohne
    // ausdrückliche Id wird deshalb nur eine Datei-Fassung in HEAD
    // freigegeben, die der Mensch per `git` prüfen kann.
    let head = match &stored.anchor.source {
        IntentSource::File { path, blob } => in_head(&ctx.repo, path, blob),
        IntentSource::Issue { .. } | IntentSource::Prompt => false,
    };
    if !head {
        if target.is_none() {
            return Err(format!(
                "the last bound anchor {id} is not a file version in HEAD — review it \
                 (minds intent show {id}) and pass the anchor id explicitly to sign it"
            )
            .into());
        }
        if matches!(stored.anchor.source, IntentSource::File { .. }) {
            eprintln!(
                "warning: this anchor is not the version in HEAD — it refers to a working-tree version"
            );
        }
    }

    let key = crate::sign_cmd::resolve_key(key, &ctx.root)?;
    // Der Pfad kann aus der Repo-Konfiguration stammen (`user.signingkey`),
    // und `ssh-keygen` zitiert ihn auf dem geerbten stderr.
    if crate::text::sanitize(&key) != key {
        return Err("the signing key path contains control or invisible characters".into());
    }
    let public = read_public_key(&key);

    // Erst zeigen, was freigegeben wird — dann die Berührung. Die
    // Kurzzeilen (stdout) reichen nicht, um zwei Fassungen zu
    // unterscheiden; die vollen Werte stehen auf stderr daneben: Wer die
    // Anker-Id mit der Ausgabe seines eigenen `bind` vergleicht, erkennt
    // einen untergeschobenen Anker — auch einen, den der Agent committet hat.
    let summary = summary(&stored.anchor);
    println!("{}", labeled("intent", &summary.intent));
    println!("{}", labeled("scope", &summary.scope));
    std::io::stdout().flush()?;
    eprintln!("{}", labeled("review", &format!("anchor {id}")));
    eprintln!(
        "{}",
        labeled("review", &format!("content {}", stored.anchor.content))
    );
    match &stored.anchor.source {
        IntentSource::File { path, blob } => {
            eprintln!(
                "{}",
                labeled("review", &format!("file {} blob {blob}", visible(path)))
            );
        }
        // Ohne Datei in Git ist der Text selbst das Einzige, was sich
        // prüfen lässt.
        IntentSource::Issue { .. } | IntentSource::Prompt => {
            // Der ganze Text: signiert wird der ganze Snapshot.
            let text = String::from_utf8_lossy(&stored.snapshot);
            for line in text.lines() {
                eprintln!("{}", labeled("review", &format!("| {}", visible(line))));
            }
        }
    }
    let public_type = public.as_deref().and_then(minds_attest::public_key_type);
    // stderr wird nur dann eingesammelt, wenn der öffentliche Schlüssel
    // sicher **kein** FIDO-Schlüssel ist. Fehlt die `.pub` oder ist sie
    // unlesbar, sieht der Mensch eine etwaige Berührungs-Aufforderung —
    // sonst blinkte der Schlüssel stumm.
    let software = public_type.is_some_and(|kind| !minds_attest::is_security_key_type(kind));
    let signature = if software {
        minds_attest::ssh_sign_ns(&stored.text, Path::new(&key), minds_attest::NS_INTENT)?
    } else {
        if public_type.is_some() {
            eprintln!("Touch your security key to approve this intent.");
        }
        minds_attest::ssh_sign_ns_presence(&stored.text, Path::new(&key), minds_attest::NS_INTENT)?
    };
    ctx.store.put_intent_signature(&id, &signature)?;
    println!(
        "{}",
        labeled("signed", &signer_line(&key, public.as_deref(), &signature))
    );

    let activation = activate(&ctx, route, &stored.text, &signature)
        .map_err(|err| format!("signature stored, but the intent is not active: {err}"))?;
    println!("{}", labeled("active", &activation));
    Ok(())
}

/// Der öffentliche Schlüssel zu `key`: die Datei selbst, wenn sie auf
/// `.pub` endet, sonst `<key>.pub`. Nur eine reguläre Datei (ein FIFO ließe
/// `sign` hängen), höchstens 64 KiB.
fn read_public_key(key: &str) -> Option<String> {
    let path = if key.ends_with(".pub") {
        PathBuf::from(key)
    } else {
        PathBuf::from(format!("{key}.pub"))
    };
    let file = open_regular(&path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(64 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
}

/// `patrick@doering-it (sk-ssh-ed25519, user presence)` — Typ und
/// User-Presence aus der Signatur selbst (dort steht der Schlüssel, der
/// tatsächlich signierte, und ob berührt wurde).
///
/// Der Name ist **nur Anzeige**: der Kommentar der `.pub`-Datei, sonst der
/// Pfad. Passt die `.pub` nicht zum privaten Schlüssel (oder hat der Agent
/// `user.signingkey` umgebogen), nennt er den Falschen — wer gilt,
/// entscheidet erst `minds verify` gegen die vertrauenswürdige Signer-Datei.
fn signer_line(key: &str, public: Option<&str>, signature: &str) -> String {
    let who = public
        .and_then(minds_attest::public_key_comment)
        .map_or_else(|| crate::text::sanitize(key), crate::text::sanitize);
    let (kind, presence) = match minds_attest::signature_key(signature) {
        Some(signed) => {
            let presence = match (
                minds_attest::is_security_key_type(&signed.key_type),
                signed.user_presence,
            ) {
                (true, true) => "user presence",
                (true, false) => "no user presence",
                (false, _) => "software key",
            };
            (signed.key_type, presence)
        }
        None => match public.and_then(minds_attest::public_key_type) {
            // Ohne lesbare Signatur gibt es keinen Beleg für eine Berührung.
            Some(kind) if minds_attest::is_security_key_type(kind) => {
                (kind.to_owned(), "presence unknown")
            }
            Some(kind) => (kind.to_owned(), "software key"),
            None => return format!("{who} (unknown key type)"),
        },
    };
    let short = kind.strip_suffix("@openssh.com").unwrap_or(&kind);
    format!("{who} ({}, {presence})", crate::text::sanitize(short))
}

/// Wohin aktiviert wird — bestimmt vor dem Signieren.
enum Route {
    /// Der Steuer-Socket eines laufenden Witness.
    Witness { home: PathBuf, socket: PathBuf },
    /// Die A1-Datei.
    Local,
}

/// Bestimmt den Weg: ein ausdrücklich genanntes oder vorgefundenes
/// Witness-Home mit Steuer-Socket, der antwortet — sonst die Datei. Liegt
/// dort ein Socket, der nicht antwortet, oder nennt `MINDS_WITNESS_SOCKET`
/// einen Witness ohne erreichbaren Steuer-Socket, ist das ein Fehler: Die
/// Datei liest ein Witness nie, die Bindung ginge still verloren.
fn route(root: &Path, witness_home: Option<&str>) -> Fallible<Route> {
    if let Some((home, explicit)) = witness_home_for(root, witness_home)? {
        let socket = home.join(control_socket());
        if explicit || std::fs::symlink_metadata(&socket).is_ok() {
            if reachable(&socket) {
                return Ok(Route::Witness { home, socket });
            }
            return Err(format!(
                "witness control socket {} did not answer — is the witness running \
                 (minds witness run)? Nothing was signed; retry, or, if no witness should \
                 run, remove that stale socket",
                crate::text::sanitize(&socket.display().to_string())
            )
            .into());
        }
    }
    if crate::hook::witness::socket_path().is_some() {
        return Err(
            "hooks report to a witness (MINDS_WITNESS_SOCKET), but its control socket is not \
             reachable from here — activate on the host: minds intent sign <anchor-id> \
             --witness-home <dir>"
                .into(),
        );
    }
    Ok(Route::Local)
}

/// Ob der Steuer-Socket antwortet — und von der Aktivierung angenommen
/// würde (absoluter Pfad, kein Symlink): Vorprüfung und Aktivierung dürfen
/// nicht verschieden urteilen, sonst wäre signiert, aber nicht aktiviert.
#[cfg(unix)]
fn reachable(socket: &Path) -> bool {
    socket.is_absolute()
        && std::fs::symlink_metadata(socket).is_ok_and(|meta| !meta.file_type().is_symlink())
        && crate::witness_cmd::ping(socket)
}

#[cfg(not(unix))]
fn reachable(_: &Path) -> bool {
    false
}

/// Aktiviert den Anker auf dem vorab bestimmten Weg und sagt, auf welchem
/// (`witness (container)` oder die lokale Datei).
fn activate(ctx: &Context, route: Route, anchor: &str, signature: &str) -> Fallible<String> {
    match route {
        Route::Witness { home, socket } => match activate_at_witness(&socket, anchor, signature) {
            // Das Profil steht in `witness.json` des Homes — so weit zu
            // trauen wie dieser Datei; die Bindung selbst bestätigte der
            // Witness eben.
            Ok(()) => Ok(match witness_profile(&home) {
                Some(profile) => format!("witness ({profile})"),
                None => "witness".to_owned(),
            }),
            Err(Refusal::Refused(reason)) => {
                Err(format!("the witness refused it: {reason}").into())
            }
            Err(Refusal::Unreachable(kind)) => Err(format!(
                "witness control socket {} not reachable ({kind})",
                crate::text::sanitize(&socket.display().to_string())
            )
            .into()),
        },
        Route::Local => {
            let id = minds_core::intent_anchor::IntentAnchor::id_of_text(anchor);
            write_id_file(ctx.repo.git_dir(), ACTIVE_INTENT_FILE, &id)?;
            let path = ctx.repo.git_dir().join(ACTIVE_INTENT_FILE);
            let shown = path
                .strip_prefix(&ctx.root)
                .unwrap_or(&path)
                .display()
                .to_string();
            Ok(format!(
                "local file {} (A1, unchained — no witness)",
                crate::text::sanitize(&shown)
            ))
        }
    }
}

/// Warum der Witness nicht aktiviert hat.
enum Refusal {
    /// Er hat geantwortet und abgelehnt (fester Grund).
    Refused(String),
    /// Er war nicht erreichbar.
    Unreachable(&'static str),
}

/// Das Witness-Home und ob es ausdrücklich genannt wurde. Ohne
/// `--witness-home`: `MINDS_WITNESS_HOME` oder der XDG-Pfad dieses
/// Repositorys (00-conventions) — nur ein Kandidat, genutzt nur, wenn dort
/// ein Steuer-Socket liegt.
#[cfg(unix)]
fn witness_home_for(root: &Path, explicit: Option<&str>) -> Fallible<Option<(PathBuf, bool)>> {
    if let Some(home) = explicit {
        if home.is_empty() {
            return Err("--witness-home must not be empty".into());
        }
        return Ok(Some((std::path::absolute(home)?, true)));
    }
    let root = root.canonicalize()?;
    Ok(root
        .to_str()
        .and_then(|root| crate::witness_cmd::resolve_home_for(None, Some(root)).ok())
        .and_then(|home| std::path::absolute(home).ok())
        .map(|home| (home, false)))
}

#[cfg(not(unix))]
fn witness_home_for(_: &Path, explicit: Option<&str>) -> Fallible<Option<(PathBuf, bool)>> {
    match explicit {
        Some(_) => Err("--witness-home: the witness is not supported on this platform".into()),
        None => Ok(None),
    }
}

#[cfg(unix)]
fn control_socket() -> &'static str {
    crate::witness_cmd::CONTROL_SOCKET
}

#[cfg(not(unix))]
fn control_socket() -> &'static str {
    "control/control.sock"
}

/// Das Profil aus `witness.json` des Homes, falls lesbar.
#[cfg(unix)]
fn witness_profile(home: &Path) -> Option<&'static str> {
    crate::witness_cmd::load(home)
        .ok()
        .map(|config| config.profile().name())
}

#[cfg(not(unix))]
fn witness_profile(_: &Path) -> Option<&'static str> {
    None
}

/// Schickt `IntentActivate` an den Steuer-Socket.
fn activate_at_witness(socket: &Path, anchor: &str, signature: &str) -> Result<(), Refusal> {
    use minds_capture::witness_proto::Frame;
    let frame = Frame::IntentActivate {
        anchor: anchor.to_owned(),
        signature: Some(signature.to_owned()),
        request_id: request_id(),
    };
    let deadline = std::time::Instant::now() + ACTIVATE_TIMEOUT;
    match crate::hook::witness::request(socket, &frame, deadline) {
        Ok(Frame::Ack { .. }) => Ok(()),
        // Der Grund kommt vom Witness, aber über einen Socket: entschärft.
        Ok(Frame::Nack { reason, .. }) => Err(Refusal::Refused(crate::text::sanitize(&reason))),
        Ok(_) => Err(Refusal::Unreachable("unexpected response")),
        Err(kind) => Err(Refusal::Unreachable(kind)),
    }
}

/// Eine Anfrage-Id — eindeutig, nicht geheim (wie beim Checkpoint).
fn request_id() -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"minds intent activate");
    hasher.update(&std::process::id().to_le_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(&now.as_nanos().to_le_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    id
}

// ---------------------------------------------------------------------------
// show / list
// ---------------------------------------------------------------------------

fn show(target: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    let id = resolve_target(&ctx, target)?;
    let stored = load(&ctx, &id)?;
    let summary = summary(&stored.anchor);
    println!("{}", labeled("anchor", id.as_str()));
    println!("{}", labeled("intent", &summary.intent));
    println!("{}", labeled("content", stored.anchor.content.as_str()));
    println!("{}", labeled("scope", &summary.scope));
    if let IntentSource::File { path, blob } = &stored.anchor.source {
        let head = if in_head(&ctx.repo, path, blob) {
            "the version in HEAD"
        } else {
            "a working-tree version, not the one in HEAD"
        };
        println!("{}", labeled("version", head));
    }
    // Der Store gehört womöglich dem Agenten: Ob Snapshot und Anker
    // zusammenpassen und bereinigt sind, wird hier geprüft, nicht
    // angenommen — `sign` verlangt denselben Beleg.
    // Beleg über genau den Stand, der unten gezeigt wird.
    let pipeline = crate::config::load_redaction(&ctx.root)?.pipeline()?;
    let proven = crate::intent_proof::proven_stored(&stored, &ctx.repo, &pipeline);
    let proof = match proven {
        Ok(()) => "ok".to_owned(),
        Err(reason) => format!("NOT PROVEN — {reason}"),
    };
    println!("{}", labeled("proof", &proof));
    let signed = match ctx.store.intent_signature(&id) {
        Ok(Some(signature)) => {
            let kind = minds_attest::signature_key_type(&signature)
                .unwrap_or_else(|| "unknown key type".to_owned());
            format!(
                "signature present (claims {}) — not verified here; minds verify checks it",
                crate::text::sanitize(&kind)
            )
        }
        Ok(None) => "no".to_owned(),
        Err(_) => "unreadable signature".to_owned(),
    };
    println!("{}", labeled("signed", &signed));
    let active = read_id_file(&ctx.repo.git_dir().join(ACTIVE_INTENT_FILE)).as_ref() == Some(&id);
    println!(
        "{}",
        labeled(
            "active",
            if active {
                "local file (A1)"
            } else {
                "not in the local file (a witness keeps its own)"
            }
        )
    );
    println!();
    // Ein nicht belegter Snapshot kann gepflanzter Klartext sein (auch eine
    // Zugangsdaten-Datei): Er wird nicht ausgegeben.
    if proven.is_err() {
        println!(
            "snapshot ({} bytes) not shown — the anchor is not proven",
            stored.snapshot.len()
        );
        return Ok(());
    }
    println!("snapshot ({} bytes, as stored)", stored.snapshot.len());
    for line in String::from_utf8_lossy(&stored.snapshot).lines() {
        println!("  {}", crate::text::sanitize(line));
    }
    Ok(())
}

fn list() -> Fallible<()> {
    let ctx = Context::open()?;
    let ids = ctx.store.list_intents()?;
    if ids.is_empty() {
        println!(
            "no intent anchors — bind one with: minds intent bind --file <path> (or --issue <project#iid>)"
        );
        return Ok(());
    }
    let active = read_id_file(&ctx.repo.git_dir().join(ACTIVE_INTENT_FILE));
    for id in ids {
        let marker = if active.as_ref() == Some(&id) {
            "  (active, local file)"
        } else {
            ""
        };
        match ctx.store.get_intent(&id) {
            Ok(Some(stored)) => {
                // Vorhanden, nicht geprüft — das tut `minds verify`.
                let signed = match ctx.store.intent_signature(&id) {
                    Ok(Some(_)) => "has sig ",
                    Ok(None) => "unsigned",
                    Err(_) => "sig?    ",
                };
                println!("{id}  {signed}  {}{marker}", summary(&stored.anchor).source);
            }
            Ok(None) | Err(_) => println!("{id}  unreadable{marker}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Gemeinsames
// ---------------------------------------------------------------------------

/// Die Anker-Id: ausdrücklich genannt, sonst die zuletzt gebundene.
fn resolve_target(ctx: &Context, target: Option<&str>) -> Fallible<ContentHash> {
    match target {
        Some(raw) => raw.parse().map_err(|err| {
            format!(
                "not a valid anchor id \"{}\": {err}",
                crate::text::sanitize(raw)
            )
            .into()
        }),
        None => read_id_file(&ctx.repo.git_dir().join(LAST_BOUND_FILE)).ok_or_else(|| {
            "no anchor id given and none bound yet — minds intent bind --file <path>".into()
        }),
    }
}

/// Liest einen Anker samt Snapshot aus dem Store.
fn load(ctx: &Context, id: &ContentHash) -> Fallible<StoredIntent> {
    ctx.store
        .get_intent(id)?
        .ok_or_else(|| format!("intent anchor {id} is not in the store").into())
}

/// Die Kurzform eines Ankers für die Ausgabe.
pub(crate) struct Summary {
    /// `file:docs/req.md@3f9c1e2a`
    pub(crate) source: String,
    /// `file:docs/req.md@3f9c1e2a  content b3-7a41…`
    intent: String,
    /// `src/sort/**, tests/**` oder `none`
    scope: String,
}

pub(crate) fn summary(anchor: &minds_core::intent_anchor::IntentAnchor) -> Summary {
    let source = match &anchor.source {
        IntentSource::File { path, blob } => {
            format!("file:{}@{}", visible(path), blob.get(..8).unwrap_or(blob))
        }
        IntentSource::Issue {
            project,
            iid,
            updated_at,
        } => format!("issue:{}#{iid}@{}", visible(project), visible(updated_at)),
        IntentSource::Prompt => "prompt".to_owned(),
    };
    let content = anchor.content.as_str();
    let intent = format!("{source}  content {}…", content.get(..7).unwrap_or(content));
    let scope = if anchor.scope.is_empty() {
        "none".to_owned()
    } else {
        anchor
            .scope
            .iter()
            .map(|glob| visible(glob))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Summary {
        source,
        intent,
        scope,
    }
}

/// Fremder Text so, dass jedes Zeichen sichtbar ist: entschärft und
/// Nicht-ASCII als `\u{…}` — ein kyrillisches `а` im Pfad sieht sonst aus
/// wie ein lateinisches.
pub(crate) fn visible(text: &str) -> String {
    crate::text::sanitize(text)
        .chars()
        .map(|c| {
            if c == '\\' {
                "\\\\".to_owned()
            } else if c.is_ascii() {
                c.to_string()
            } else {
                format!("\\u{{{:x}}}", c as u32)
            }
        })
        .collect()
}

fn labeled(label: &str, value: &str) -> String {
    format!("{label:<LABEL$}{value}")
}

/// Liest eine Id-Datei — tolerant (Leerraum, Groß-/Kleinschreibung),
/// begrenzt, nur eine reguläre Datei, ohne Symlinks zu folgen. Was keine Id
/// ist, ist keine.
pub(crate) fn read_id_file(path: &Path) -> Option<ContentHash> {
    let file = open_regular(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(ID_FILE_MAX + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > ID_FILE_MAX {
        return None;
    }
    text.trim().parse().ok()
}

/// Schreibt `id` kanonisch (`b3-<64hex>\n`) nach `<git_dir>/<relative>` —
/// atomar über eine frisch angelegte Nachbardatei und `rename`.
///
/// Das Git-Verzeichnis kann der Agent beschreiben (im Container ist es
/// eingebunden). Deshalb wird jedes Verzeichnis unter `git_dir` einzeln
/// angelegt bzw. geprüft (ein Symlink dort ist ein Fehler) und die
/// Temp-Datei nach dem Anlegen gegen das erwartete Verzeichnis
/// nachgeprüft. Am Ziel selbst ersetzt `rename` einen Symlink, statt ihm zu
/// folgen. Ein Restfenster zwischen Nachprüfung und `rename` bleibt (ohne
/// `renameat` auf einem Verzeichnis-fd); Inhalt und Name stehen fest.
fn write_id_file(git_dir: &Path, relative: &str, id: &ContentHash) -> Fallible<()> {
    let path = git_dir.join(relative);
    let mut dir = git_dir.to_path_buf();
    let parents: Vec<&str> = relative.split('/').collect();
    let (name, parents) = parents.split_last().ok_or("invalid intent file path")?;
    for segment in parents {
        dir.push(segment);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a plain directory — refusing to write through it",
                    crate::text::sanitize(&dir.display().to_string())
                )
                .into());
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&dir) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !std::fs::symlink_metadata(&dir)?.is_dir() {
                            return Err("intent directory replaced while writing".into());
                        }
                    }
                    Err(err) => return Err(err.into()),
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
    let expected = git_dir.canonicalize()?.join(parents.join("/"));
    let tmp = dir.join(format!(".{name}.{}.tmp", hex16(&request_id())));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        // Nachgeprüft: Die Datei entstand im erwarteten Verzeichnis — nicht
        // hinter einem Verzeichnis-Symlink, der nach der Prüfung oben
        // getauscht wurde. Ein Restfenster bis zum `rename` bleibt.
        let moved = || std::io::Error::other("intent directory replaced while writing");
        if dir.canonicalize()? != expected {
            return Err(moved());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (opened, named) = (file.metadata()?, std::fs::symlink_metadata(&tmp)?);
            if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
                return Err(moved());
            }
        }
        file.write_all(format!("{id}\n").as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Key-Typ-Erkennung (EA-15): aus der `.pub`-Zeile für die Frage, ob
    /// stderr geerbt wird, aus der Signatur für die Ausgabe.
    #[test]
    fn intent_sign_detects_key_type() {
        let sk = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t patrick@doering-it\n";
        let ecdsa_sk = "sk-ecdsa-sha2-nistp256@openssh.com AAAAInNr patrick@doering-it\n";
        let software = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 patrick@doering-it\n";
        for (line, presence) in [(sk, true), (ecdsa_sk, true), (software, false)] {
            assert_eq!(
                minds_attest::public_key_type(line).is_some_and(minds_attest::is_security_key_type),
                presence,
                "{line}"
            );
        }
        // Ohne lesbare Signatur fällt die Zeile auf den `.pub`-Typ zurück —
        // eine Berührung behauptet sie dann nicht.
        assert_eq!(
            signer_line("/k/id_sk", Some(sk), "not a signature"),
            "patrick@doering-it (sk-ssh-ed25519, presence unknown)"
        );
        assert_eq!(
            signer_line("/k/id", Some(software), "not a signature"),
            "patrick@doering-it (ssh-ed25519, software key)"
        );
        assert_eq!(
            signer_line("/k/id", None, "not a signature"),
            "/k/id (unknown key type)"
        );
    }

    /// Security-Review EA-15: Ein Homoglyph im Pfad bleibt sichtbar.
    #[test]
    fn non_ascii_is_shown_escaped() {
        assert_eq!(visible("docs/anforderung.md"), "docs/anforderung.md");
        assert_eq!(
            visible("docs/\u{430}nforderung.md"),
            "docs/\\u{430}nforderung.md"
        );
    }

    #[test]
    fn scope_is_read_tolerantly_and_rejects_empty_globs() {
        assert_eq!(parse_scope(None).unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_scope(Some("src/sort/**, tests/**")).unwrap(),
            vec!["src/sort/**", "tests/**"]
        );
        for bad in ["", ",", "a,,b", "a, ", "src/**,!src/auth/**"] {
            assert!(parse_scope(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn id_files_are_written_canonically_and_read_tolerantly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minds/intent/active");
        let id: ContentHash = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        write_id_file(dir.path(), ACTIVE_INTENT_FILE, &id).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("b3-{}\n", "a".repeat(64))
        );
        assert_eq!(read_id_file(&path), Some(id.clone()));
        std::fs::write(&path, format!("  B3-{}  \n", "A".repeat(64))).unwrap();
        assert_eq!(read_id_file(&path), Some(id));
        std::fs::write(&path, "-\n").unwrap();
        assert_eq!(read_id_file(&path), None);
        assert_eq!(read_id_file(&dir.path().join("missing")), None);
        // Keine Reste der atomaren Schreibweise.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("active")]);
    }

    #[cfg(unix)]
    #[test]
    fn writing_an_id_file_replaces_a_planted_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();
        let git_dir = dir.path().join("git");
        std::fs::create_dir_all(git_dir.join("minds/intent")).unwrap();
        let path = git_dir.join(ACTIVE_INTENT_FILE);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let id: ContentHash = format!("b3-{}", "b".repeat(64)).parse().unwrap();
        write_id_file(&git_dir, ACTIVE_INTENT_FILE, &id).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        assert!(!std::fs::symlink_metadata(&path).unwrap().is_symlink());
    }

    /// Security-Review EA-15: Ein vom Agenten gepflanzter Verzeichnis-Symlink
    /// unter dem Git-Verzeichnis lenkt den Schreibzugriff nicht nach außen.
    #[cfg(unix)]
    #[test]
    fn writing_an_id_file_refuses_a_planted_directory_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let git_dir = dir.path().join("git");
        std::fs::create_dir_all(git_dir.join("minds")).unwrap();
        std::os::unix::fs::symlink(&outside, git_dir.join("minds/intent")).unwrap();
        let id: ContentHash = format!("b3-{}", "c".repeat(64)).parse().unwrap();
        let err = write_id_file(&git_dir, ACTIVE_INTENT_FILE, &id).unwrap_err();
        assert!(err.to_string().contains("not a plain directory"), "{err}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
}
