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
//! - **Geschützter Witness** (`minds enable`, eigener Nutzer): Seinen
//!   Steuer-Socket erreicht der Mensch nicht — sonst erreichte ihn auch der
//!   Agent, der dieselbe Kennung hat. Aktiviert wird mit einem Beleg, dass
//!   der Mensch es **jetzt** will: Signierte ein Sicherheitsschlüssel
//!   (`sk-…`), holt `sign` eine Nonce vom Witness und signiert Anker-Id und
//!   Nonce unter `minds-intent-activate` (eine zweite Berührung) — der
//!   Witness prüft sie gegen den Schlüssel, den die Einrichtung festhielt.
//!   Sonst (oder ohne festgehaltenen Schlüssel) aktiviert `sign` als
//!   Witness-Nutzer mit der einen Erhöhung; das Passwort ist der Beleg.
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

    // `--key`, sonst der eigene Freigabe-Schlüssel (`minds.intentKey`, den
    // `minds enable` am geschützten Witness festhält), sonst
    // `user.signingkey`.
    let key = match key {
        Some(key) => key.to_owned(),
        None => match crate::sign_cmd::intent_key_config(&ctx.root) {
            Some(key) => key,
            None => crate::sign_cmd::resolve_key(None, &ctx.root)?,
        },
    };
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
    // Am geschützten Witness signiert immer der abgeschirmte Signierer
    // ([`TouchSigner`]) — was die `.pub` behauptet, entscheidet das nicht:
    // Die kann der Agent gelöscht oder umgeschrieben haben.
    let shielded = match &route {
        Route::Protected { .. } | Route::ProtectedPassword => Some(shielded_signer(&ctx.root)?),
        Route::Witness { .. } | Route::Local => None,
    };
    let signature = if let Some(signer) = &shielded {
        if !software {
            eprintln!("Touch your security key to approve this intent.");
        }
        signer.sign(&stored.text, Path::new(&key), minds_attest::NS_INTENT)?
    } else if software {
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

    let activation = activate(
        &ctx,
        route,
        &stored.text,
        &signature,
        shielded.as_ref(),
        Path::new(&key),
    )
    .map_err(|err| format!("signature stored, but the intent is not active: {err}"))?;
    println!("{}", labeled("active", &activation));
    Ok(())
}

/// Der öffentliche Schlüssel zu `key`: die Datei selbst, wenn sie auf
/// `.pub` endet, sonst `<key>.pub`. Nur eine reguläre Datei (ein FIFO ließe
/// `sign` hängen), höchstens 64 KiB.
pub(crate) fn read_public_key(key: &str) -> Option<String> {
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
pub(crate) fn signer_line(key: &str, public: Option<&str>, signature: &str) -> String {
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
    /// Der geschützte Witness dieses Repos: sein Socket der Agent-Seite
    /// und die Kennung, die die Gegenstelle haben muss.
    Protected { socket: PathBuf, peer: u32 },
    /// Der geschützte Witness, dessen Socket diese Sitzung (noch) nicht
    /// erreicht — nur der Weg über das Passwort bleibt.
    ProtectedPassword,
    /// Die A1-Datei.
    Local,
}

/// Was über den geschützten Witness dieses Repos feststeht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectedState {
    /// Keiner eingerichtet (oder nicht an einem vertrauenswürdigen Ort).
    Absent,
    /// Gefunden, sein Socket antwortet.
    Reachable,
    /// Gefunden, aber stumm.
    Silent,
    /// Für dieses Repo eingerichtet, sein Home ist für diese Sitzung aber
    /// verschlossen, und dieser Nutzer steht in seiner Gruppe, die die
    /// Sitzung noch nicht hat (Linux: erst nach dem nächsten Anmelden).
    GroupPending,
}

/// Die Entscheidung für den geschützten Witness — `None`: keiner, weiter
/// mit den anderen Wegen.
fn protected_route(
    state: ProtectedState,
    found: Option<(PathBuf, u32)>,
) -> Option<Fallible<Route>> {
    match (state, found) {
        (ProtectedState::Reachable, Some((socket, peer))) => {
            Some(Ok(Route::Protected { socket, peer }))
        }
        // Den Weg über das Passwort braucht die Gruppe nicht.
        (ProtectedState::GroupPending, _) => Some(Ok(Route::ProtectedPassword)),
        (ProtectedState::Silent, _) | (ProtectedState::Reachable, None) => Some(Err(
            "the protected witness of this repository did not answer — is its service running \
             (minds doctor)? Nothing was signed"
                .into(),
        )),
        (ProtectedState::Absent, _) => None,
    }
}

/// Bestimmt den Weg: ein ausdrücklich genanntes oder vorgefundenes
/// Witness-Home mit Steuer-Socket, der antwortet — sonst die Datei. Liegt
/// dort ein Socket, der nicht antwortet, oder nennt `MINDS_WITNESS_SOCKET`
/// einen Witness ohne erreichbaren Steuer-Socket, ist das ein Fehler: Die
/// Datei liest ein Witness nie, die Bindung ginge still verloren.
fn route(root: &Path, witness_home: Option<&str>) -> Fallible<Route> {
    // Der geschützte Witness zuerst — wie für die Hooks. Ein ausdrücklich
    // genanntes Home geht vor.
    if witness_home.is_none() {
        let found = protected(root);
        let state = match &found {
            Some((socket, _)) if reachable(socket) => ProtectedState::Reachable,
            Some(_) => ProtectedState::Silent,
            // Eingerichtet, aber ohne Socket: ein angehaltener Dienst — nie
            // „kein geschützter Witness" (die A1-Datei läse er nie).
            None if protected_home(root) => ProtectedState::Silent,
            None if group_pending(root) => ProtectedState::GroupPending,
            None => ProtectedState::Absent,
        };
        if let Some(route) = protected_route(state, found) {
            return route;
        }
    }
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

/// Der geschützte Witness dieses Repos, wie die Hooks ihn finden.
#[cfg(unix)]
fn protected(root: &Path) -> Option<(PathBuf, u32)> {
    crate::enable_protected::discover(&root.canonicalize().ok()?)
}

#[cfg(not(unix))]
fn protected(_: &Path) -> Option<(PathBuf, u32)> {
    None
}

/// Belegt wie in `minds doctor`: Das Home des geschützten Witness für
/// dieses Repo ist für diese Sitzung verschlossen, und dieser Nutzer steht
/// in der Witness-Gruppe, die die Sitzung noch nicht hat.
#[cfg(unix)]
fn group_pending(root: &Path) -> bool {
    root.canonicalize().is_ok_and(|root| {
        std::fs::symlink_metadata(crate::enable_protected::home_of(&root))
            .is_err_and(|err| err.kind() == std::io::ErrorKind::PermissionDenied)
    }) && crate::enable_protected::membership_pending()
}

#[cfg(not(unix))]
fn group_pending(_: &Path) -> bool {
    false
}

/// Ob für dieses Repo ein geschützter Witness eingerichtet ist (ein
/// vertrauenswürdiger Ort), auch wenn sein Socket fehlt.
#[cfg(unix)]
fn protected_home(root: &Path) -> bool {
    root.canonicalize()
        .is_ok_and(|root| crate::enable_protected::trusted_home(&root).is_some())
}

#[cfg(not(unix))]
fn protected_home(_: &Path) -> bool {
    false
}

/// Wie `sign` am geschützten Witness signiert — beide Berührungen, die
/// Freigabe des Ankers und die der Aktivierung: ein root-eigenes
/// `ssh-keygen` außerhalb des Checkouts (wie `spec sign` — eins, das der
/// Agent in den `PATH` gelegt hat, sähe die Berührung des Menschen und
/// könnte sie für einen anderen Text verwenden), der Text über stdin, eine
/// leere Umgebung — `SSH_SK_PROVIDER` nur, wenn er `internal` ist oder
/// eine Bibliothek, die samt aller Verzeichnisse darüber root gehört
/// (weitergereicht als ihr kanonischer Pfad).
#[cfg(unix)]
struct TouchSigner {
    program: PathBuf,
    env: Vec<(&'static str, std::ffi::OsString)>,
}

#[cfg(unix)]
impl TouchSigner {
    fn resolve(root: &Path, provider: Option<std::ffi::OsString>) -> Result<Self, &'static str> {
        let program = crate::verify_cmd::conformance::spec_program(root)
            .filter(|program| minds_attest::ssh_keygen_available_at(program))
            .ok_or(
                "no root-owned ssh-keygen outside the checkout — required to sign at the \
                 protected witness",
            )?;
        Ok(Self {
            program,
            env: trusted_provider(provider)
                .map(|provider| vec![("SSH_SK_PROVIDER", provider)])
                .unwrap_or_default(),
        })
    }

    fn sign(&self, text: &str, key: &Path, namespace: &str) -> Fallible<String> {
        let env: Vec<(&str, &std::ffi::OsStr)> = self
            .env
            .iter()
            .map(|(name, value)| (*name, value.as_os_str()))
            .collect();
        Ok(minds_attest::ssh_sign_ns_presence_isolated(
            &self.program,
            text,
            key,
            namespace,
            &env,
        )?)
    }
}

/// `SSH_SK_PROVIDER`, wenn er vertraut werden kann: `internal` oder der
/// kanonische Pfad einer root-eigenen Bibliothek. Sonst keiner — ein
/// Sicherheitsschlüssel lässt sich dann (mit Apples `ssh-keygen`) nicht
/// abgeschirmt nutzen; `sign` sagt es.
#[cfg(unix)]
fn trusted_provider(provider: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let provider = provider?;
    if provider == "internal" {
        return Some(provider);
    }
    let real = Path::new(&provider)
        .is_absolute()
        .then(|| std::fs::canonicalize(&provider).ok())
        .flatten()
        .filter(|real| crate::verify_cmd::conformance::root_owned_chain(real));
    if real.is_none() {
        eprintln!(
            "minds: SSH_SK_PROVIDER is not root-owned — not used for signing at the protected \
             witness (a security key then needs a root-owned provider; a software key works)"
        );
    }
    real.map(PathBuf::into_os_string)
}

/// Der abgeschirmte Signierer für den geschützten Witness.
#[cfg(unix)]
fn shielded_signer(root: &Path) -> Fallible<TouchSigner> {
    Ok(TouchSigner::resolve(
        root,
        std::env::var_os("SSH_SK_PROVIDER"),
    )?)
}

#[cfg(not(unix))]
fn shielded_signer(_: &Path) -> Fallible<TouchSigner> {
    Err("the protected witness is not supported on this platform".into())
}

/// Ohne Unix gibt es keinen geschützten Witness.
#[cfg(not(unix))]
struct TouchSigner;

#[cfg(not(unix))]
impl TouchSigner {
    fn sign(&self, _: &str, _: &Path, _: &str) -> Fallible<String> {
        Err("the protected witness is not supported on this platform".into())
    }
}

/// Aktiviert den Anker auf dem vorab bestimmten Weg und sagt, auf welchem
/// (`witness (container)` oder die lokale Datei). `touch`: Der Anker wurde
/// für die Freigabe per Berührung signiert.
fn activate(
    ctx: &Context,
    route: Route,
    anchor: &str,
    signature: &str,
    touch: Option<&TouchSigner>,
    key: &Path,
) -> Fallible<String> {
    match route {
        Route::ProtectedPassword => {
            eprintln!(
                "minds: the witness socket is not reachable in this session yet (log in again \
                 for your new group) — activating with your password"
            );
            activate_with_password(&ctx.root, anchor, signature)
        }
        Route::Protected { socket, peer } => match touch {
            Some(signer) => {
                activate_protected(&ctx.root, &socket, peer, anchor, signature, signer, key)
            }
            None => Err("no shielded signer for the protected witness".into()),
        },
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

/// Aktiviert am geschützten Witness mit einer zweiten Berührung — wenn ein
/// Sicherheitsschlüssel mit Berührung signierte (die Signatur selbst sagt
/// es, nicht die `.pub`) — oder, wenn es dazu nicht kommt (kein Schlüssel
/// festgehalten oder ein anderer, ein älterer Witness, der Socket belegt,
/// der Witness gebremst), mit dem Passwort.
#[cfg(unix)]
fn activate_protected(
    root: &Path,
    socket: &Path,
    peer: u32,
    anchor: &str,
    signature: &str,
    signer: &TouchSigner,
    key: &Path,
) -> Fallible<String> {
    // Nur mit Zähler: Ohne ihn lehnte der Witness die zweite Berührung ab —
    // sie wäre vergeudet.
    let touched = minds_attest::signature_key(signature).is_some_and(|signed| {
        minds_attest::is_security_key_type(&signed.key_type) && signed.user_presence
    }) && minds_attest::signature_counter(signature)
        .is_some_and(|counter| counter > 0);
    if !touched {
        return activate_with_password(root, anchor, signature);
    }
    let mut sign = |text: &str, namespace: &str| signer.sign(text, key, namespace);
    let outcome = activate_with_touch(socket, peer, anchor, signature, &mut sign, THROTTLE_BUDGET)?;
    match after_touch(outcome) {
        Next::Done(route) => Ok(route),
        Next::Password(why) => {
            eprintln!("minds: {why} — activating with your password instead");
            activate_with_password(root, anchor, signature)
        }
        Next::Fail(error) => Err(error.into()),
    }
}

/// Was nach der Freigabe per Berührung folgt.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Aktiviert; der Weg für die Ausgabe.
    Done(String),
    /// Weiter mit dem Passwort — warum.
    Password(String),
    /// Abbrechen, mit Grund.
    Fail(String),
}

/// Entscheidet nach der Freigabe per Berührung. Das Passwort ist ein eigener
/// Beleg: Wo die Berührung nicht zum Ziel kommt, bleibt dieser Weg — außer
/// der Zählerstand des Witness ist kaputt (dann scheiterte auch er) oder der
/// Witness ist nach der Berührung unerreichbar.
#[cfg(unix)]
fn after_touch(outcome: Touch) -> Next {
    use crate::intent_proof::{
        COUNTER_UNREADABLE, NO_INTENT_KEY, NOT_CHECKABLE, NOT_PINNED_PROOF, NOT_PINNED_SIGNATURE,
        NOT_RECORDABLE,
    };
    match outcome {
        // Kann der Witness schon die Nonce nicht ausgeben (`witness.json`
        // unlesbar), scheiterte auch der Weg über das Passwort daran — kein
        // vergebliches Passwort.
        Touch::NoChallenge(Refusal::Refused(reason)) if reason == NOT_CHECKABLE => {
            Next::Fail(format!(
                "the witness cannot check approvals right now ({reason}) — as the witness user, \
                 check witness.json in its home"
            ))
        }
        Touch::Activated => Next::Done("witness (protected, security key)".to_owned()),
        Touch::NoChallenge(Refusal::Refused(reason)) if reason == NO_INTENT_KEY => Next::Password(
            "the protected witness has no usable security key pinned (set minds.intentKey and \
             rerun `MINDS_WITNESS=protected minds enable`; a key pinned before is replaced only \
             with `minds witness init --replace-intent-key`, see the docs)"
                .to_owned(),
        ),
        Touch::NoChallenge(Refusal::Refused(reason)) => {
            Next::Password(format!("the protected witness gave no nonce ({reason})"))
        }
        Touch::NoChallenge(Refusal::Unreachable(kind)) => Next::Password(format!(
            "the protected witness gave no nonce ({kind}; an older witness? rerun \
             `MINDS_WITNESS=protected minds enable` to update it)"
        )),
        Touch::Busy => Next::Password(
            "the protected witness kept deferring the check (another process is flooding it \
             with approvals)"
                .to_owned(),
        ),
        Touch::Late => Next::Password(
            "the second touch came too late (the witness's nonce is valid for two minutes)"
                .to_owned(),
        ),
        Touch::Failed(Refusal::Refused(reason))
            if reason == NOT_PINNED_PROOF || reason == NOT_PINNED_SIGNATURE =>
        {
            Next::Password("this key is not the one pinned at the protected witness".to_owned())
        }
        // Nach der Berührung nicht prüfbar (kein `ssh-keygen`, Frist): Der
        // Weg über das Passwort prüfte dieselbe Signatur genauso.
        Touch::Failed(Refusal::Refused(reason)) if reason == NOT_CHECKABLE => Next::Fail(format!(
            "the witness could not check the approval ({reason}) — its ssh-keygen; retry \
                 minds intent sign"
        )),
        Touch::Failed(Refusal::Refused(reason)) if reason == COUNTER_UNREADABLE => {
            Next::Fail(format!(
                "the witness refused it: {reason} — as the witness user, inspect or remove \
                 evidence/intent-counter in its home"
            ))
        }
        Touch::Failed(Refusal::Refused(reason)) if reason == NOT_RECORDABLE => Next::Fail(format!(
            "the witness refused it: {reason} — it could not write to its home; check the \
                 witness home (evidence/, free space)"
        )),
        Touch::Failed(Refusal::Refused(reason)) => {
            Next::Password(format!("the witness refused the touch approval ({reason})"))
        }
        Touch::Failed(Refusal::Unreachable(kind)) => {
            Next::Fail(format!("the protected witness is not reachable ({kind})"))
        }
    }
}

#[cfg(not(unix))]
fn activate_protected(
    _: &Path,
    _: &Path,
    _: u32,
    _: &str,
    _: &str,
    _: &TouchSigner,
    _: &Path,
) -> Fallible<String> {
    Err("the protected witness is not supported on this platform".into())
}

#[cfg(not(unix))]
fn activate_with_password(_: &Path, _: &str, _: &str) -> Fallible<String> {
    Err("the protected witness is not supported on this platform".into())
}

/// Wie die Freigabe per Berührung ausging.
#[cfg(unix)]
#[derive(Debug)]
enum Touch {
    Activated,
    /// Keine Nonce — vor jeder Berührung; der Weg über das Passwort bleibt.
    NoChallenge(Refusal),
    /// Der Witness bremste die Prüfung länger als [`THROTTLE_BUDGET`].
    Busy,
    /// Die zweite Berührung kam nach [`TOUCH_WINDOW`].
    Late,
    /// Nach der Berührung abgelehnt oder abgebrochen.
    Failed(Refusal),
}

/// So lange nach der Nonce darf die zweite Berührung dauern — knapp unter
/// der Frist des Witness (120 s), damit eine verspätete Freigabe hier einen
/// klaren Grund bekommt statt einer geschlossenen Verbindung.
#[cfg(unix)]
const TOUCH_WINDOW: std::time::Duration = std::time::Duration::from_secs(110);

/// So lange schickt `sign` eine gebremste Freigabe erneut, bevor es auf das
/// Passwort ausweicht.
#[cfg(unix)]
const THROTTLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Die Pause, bevor eine gebremste Freigabe erneut geht.
#[cfg(unix)]
const THROTTLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(300);

/// Nonce holen, Anker-Id, Anker-Signatur und Nonce mit Berührung signieren
/// (`sign`: Text, Namespace), auf derselben Verbindung aktivieren —
/// gebremst, dieselbe Freigabe erneut, höchstens `budget` lang. `Err` nur,
/// wenn das Signieren scheitert.
#[cfg(unix)]
fn activate_with_touch(
    socket: &Path,
    peer: u32,
    anchor: &str,
    signature: &str,
    sign: &mut dyn FnMut(&str, &str) -> Fallible<String>,
    budget: std::time::Duration,
) -> Fallible<Touch> {
    use crate::hook::witness::Conversation;
    use minds_capture::witness_proto::{Frame, NONCE_HEX};
    use std::time::Instant;

    let answer = |answer: Result<Frame, &'static str>| match answer {
        Ok(Frame::Ack { status, .. }) => Ok(status),
        Ok(Frame::Nack { reason, .. }) => Err(Refusal::Refused(crate::text::sanitize(&reason))),
        Ok(_) => Err(Refusal::Unreachable("unexpected response")),
        Err(kind) => Err(Refusal::Unreachable(kind)),
    };
    let mut conversation =
        match Conversation::open(socket, Some(peer), Instant::now() + ACTIVATE_TIMEOUT) {
            Ok(conversation) => conversation,
            Err(kind) => return Ok(Touch::NoChallenge(Refusal::Unreachable(kind))),
        };
    let challenge = Frame::IntentChallenge {
        request_id: request_id(),
    };
    let status = match answer(conversation.request(&challenge, Instant::now() + ACTIVATE_TIMEOUT)) {
        Ok(status) => status,
        Err(refusal) => return Ok(Touch::NoChallenge(refusal)),
    };
    let issued = Instant::now();
    let Some(nonce) = status.strip_prefix("nonce ").filter(|nonce| {
        nonce.len() == NONCE_HEX
            && nonce
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }) else {
        return Ok(Touch::NoChallenge(Refusal::Unreachable(
            "unexpected response",
        )));
    };
    let id = minds_core::intent_anchor::IntentAnchor::id_of_text(anchor);
    let message = crate::intent_proof::activation_message(id.as_str(), signature, nonce);
    eprintln!("Touch your security key again to activate the intent at the protected witness.");
    let proof = sign(&message, minds_attest::NS_INTENT_ACTIVATE)?;
    if issued.elapsed() >= TOUCH_WINDOW {
        return Ok(Touch::Late);
    }
    let frame = Frame::IntentActivateProven {
        anchor: anchor.to_owned(),
        signature: Some(signature.to_owned()),
        nonce: nonce.to_owned(),
        proof,
        request_id: request_id(),
    };
    let sent = Instant::now();
    let mut told = false;
    loop {
        match answer(conversation.request(&frame, Instant::now() + ACTIVATE_TIMEOUT)) {
            Ok(_) => return Ok(Touch::Activated),
            Err(Refusal::Refused(reason)) if reason == crate::intent_proof::THROTTLED => {
                if sent.elapsed() + THROTTLE_PAUSE >= budget
                    || issued.elapsed() + THROTTLE_PAUSE >= TOUCH_WINDOW
                {
                    return Ok(Touch::Busy);
                }
                if !told {
                    eprintln!("minds: the protected witness is busy — retrying …");
                    told = true;
                }
                std::thread::sleep(THROTTLE_PAUSE);
            }
            Err(refusal) => return Ok(Touch::Failed(refusal)),
        }
    }
}

/// Aktiviert als Witness-Nutzer (`minds witness activate` mit der einen
/// Erhöhung); das Passwort des Menschen ist der Beleg.
#[cfg(unix)]
fn activate_with_password(root: &Path, anchor: &str, signature: &str) -> Fallible<String> {
    let user = crate::enable_protected::WITNESS_USER;
    let request = serde_json::to_vec(&serde_json::json!({
        "anchor": anchor,
        "signature": signature,
    }))?;
    eprintln!(
        "minds: activating the intent at the protected witness as {user} — your password approves it"
    );
    let output = crate::enable_protected::activate_elevated(&root.canonicalize()?, &request)?;
    if !output.status.success() {
        return Err(format!(
            "the activation as {user} did not succeed ({}) — see the lines above; if the \
             witness binary is older than this minds, rerun `MINDS_WITNESS=protected minds \
             enable`",
            output.status
        )
        .into());
    }
    Ok("witness (protected, approved with your password)".to_owned())
}

/// Warum der Witness nicht aktiviert hat.
#[derive(Debug)]
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
    match crate::hook::witness::request(socket, None, &frame, deadline) {
        Ok(Frame::Ack { .. }) => Ok(()),
        // Der Grund kommt vom Witness, aber über einen Socket: entschärft.
        Ok(Frame::Nack { reason, .. }) => Err(Refusal::Refused(crate::text::sanitize(&reason))),
        Ok(_) => Err(Refusal::Unreachable("unexpected response")),
        Err(kind) => Err(Refusal::Unreachable(kind)),
    }
}

/// Eine Anfrage-Id — eindeutig, nicht geheim (wie beim Checkpoint).
pub(crate) fn request_id() -> [u8; 16] {
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

    /// Ein Stand-in für den geschützten Witness: beantwortet die Nonce-Frage
    /// mit `nonce`, jede Freigabe mit der nächsten Antwort aus `answers`;
    /// liefert die empfangenen Freigaben zurück.
    #[cfg(unix)]
    fn stand_in(
        challenge: minds_capture::witness_proto::Frame,
        answers: Vec<&'static str>,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::thread::JoinHandle<Vec<minds_capture::witness_proto::Frame>>,
    ) {
        use minds_capture::witness_proto::{self, Frame};
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("w.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut bytes = Vec::new();
            let mut answers = answers.into_iter();
            let mut chunk = [0u8; 4096];
            loop {
                let frame = loop {
                    if let Ok((frame, used)) = witness_proto::decode(&bytes) {
                        bytes.drain(..used);
                        break Some(frame);
                    }
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    }
                };
                let Some(frame) = frame else {
                    return seen;
                };
                let answer = match &frame {
                    Frame::IntentChallenge { request_id } => match &challenge {
                        Frame::Ack { status, .. } => Frame::Ack {
                            request_id: *request_id,
                            status: status.clone(),
                        },
                        Frame::Nack { reason, .. } => Frame::Nack {
                            request_id: *request_id,
                            reason: reason.clone(),
                        },
                        _ => unreachable!(),
                    },
                    Frame::IntentActivateProven { request_id, .. } => {
                        let request_id = *request_id;
                        seen.push(frame.clone());
                        match answers.next().unwrap_or("intent active b3-x") {
                            ok if ok.starts_with("intent active") => Frame::Ack {
                                request_id,
                                status: ok.into(),
                            },
                            reason => Frame::Nack {
                                request_id,
                                reason: reason.into(),
                            },
                        }
                    }
                    other => panic!("unexpected {other:?}"),
                };
                stream
                    .write_all(&witness_proto::encode(&answer).unwrap())
                    .unwrap();
            }
        });
        (dir, socket, handle)
    }

    #[cfg(unix)]
    fn nonce_ack() -> minds_capture::witness_proto::Frame {
        minds_capture::witness_proto::Frame::Ack {
            request_id: [0; 16],
            status: format!("nonce {}", "c3".repeat(32)),
        }
    }

    #[cfg(unix)]
    fn me() -> u32 {
        // SAFETY: geteuid hat keine Vorbedingungen.
        unsafe { libc::geteuid() }
    }

    /// Die Freigabe per Berührung: Nonce, Signatur über Anker-Id,
    /// Anker-Signatur und Nonce; gebremst dieselbe Freigabe erneut, bis
    /// der Witness sie annimmt.
    #[cfg(unix)]
    #[test]
    fn a_touch_approval_is_resent_while_the_witness_defers_it() {
        use minds_capture::witness_proto::Frame;
        let (_dir, socket, handle) = stand_in(
            nonce_ack(),
            vec![crate::intent_proof::THROTTLED, "intent active b3-x"],
        );
        let mut signed = Vec::new();
        let mut sign = |text: &str, namespace: &str| -> Fallible<String> {
            signed.push((text.to_owned(), namespace.to_owned()));
            Ok("PROOF".to_owned())
        };
        let outcome = activate_with_touch(
            &socket,
            me(),
            "ANCHOR",
            "SIG",
            &mut sign,
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(matches!(outcome, Touch::Activated), "{outcome:?}");
        let id = minds_core::intent_anchor::IntentAnchor::id_of_text("ANCHOR");
        assert_eq!(
            signed,
            [(
                crate::intent_proof::activation_message(id.as_str(), "SIG", &"c3".repeat(32)),
                minds_attest::NS_INTENT_ACTIVATE.to_owned()
            )]
        );
        let frames = handle.join().unwrap();
        assert_eq!(frames.len(), 2, "sent again after the throttle");
        assert_eq!(frames[0], frames[1]);
        assert!(matches!(
            &frames[0],
            Frame::IntentActivateProven { anchor, signature: Some(signature), proof, .. }
                if anchor == "ANCHOR" && signature == "SIG" && proof == "PROOF"
        ));
    }

    /// Bremst der Witness länger als das Budget: `Busy` (dann das
    /// Passwort). Ohne festgehaltenen Schlüssel: keine Nonce, keine
    /// Berührung. Ein anderer Schlüssel: abgelehnt (dann das Passwort).
    #[cfg(unix)]
    #[test]
    fn a_touch_approval_gives_way_to_the_password() {
        use minds_capture::witness_proto::Frame;
        let never = |_: &str, _: &str| -> Fallible<String> { panic!("no touch without a nonce") };

        let (_dir, socket, handle) =
            stand_in(nonce_ack(), vec![crate::intent_proof::THROTTLED; 50]);
        let mut sign = |_: &str, _: &str| -> Fallible<String> { Ok("PROOF".to_owned()) };
        let outcome = activate_with_touch(
            &socket,
            me(),
            "ANCHOR",
            "SIG",
            &mut sign,
            std::time::Duration::from_millis(700),
        )
        .unwrap();
        assert!(matches!(outcome, Touch::Busy), "{outcome:?}");
        drop(handle);

        let (_dir, socket, _handle) = stand_in(
            Frame::Nack {
                request_id: [0; 16],
                reason: crate::intent_proof::NO_INTENT_KEY.into(),
            },
            vec![],
        );
        let outcome = activate_with_touch(
            &socket,
            me(),
            "ANCHOR",
            "SIG",
            &mut { never },
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert!(
            matches!(&outcome, Touch::NoChallenge(Refusal::Refused(reason))
                if reason == crate::intent_proof::NO_INTENT_KEY),
            "{outcome:?}"
        );

        let (_dir, socket, _handle) =
            stand_in(nonce_ack(), vec![crate::intent_proof::NOT_PINNED_PROOF]);
        let mut sign = |_: &str, _: &str| -> Fallible<String> { Ok("PROOF".to_owned()) };
        let outcome = activate_with_touch(
            &socket,
            me(),
            "ANCHOR",
            "SIG",
            &mut sign,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert!(
            matches!(&outcome, Touch::Failed(Refusal::Refused(reason))
                if reason == crate::intent_proof::NOT_PINNED_PROOF),
            "{outcome:?}"
        );
    }

    /// Nach der Berührung: Wohin jeder Ausgang führt — der Weg über das
    /// Passwort bleibt, außer der Zählerstand des Witness ist kaputt oder er
    /// ist nach der Berührung unerreichbar.
    #[cfg(unix)]
    #[test]
    fn after_a_touch_every_outcome_has_its_route() {
        use crate::intent_proof::{
            COUNTER_UNREADABLE, NO_INTENT_KEY, NOT_PINNED_PROOF, NOT_PINNED_SIGNATURE,
            NOT_RECORDABLE, THROTTLED,
        };
        let refused = |reason: &str| Refusal::Refused(reason.to_owned());
        assert_eq!(
            after_touch(Touch::Activated),
            Next::Done("witness (protected, security key)".to_owned())
        );
        for outcome in [
            Touch::NoChallenge(refused(NO_INTENT_KEY)),
            Touch::NoChallenge(refused("too many open approvals at this witness")),
            Touch::NoChallenge(Refusal::Unreachable("connection closed")),
            Touch::Busy,
            Touch::Late,
            Touch::Failed(refused(NOT_PINNED_PROOF)),
            Touch::Failed(refused(NOT_PINNED_SIGNATURE)),
            Touch::Failed(refused(THROTTLED)),
            Touch::Failed(refused(
                "this intent approval was already used for an activation — sign again",
            )),
        ] {
            let label = format!("{outcome:?}");
            assert!(matches!(after_touch(outcome), Next::Password(_)), "{label}");
        }
        for (outcome, hint) in [
            (
                Touch::Failed(refused(COUNTER_UNREADABLE)),
                "evidence/intent-counter",
            ),
            (Touch::Failed(refused(NOT_RECORDABLE)), "free space"),
            (
                Touch::NoChallenge(refused(crate::intent_proof::NOT_CHECKABLE)),
                "witness.json",
            ),
            (
                Touch::Failed(refused(crate::intent_proof::NOT_CHECKABLE)),
                "retry minds intent sign",
            ),
            (
                Touch::Failed(Refusal::Unreachable("connection closed")),
                "not reachable",
            ),
        ] {
            let label = format!("{outcome:?}");
            assert!(
                matches!(after_touch(outcome), Next::Fail(error) if error.contains(hint)),
                "{label}"
            );
        }
    }

    /// `SSH_SK_PROVIDER` geht nur als `internal` oder als kanonischer Pfad
    /// einer root-eigenen Datei weiter — nie relativ, nie eine Datei des
    /// Nutzers, und ein Symlink wird zu seinem Ziel.
    #[cfg(unix)]
    #[test]
    fn only_a_root_owned_security_key_provider_is_passed_on() {
        use std::ffi::OsString;
        assert_eq!(
            trusted_provider(Some("internal".into())),
            Some(OsString::from("internal"))
        );
        assert_eq!(trusted_provider(None), None);
        assert_eq!(trusted_provider(Some("libsk.dylib".into())), None);
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("libsk.dylib");
        std::fs::write(&own, b"").unwrap();
        assert_eq!(trusted_provider(Some(own.clone().into_os_string())), None);
        // Eine root-eigene Datei (als Stand-in für eine Bibliothek) über
        // einen Symlink des Nutzers: weiter geht ihr kanonischer Pfad.
        let real = std::fs::canonicalize("/bin/sh").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            trusted_provider(Some(link.into_os_string())),
            Some(real.into_os_string())
        );
    }

    /// Der geschützte Witness geht vor; erreicht diese Sitzung seinen
    /// Socket noch nicht (neue Gruppe), bleibt der Weg über das Passwort;
    /// ein stummer ist ein Fehler vor dem Signieren; ohne ihn die anderen
    /// Wege.
    #[test]
    fn the_protected_witness_route_follows_its_state() {
        let found = || {
            Some((
                PathBuf::from("/var/lib/minds-witness/x/run/witness.sock"),
                498,
            ))
        };
        assert!(matches!(
            protected_route(ProtectedState::Reachable, found()),
            Some(Ok(Route::Protected { peer: 498, .. }))
        ));
        assert!(matches!(
            protected_route(ProtectedState::GroupPending, None),
            Some(Ok(Route::ProtectedPassword))
        ));
        let silent = protected_route(ProtectedState::Silent, found());
        assert!(matches!(&silent, Some(Err(err)) if err.to_string().contains("did not answer")),);
        assert!(protected_route(ProtectedState::Absent, None).is_none());
    }

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
