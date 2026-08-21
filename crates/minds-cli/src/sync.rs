//! `minds sync` — der Kontext reist mit dem Code, in **einer** Verbindung.
//!
//! Der `pre-push`-Hook rief bisher ein zweites `git push` auf. Das kostete auf
//! jedem Push den vollen Verbindungsaufbau — gegen gitlab.com gemessen ~2,7 s,
//! und zwar unabhängig davon, ob es überhaupt etwas zu schicken gab. Bei einer
//! Agent-Flotte, die im Minutentakt pusht, ist das der teuerste Teil des
//! Vorgangs.
//!
//! Dieses Modul ersetzt den Hook durch drei Regeln:
//!
//! 1. **Kein Netz, wenn nichts neu ist.** Was am Remote steht, wissen wir aus
//!    eigenen Tracking-Refs (siehe unten) — der Soll-Ist-Vergleich ist rein
//!    lokal. Ein Push ohne neuen Kontext kostet damit einen Prozessstart, keine
//!    Verbindung.
//! 2. **Ein Push für alle Refs.** Kontext, Reviews und Session-Refs gehen als
//!    Refspec-Liste in *einen* `git push`. N Refs kosten eine Verbindung, nicht
//!    N.
//! 3. **Nie `--force`** — mit genau einer, eng gefassten Ausnahme. Wird ein Ref
//!    abgewiesen, weil zwei Maschinen ihn fortgeschrieben haben, wird der fremde
//!    Stand geholt und **vereinigt** (der Thread-Log ist konfliktfrei mergebar,
//!    siehe [`ReviewStore::merge_from`]), dann erneut gepusht — wieder
//!    fast-forward. Was sich nicht vereinigen lässt, bleibt liegen. Der Remote
//!    wird nie überschrieben. Die Ausnahme ist die Übertragung einer
//!    DSGVO-Löschung (#102): Trägt ein session-exklusiver Ref lokal nachweislich
//!    einen Tombstone ([`minds_store::tombstone_at`]), geht genau dieser Ref mit
//!    einer `+`-Refspec — sonst behielte die Forge den Klartext einer gelöschten
//!    Session als aktuelle, browsbare Ref-Spitze. Nie Klartext über Klartext;
//!    jeder andere Ref bleibt strikt fast-forward.
//!
//! # Warum synchron und nicht im Hintergrund
//!
//! Ein abgelöster Hintergrundprozess wäre schneller — er hat aber **kein
//! Terminal**. Credential-Helper, SSH-Passphrase und der Touch eines
//! Security-Keys brauchen genau das. Ein Sync, der im Hintergrund still an der
//! Authentifizierung scheitert, ist schlimmer als einer, der zwei Sekunden
//! braucht und es sagt. Deshalb läuft der Push im Vordergrund, meldet seinen
//! Fortschritt auf **stdout** (Git zeigt die Ausgabe des Hooks) — und tut in dem
//! häufigen Fall, dass nichts neu ist, gar nichts.
//!
//! # Welche Meldung auf welchen Kanal geht
//!
//! Der `pre-push`-Hook wirft stderr weg, weil dort sonst rohe Git-Fehler
//! zwischen den Zeilen von `git push` stünden (#10). Damit wird die Wahl des
//! Kanals zur Entscheidung darüber, was der Nutzer beim Push noch sieht:
//!
//! - **stdout** — Fortschritt und Ergebnis: was geschickt wurde, wie viele
//!   fremde Verdicts übernommen wurden. Das gehört zu dem Push, bei dem es
//!   entsteht, und ist keine Fehlermeldung.
//! - **stderr + [`crate::hooklog`]** — jeder Fehlschlag. Im Terminal für den,
//!   der `minds sync` von Hand aufruft; in der Datei für den, dessen Hook ihn
//!   gerade verschluckt hat.
//!
//! # Die Tracking-Refs
//!
//! Für `refs/heads/*` führt Git selbst Buch (`refs/remotes/origin/*`); für
//! `refs/minds/*` tut es das nicht. Also führen wir es selbst: Nach einem
//! bestätigten Push steht der gepushte Stand unter
//! `refs/minds/remotes/<remote>/<rest>`. Das ist bewusst **kein** Zustand neben
//! Git — es sind Refs, sie überleben `git gc`, sie lassen sich mit `git
//! for-each-ref` ansehen, und geht einer verloren, ist die Folge ein
//! überflüssiger, idempotenter Push. Kein Journal, das kaputtgehen kann.
//!
//! # Fail-soft
//!
//! Nichts hier darf den Push des Nutzers scheitern lassen. Jeder Fehler wird
//! gemeldet und geschluckt; der Rückgabewert ist auch dann 0. Wer den Sync von
//! Hand aufruft und den Fehler sehen will, nimmt `-v`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime};

use minds_git::{CommitId, MINDS_REF_NAMESPACE, Repo};
use minds_store::{Backend, DEFAULT_REVIEW_REF, ReviewStore, TRACKING_REF_PREFIX, tombstone_at};

use crate::config;
use crate::hooklog::{self, Source};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Unterhalb dieses Präfix liegen die eigenen Tracking-Refs. Sie werden nie
/// gepusht — sie sind die Buchhaltung *über* das Pushen.
///
/// Die Konstante lebt in `minds-store` ([`TRACKING_REF_PREFIX`]), weil `forget`
/// dieselben Refs mit-tilgen muss (#14): Store und Sync teilen sich damit eine
/// einzige Wahrheit über den Namensraum.
const TRACKING_PREFIX: &str = TRACKING_REF_PREFIX;

/// Bricht die Rekursion: Der Push dieses Moduls löst im selben Repo erneut
/// `pre-push` aus. `--no-verify` verhindert das bereits; die Umgebungsvariable
/// ist der Gürtel zum Hosenträger (und greift auch, wenn jemand den Hook von
/// Hand aufruft).
const GUARD_ENV: &str = "MINDS_SYNCING";

/// Schlüssel in `.git/config`: `false` schaltet den Sync ab.
const KEY_SYNC: &str = "minds.sync";

/// Nach dieser Zeit gilt ein Lock als verwaist (ein abgestürzter Sync soll den
/// nächsten nicht dauerhaft blockieren).
const LOCK_STALE: Duration = Duration::from_secs(300);

/// Der Remote, wenn der Hook keinen nennt.
const DEFAULT_REMOTE: &str = "origin";

/// Führt `minds sync` aus. Endet **immer** mit 0, wenn es aus dem Hook kommt —
/// siehe Modul-Doku.
pub fn run(remote: Option<&str>, verbose: bool) -> ExitCode {
    if std::env::var_os(GUARD_ENV).is_some() {
        return ExitCode::SUCCESS;
    }
    hooklog::guarded(Source::Sync, || {
        sync_or_report(remote.unwrap_or(DEFAULT_REMOTE), verbose)
    })
}

fn sync_or_report(remote: &str, verbose: bool) -> ExitCode {
    match sync(remote, verbose) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Seit der pre-push-Hook stderr nach `/dev/null` schickt (statt sie
            // roh in den Push-Output zu kippen), ist die Datei hier der einzige
            // Ort, an dem ein gescheiterter Sync noch auftaucht.
            hooklog::report(Source::Sync, &err.to_string());
            ExitCode::FAILURE
        }
    }
}

fn sync(remote: &str, verbose: bool) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let repo = Repo::discover(&cwd)?;
    let git_dir = repo.git_dir().to_path_buf();
    let root = repo_root(&repo);

    if !enabled(&root) {
        vln(verbose, "minds sync: durch minds.sync abgeschaltet");
        return Ok(());
    }

    // Was ein `git fetch` an fremden Verdicts mitgebracht hat, liegt im
    // Tracking-Namensraum und wird hier vereinigt — lokal, ohne Netz, idempotent.
    // Erst danach steht fest, was zu pushen ist.
    merge_incoming(&root, &git_dir, remote, verbose);

    let (jobs, deferred) = plan(&root, &repo, remote)?;

    // Zurückgestellte, non-fast-forward Refs (siehe `due`): ohne `--force` nicht
    // pushbar. Der `forget`-Fall landet hier nicht — ein verifizierter Tombstone
    // geht als gezielter Force-Push mit (#102) —, hier bleibt echte Divergenz.
    // Gezählt werden **Refs**, nicht Sessions. Der Hinweis geht auf stdout
    // (sichtbar beim Push) **und** ins Log, weil ein nicht übertragbarer Ref
    // sichtbar bleiben soll, bis er aufgelöst ist.
    if !deferred.is_empty() {
        ProgressLine::write(&format!(
            "minds: {} divergierte(r) Ref(s) nicht übertragen \
             — non-fast-forward, ohne `--force` nicht pushbar\n",
            deferred.len()
        ));
        hooklog::log_at(
            &git_dir,
            Source::Sync,
            &format!(
                "{} divergierte(r) Ref(s) non-fast-forward, nicht synchronisiert: {}",
                deferred.len(),
                deferred.join(", ")
            ),
        );
    }

    if jobs.iter().all(|job| job.updates.is_empty()) {
        // Der häufige Fall. Bis hierher wurde keine Verbindung geöffnet.
        vln(verbose, "minds sync: nichts Neues");
        return Ok(());
    }

    let Some(_lock) = Lock::acquire(&git_dir)? else {
        // Ein zweiter Agent pusht gerade. Der ist entweder schon weiter als wir
        // oder wird gleich neu planen; ein zweiter Verbindungsaufbau brächte
        // nichts.
        vln(verbose, "minds sync: läuft bereits");
        return Ok(());
    };

    for job in jobs {
        if job.updates.is_empty() {
            continue;
        }
        if let Err(err) = job.execute(&git_dir, verbose) {
            // Fail-soft: Der Push des Nutzers läuft weiter, die Refs bleiben
            // ungetrackt und werden beim nächsten Mal erneut angeboten.
            //
            // Und genau deshalb muss es hier ins Log: Das ist der *häufige*
            // Sync-Fehler — kein Zugriff aufs Remote —, und weil er fail-soft
            // behandelt wird, endet `minds sync` trotzdem mit 0. Über den
            // Rückgabewert erfährt ihn niemand, über den pre-push-Hook seit der
            // stderr-Umleitung auch nicht.
            let note = format!(
                "Kontext-Sync ({}) nicht möglich: {err}",
                display(&job.label)
            );
            hooklog::report_at(&git_dir, Source::Sync, &note);
            // Und eine Zeile auf dem Kanal, der den Hook überlebt — ohne
            // Wortlaut, aber mit dem Weg dorthin. Sonst wäre der Unterschied
            // zwischen „fertig" und „gescheitert" für den Nutzer beim Push nur
            // die *Abwesenheit* eines Wortes, und das sieht niemand. Hier
            // gebündelt statt an jedem Rückweg in `execute`/`reconcile`.
            // Über denselben panikfreien Weg wie die Fortschrittszeile: Ein
            // geschlossenes stdout (`git push … | head`) ließe `println!`
            // panicken, `guarded` schriebe daraufhin einen Panic-Eintrag — ein
            // `fsck`-Hinweis aus einer völlig harmlosen Bedingung.
            ProgressLine::write(
                "minds: Kontext-Sync nicht möglich — `minds fsck` sagt, wo der Grund steht\n",
            );
        }
    }
    Ok(())
}

/// Ob der Sync eingeschaltet ist (`minds.sync`, Default `true`).
fn enabled(root: &Path) -> bool {
    !matches!(
        git_output(root, &["config", "--local", "--get", KEY_SYNC])
            .unwrap_or_default()
            .trim(),
        "false" | "no" | "0" | "off"
    )
}

// ---------------------------------------------------------------------------
// Der Plan — rein lokal
// ---------------------------------------------------------------------------

/// Ein Ref, der zu schicken ist: was, wohin, und wo der Erfolg vermerkt wird.
#[derive(Debug, PartialEq, Eq)]
struct Update {
    /// Der lokale Ref (nur für die Anzeige).
    local: String,
    /// Der Commit, der geschickt wird. Bewusst der Hash und nicht der Ref-Name:
    /// So landet am Remote genau das, was hier geplant wurde, auch wenn ein
    /// paralleler Checkpoint den Ref inzwischen weiterschiebt.
    oid: String,
    /// Der Ziel-Ref am Remote.
    destination: String,
    /// Der Tracking-Ref, der nach dem Erfolg auf `oid` gesetzt wird.
    tracking: String,
    /// Ob dieser Ref mit einer `+`-Refspec geht — die eine Ausnahme vom
    /// „nie `--force`": die Übertragung einer DSGVO-Löschung. Gesetzt wird das
    /// Flag nur in [`due`], und nur wenn der zu pushende Stand nachweislich ein
    /// Tombstone an einem session-exklusiven Ref ist (#102).
    force: bool,
}

/// Ein Push: ein Repository, ein Remote, alle dorthin fälligen Refs.
#[derive(Debug)]
struct Job {
    dir: PathBuf,
    remote: String,
    label: String,
    updates: Vec<Update>,
}

/// Vereinigt den vom letzten `git fetch` mitgebrachten Review-Stand in den
/// lokalen Log.
///
/// `minds enable` setzt dafür den Fetch-Refspec
/// `+refs/minds/reviews:refs/minds/remotes/<remote>/reviews`: Fremde Verdicts
/// landen im Tracking-Namensraum und **überschreiben nie** den lokalen Log.
/// Zusammengeführt wird hier — durch Vereinigung, konfliktfrei (siehe
/// [`ReviewStore::merge_from`]). Damit konvergieren zwei Maschinen ohne
/// Zutun: fetch bringt es her, sync führt es zusammen, der nächste Push
/// schickt den vereinigten Stand zurück.
fn merge_incoming(root: &Path, git_dir: &Path, remote: &str, verbose: bool) {
    // Nur für ein *benanntes* Remote. Git ruft den pre-push-Hook mit `$1 =
    // <ort>` auf, wenn jemand `git push <url>` schreibt — dann baute
    // `tracking_prefix` daraus `refs/minds/remotes/https://…/reviews`, und das
    // ist kein gültiger Ref-Name. `merge_from` scheiterte, und seit der Fehler
    // ins Log geht, entstünde bei *jedem* solchen Push ein Eintrag: ein
    // `fsck`-Hinweis, den man nur durch Löschen loswird und der sofort
    // wiederkommt. Genau das Rauschen, gegen das `log_report_lines` argumentiert.
    if !has_remote(root, remote) {
        vln(verbose, "minds sync: kein benanntes Remote — kein Merge");
        return;
    }
    let incoming = format!("{}reviews", tracking_prefix(remote));
    let merged = Repo::open(root)
        .map_err(|err| err.to_string())
        .and_then(|repo| {
            ReviewStore::new(repo)
                .merge_from(&incoming)
                .map_err(|err| err.to_string())
        });
    match merged {
        Ok(0) => {}
        Ok(count) => {
            ProgressLine::write(&format!("minds: {count} fremde(s) Verdict(s) übernommen\n"))
        }
        Err(err) => {
            // Nicht nur `vln`: Ein fehlender Tracking-Ref ist hier **kein**
            // Fehler ([`ReviewStore::merge_from`] gibt dafür `Ok(0)` zurück),
            // ein `Err` also immer einer. Und er wiegt schwer — dieser Merge
            // füllt den Review-Store, den `fsck --require-review` als CI-Gate
            // liest. Bliebe er beim Push still liegen, prüfte das Gate gegen
            // einen Stand, dem fremde Verdicts fehlen.
            let note = crate::text::without_url_credentials(&format!("Merge übersprungen: {err}"));
            vln(verbose, &format!("minds sync: {note}"));
            hooklog::log_at(git_dir, Source::Sync, &note);
        }
    }
}

/// Was zu tun ist — ohne eine einzige Netzoperation.
///
/// Der zweite Rückgabewert nennt die Refs, die nur mit `--force` gingen, es aber
/// nicht dürfen (echte Divergenz zweier Maschinen, #14) und deshalb übersprungen
/// wurden — der Aufrufer meldet sie. Getilgte Sessions zählen seit #102 nicht
/// mehr dazu: Ihr Tombstone reist als gezielter Force-Push mit den Updates.
fn plan(root: &Path, repo: &Repo, remote: &str) -> Fallible<(Vec<Job>, Vec<String>)> {
    let store = config::load(root);
    let mut jobs = Vec::new();
    let mut deferred = Vec::new();

    // 1. Das Repo des Codes → das Remote, auf das der Nutzer gerade pusht.
    //    In-Repo-Backend: Kontext, Reviews und Session-Refs. Child-Backend:
    //    nur die Reviews (die liegen immer beim Code).
    if has_remote(root, remote) {
        let (updates, mut skipped) = due(repo, remote, MINDS_REF_NAMESPACE, identity)?;
        deferred.append(&mut skipped);
        jobs.push(Job {
            dir: root.to_path_buf(),
            remote: remote.to_string(),
            label: remote.to_string(),
            updates,
        });
    }

    // 2. Das Child-Repo → dessen eigenes `origin`. Dort werden die Session-Refs
    //    bewusst auf Branches gemappt: Das Kontext-Repo *soll* sie in der
    //    Forge-Oberfläche als auswählbare Branches zeigen (siehe `enable.rs`).
    if let Backend::ChildRepo { path } = store.backend() {
        let child = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if child.is_dir() && has_remote(&child, DEFAULT_REMOTE) {
            let child_repo = Repo::open(&child)?;
            // Alles unter refs/minds/ — die Nutzlast (`store/…`) identisch, die
            // Browsing-Refs (`sessions/…`) als Branches. Welches was ist,
            // entscheidet die Abbildung.
            let (updates, mut skipped) = due(
                &child_repo,
                DEFAULT_REMOTE,
                MINDS_REF_NAMESPACE,
                session_branch,
            )?;
            deferred.append(&mut skipped);
            jobs.push(Job {
                dir: child.clone(),
                remote: DEFAULT_REMOTE.to_string(),
                label: format!("Child-Repo {}", child.display()),
                updates,
            });
        }
    }

    Ok((jobs, deferred))
}

/// Alle Refs unter `prefix`, deren Stand noch nicht am `remote` vermerkt ist —
/// und daneben die, die nur mit `--force` gingen und es nicht dürfen.
///
/// Ein Ref, dessen lokaler Stand **kein Fast-Forward** des zuletzt gepushten ist,
/// landet grundsätzlich nicht in den Updates: `minds sync` pusht nicht mit
/// `--force`, und ihn mitzuschicken ließe den ganzen (nicht-atomaren) Push
/// scheitern und die übrigen Refs ungetrackt. Solche Refs kommen als zweiter
/// Rückgabewert zurück, damit der Aufrufer sie melden kann. Das ist das
/// **Sicherheitsnetz** für echte Divergenz (zwei Maschinen).
///
/// Die eine Ausnahme ist die DSGVO-Löschung (#102): Trägt der lokale Stand
/// nachweislich einen Tombstone an einem session-exklusiven Ref
/// ([`tombstone_at`], fail-closed) — und der zuletzt gepushte Stand keinen —,
/// bekommt der Ref das `force`-Flag und geht mit `+`-Refspec. Der häufige Fall
/// dahinter: `forget` hat den Tracking-Ref gelöscht (er ankerte den Klartext),
/// der Ref erscheint hier als ungetrackt, seine Spitze ist der Tombstone — nur
/// so erreicht die Löschung eine Forge, die den Klartext noch als Ref-Spitze
/// trägt. Ein Force-Push geht damit nur „vorwärts zu einem Tombstone", nie
/// Klartext über Klartext.
fn due(
    repo: &Repo,
    remote: &str,
    prefix: &str,
    destination: fn(&str) -> String,
) -> Fallible<(Vec<Update>, Vec<String>)> {
    let tracked: BTreeMap<String, CommitId> = repo
        .refs_under(&tracking_prefix(remote))?
        .into_iter()
        .collect();

    let mut updates = Vec::new();
    let mut deferred = Vec::new();
    for (name, commit) in repo.refs_under(prefix)? {
        // Die eigene Buchhaltung wird nie gepusht.
        if name.starts_with(TRACKING_PREFIX) {
            continue;
        }
        let tracking = tracking_ref(remote, &name);
        let force = match tracked.get(&tracking) {
            // Schon auf diesem Stand vermerkt — nichts zu tun.
            Some(previous) if *previous == commit => continue,
            // Abweichend und **kein** Fast-Forward: nur pushbar, wenn es die
            // Übertragung einer Löschung ist — lokal ein Tombstone, der getrackte
            // Stand keiner (#102). Alles andere wird zurückgestellt statt den
            // ganzen Push zu reißen (#14). Ist der getrackte Commit lokal
            // weggeprunt, liefert der Revwalk `false` und `tombstone_at` für ihn
            // `None` — ein lokaler Tombstone geht dann trotzdem, denn die
            // Schutzbedingung ist der nachgewiesene Tombstone auf der *eigenen*
            // Seite; ein Nicht-Tombstone bleibt zurückgestellt (nie ungefragt
            // force).
            Some(previous) if !repo.is_ancestor(*previous, commit)? => {
                if tombstone_at(repo, &name, commit).is_some()
                    && tombstone_at(repo, &name, *previous).is_none()
                {
                    true
                } else {
                    deferred.push(name);
                    continue;
                }
            }
            // Ein sauberes Fast-Forward — regulär pushen.
            Some(_) => false,
            // Ungetrackt. Trägt der Ref einen Tombstone, hat `forget` die
            // Buchhaltung gelöscht und die Forge womöglich noch den Klartext:
            // mit `+` schicken, damit die Löschung ankommt (#102). Für einen nie
            // gepushten Ref ist die `+`-Refspec ein gewöhnliches Anlegen.
            None => tombstone_at(repo, &name, commit).is_some(),
        };
        updates.push(Update {
            local: name.clone(),
            oid: commit.to_string(),
            destination: destination(&name),
            tracking,
            force,
        });
    }
    Ok((updates, deferred))
}

/// Der Ziel-Ref beim identischen Mapping: derselbe Name am Remote.
///
/// Dass das geht, ist keine Selbstverständlichkeit — aber überprüft: GitLab
/// nimmt `refs/minds/context` an. Und weil der Ref nicht unter `refs/heads/`
/// liegt, kann eine Forge ihn weder als Default-Branch wählen noch in die
/// Branch-Liste des Nutzers stellen.
fn identity(name: &str) -> String {
    name.to_string()
}

/// Der Ziel-Ref im Child-Repo: `refs/minds/sessions/<hex>` → Branch
/// `minds/session/<hex>`, damit die Forge jede Session als eigenen,
/// anklickbaren Branch zeigt.
fn session_branch(name: &str) -> String {
    match name.strip_prefix("refs/minds/sessions/") {
        Some(hex) => format!("refs/heads/minds/session/{hex}"),
        None => name.to_string(),
    }
}

/// `refs/minds/remotes/<remote>/`.
fn tracking_prefix(remote: &str) -> String {
    format!("{TRACKING_PREFIX}{remote}/")
}

/// Der Tracking-Ref zu einem lokalen Minds-Ref.
fn tracking_ref(remote: &str, local: &str) -> String {
    let rest = local.strip_prefix(MINDS_REF_NAMESPACE).unwrap_or(local);
    format!("{}{rest}", tracking_prefix(remote))
}

// ---------------------------------------------------------------------------
// Die Ausführung — hier wird das Netz angefasst
// ---------------------------------------------------------------------------

impl Job {
    fn execute(&self, git_dir: &Path, verbose: bool) -> Fallible<()> {
        // Fortschritt auf **stdout**: Git zeigt die Ausgabe des Hooks, und ein
        // Push, der zehn Sekunden schweigt, sieht aus wie ein hängender Push.
        //
        // Warum nicht stderr, wo das früher stand: Seit der pre-push-Hook seine
        // stderr wegwirft (siehe `enable::PRE_PUSH_BODY`), verschwände diese
        // Zeile mit den Fehlern zusammen — und mit ihr der einzige Hinweis
        // darauf, dass minds beim Push überhaupt eine Verbindung aufbaut. Ein
        // Fortschritt ist keine Fehlermeldung; er gehört ohnehin hierher.
        let line = ProgressLine::start(&format!(
            "minds: {} Ref(s) → {} …",
            self.updates.len(),
            // Auch hier entschärft: Das Label kommt aus `.git/config` bzw.
            // `minds.childPath`, und ein `\e[2K\e[A` darin überschriebe die
            // Zeile, die `git push` gerade ausgegeben hat.
            crate::text::sanitize_path(display(&self.label))
        ));

        match self.push(&self.updates) {
            Ok(()) => {
                line.finish(" fertig");
                self.record(&self.updates, verbose);
                self.report_erasures(git_dir);
                Ok(())
            }
            Err(err) => {
                // Die Zeile schließt sich beim Verlassen selbst; hier endet sie
                // bewusst ohne Wort, weil der Aufrufer den Satz zu Ende bringt.
                drop(line);
                // Ein gescheiterter Push, der eine Löschung tragen sollte, darf
                // nicht stumm bleiben — auch nicht, wenn `reconcile` den Rest
                // des Jobs gleich rettet: Der `reconcile`-Umweg pusht nur den
                // Review-Ref erneut, die Löschung stünde sonst ohne ein Wort
                // aus, obwohl `forget` sie zugesagt hat. Gemeldet wird
                // „nicht bestätigt": Der nicht-atomare Push kann einzelne
                // Refspecs durchgebracht haben, ohne `record` wissen wir es
                // nicht — der nächste Sync forciert idempotent nach und meldet
                // dann den Erfolg.
                self.report_failed_erasures(git_dir);
                // Divergenz ist der eine Fehler, aus dem wir uns selbst
                // befreien können — aber nur dort, wo der Inhalt vereinigbar
                // ist. Für alles andere gilt: melden, nichts überschreiben.
                // Ob es eine war, hat `push` aus der `--porcelain`-Struktur
                // gelesen, nicht aus dem Wortlaut der Meldung (#71).
                if !err.diverged {
                    return Err(err.into());
                }
                self.reconcile(verbose)
            }
        }
    }

    /// Ein einziger `git push` für alle Refspecs.
    fn push(&self, updates: &[Update]) -> Result<(), PushError> {
        let mut args: Vec<String> = vec![
            "push".into(),
            // Der eigene Push darf den pre-push-Hook nicht erneut auslösen.
            "--no-verify".into(),
            "--porcelain".into(),
            self.remote.clone(),
        ];
        args.extend(updates.iter().map(|update| {
            // Die `+`-Refspec ist die eine, in `due` verifizierte Ausnahme
            // (Tombstone-Übertragung, #102) — ein `--force`-Flag, das alle
            // Refspecs beträfe, gibt es hier weiterhin nicht.
            let sign = if update.force { "+" } else { "" };
            format!("{sign}{}:{}", update.oid, update.destination)
        }));

        let mut command = Command::new("git");
        let output = quiet_trace(&mut command)
            .arg("-C")
            .arg(&self.dir)
            .args(&args)
            // Kein Terminal-Prompt aus dem Hook heraus: lieber schnell
            // scheitern als den Push des Nutzers an einer Passwortfrage
            // hängen lassen, die er nicht sieht.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(GUARD_ENV, "1")
            .output()
            .map_err(|err| PushError {
                diverged: false,
                // Ein io-Fehler trägt heute keine URL — redigiert wird er
                // trotzdem: Die Invariante „nichts verlässt `push`
                // unredigiert" soll lokal gelten, nicht per Fernargument.
                message: crate::text::without_url_credentials(&format!("git push: {err}")),
            })?;

        if output.status.success() {
            return Ok(());
        }
        // stdout und stderr bleiben getrennt — das ist der Kern von #71: Auf
        // stderr schreibt der Server frei (`remote: …`), auf stdout steht die
        // `--porcelain`-Struktur, die git selbst erzeugt. Nur Letztere trägt
        // die Reconcile-Entscheidung; vermischt man beide, genügt dem Server
        // eine `remote:`-Zeile mit dem passenden Wortlaut, um sie zu steuern.
        // Für die **Meldung** dagegen gehören beide hinein: stderr sagt, was
        // der Server meint, die `!`-Zeilen sagen, welcher Ref aus welchem
        // strukturellen Grund abgewiesen wurde.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let text = match (stderr.trim(), stdout.trim()) {
            (err, "") => err.to_string(),
            ("", out) => out.to_string(),
            (err, out) => format!("{err}\n{out}"),
        };
        // Zugangsdaten raus, **hier** und nicht bei der Ausgabe: Git schreibt
        // die Remote-URL in seine Fehlermeldung, und steht darin ein Token
        // (`https://glpat-…@gitlab.com/…`, die Username-Position redigiert Git
        // selbst nicht), trüge es jeder Weg weiter, der diesen Fehler anfasst —
        // stderr, `hook.log`, und mit der Datei ein Bug-Report. An der Senke zu
        // filtern hieße, es an jeder Senke einzeln zu tun und die nächste zu
        // vergessen.
        Err(PushError {
            diverged: is_divergence(&stdout),
            message: crate::text::without_url_credentials(&text),
        })
    }

    /// Meldet übertragene Löschungen — auf stdout **und** ins Log.
    ///
    /// Ein Force-Push ist die eine sicherheitssensible Ausnahme dieses Moduls;
    /// dass er stattfand, gehört sichtbar zum Push (stdout) und dauerhaft in die
    /// Akte (`hook.log`), mit den Ref-Namen — die tragen nur Hashes, keinen
    /// Inhalt. Gemeldet wird nach `record`, also nur, was wirklich ankam: Der
    /// `reconcile`-Umweg pusht ausschließlich den Review-Ref erneut und läuft
    /// deshalb bewusst an dieser Meldung vorbei.
    fn report_erasures(&self, git_dir: &Path) {
        let erased: Vec<&str> = self
            .updates
            .iter()
            .filter(|update| update.force)
            .map(|update| update.local.as_str())
            .collect();
        if erased.is_empty() {
            return;
        }
        ProgressLine::write(&format!(
            "minds: {} getilgte(r) Ref(s) per Force-Push übertragen — die Löschung ist jetzt auch auf der Forge\n",
            erased.len()
        ));
        hooklog::log_at(
            git_dir,
            Source::Sync,
            &format!(
                "DSGVO-Löschung übertragen ({}): Force-Push für {}",
                display(&self.label),
                erased.join(", ")
            ),
        );
    }

    /// Meldet Löschungen, deren Push scheiterte — bei **jedem** Lauf, bis sie
    /// durch sind.
    ///
    /// Der Gegenpart zu [`report_erasures`](Self::report_erasures): Weist die
    /// Forge die `+`-Refspec ab (Protected Branch auf `minds/session/*`,
    /// `receive.denyNonFastForwards`, ein Hook auf einem Spiegel), bleibt der
    /// Klartext dort die browsbare Ref-Spitze — und genau das darf nach der
    /// Erfolgsmeldung von `forget` nicht lautlos passieren. Weil die
    /// Tracking-Refs erst `record` schreibt, wiederholt sich diese Meldung bei
    /// jedem Sync, bis der Tombstone bestätigt ankommt.
    fn report_failed_erasures(&self, git_dir: &Path) {
        let pending: Vec<&str> = self
            .updates
            .iter()
            .filter(|update| update.force)
            .map(|update| update.local.as_str())
            .collect();
        if pending.is_empty() {
            return;
        }
        ProgressLine::write(&format!(
            "minds: DSGVO-Löschung NICHT bestätigt — {} getilgte(r) Ref(s) haben die Forge \
             nicht erreicht, der nächste Sync versucht es erneut\n",
            pending.len()
        ));
        hooklog::report_at(
            git_dir,
            Source::Sync,
            &format!(
                "Löschung nicht übertragen ({}): Force-Push scheiterte für {}",
                display(&self.label),
                pending.join(", ")
            ),
        );
    }

    /// Vermerkt den gepushten Stand in den Tracking-Refs.
    fn record(&self, updates: &[Update], verbose: bool) {
        for update in updates {
            let done = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["update-ref", &update.tracking, &update.oid])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !done {
                // Kein Drama: Ohne Vermerk wird derselbe Ref beim nächsten Mal
                // erneut angeboten — der Push ist idempotent.
                vln(
                    verbose,
                    &format!("  Tracking-Ref {} nicht gesetzt", update.tracking),
                );
            }
        }
    }

    /// Ein Ref wurde abgewiesen — jemand anderes hat ihn fortgeschrieben.
    ///
    /// Für den Review-/Thread-Log ist das lösbar: Sein Inhalt ist ein Log aus
    /// content-adressierten Einträgen, zwei Stände lassen sich **vereinigen**
    /// (kommutativ, konfliktfrei). Also: fremden Stand holen, vereinigen,
    /// erneut pushen — und der zweite Push ist wieder fast-forward.
    ///
    /// Für alles andere bleibt es beim Melden. Ein `--force` würde hier den
    /// Kontext einer anderen Maschine löschen, und das ist der eine Fehler, den
    /// ein Audit-Record nicht machen darf.
    fn reconcile(&self, verbose: bool) -> Fallible<()> {
        let Some(review) = self
            .updates
            .iter()
            .find(|update| update.local == DEFAULT_REVIEW_REF)
        else {
            return Err("Remote ist weiter als wir — bitte `git fetch` und erneut pushen".into());
        };

        let line = ProgressLine::start("minds: Review-Log divergiert, vereinige …");
        let incoming = format!("{}incoming", tracking_prefix(&self.remote));
        // `output()` statt `status()`: Sonst erbte das Kind unsere stderr, und
        // die wirft der pre-push-Hook seit #10 weg — der Grund des Fehlschlags
        // wäre dann nirgends. So steht er in der Meldung und damit im Log.
        let mut command = Command::new("git");
        let fetched = quiet_trace(&mut command)
            .arg("-C")
            .arg(&self.dir)
            .args([
                "fetch",
                "--quiet",
                &self.remote,
                &format!("+{}:{incoming}", review.destination),
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(GUARD_ENV, "1")
            .output()?;
        if !fetched.status.success() {
            return Err(crate::text::without_url_credentials(&format!(
                "fremder Review-Stand nicht abrufbar: {}",
                String::from_utf8_lossy(&fetched.stderr).trim()
            ))
            .into());
        }

        let repo = Repo::open(&self.dir)?;
        let merged = ReviewStore::new(repo).merge_from(&incoming)?;

        // Nach dem Merge zeigt der Ref woanders hin — der Plan von vorhin ist
        // veraltet, also neu nachsehen.
        let repo = Repo::open(&self.dir)?;
        let Some(commit) = repo.commit_at(DEFAULT_REVIEW_REF)? else {
            return Err("Review-Ref ist nach dem Merge verschwunden".into());
        };
        let retry = vec![Update {
            local: DEFAULT_REVIEW_REF.to_string(),
            oid: commit.to_string(),
            destination: review.destination.clone(),
            tracking: review.tracking.clone(),
            force: false,
        }];
        self.push(&retry)?;
        line.finish(&format!(" {merged} übernommen, fertig"));
        self.record(&retry, verbose);
        Ok(())
    }
}

/// Ein gescheiterter `git push`, beim Scheitern strukturell gedeutet.
///
/// Ob der Fehlschlag eine Divergenz war, steht als Flag daran — entschieden
/// aus der `--porcelain`-Struktur, nicht aus dem Wortlaut der Meldung. Der
/// Aufrufer muss den Text damit nie wieder deuten (#71).
#[derive(Debug)]
struct PushError {
    /// Mindestens ein Ref wurde als Divergenz abgewiesen — von git im lokalen
    /// Vergleich festgestellt, nicht vom Server behauptet.
    diverged: bool,
    /// Die Meldung für Mensch und `hook.log`, Zugangsdaten bereits entfernt.
    message: String,
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PushError {}

/// Ob der `--porcelain`-stdout von `git push` eine **Divergenz**-Abweisung
/// enthält — also eine, die `reconcile` durch Vereinigen auflösen kann.
///
/// Je Ref schreibt git eine Zeile `<flag>\t<from>:<to>\t<zusammenfassung>`;
/// eine Abweisung trägt das Flag `!`. Entscheidend ist die Zusammenfassung:
/// `[rejected] (<grund>)` stellt git im lokalen Vergleich selbst fest,
/// `[remote rejected] (…)` zitiert dagegen wörtlich den Server — dessen Grund
/// (die Ausgabe eines pre-receive-Hooks!) hier mitzulesen hieße, die
/// Reconcile-Entscheidung wieder an server-kontrollierten Text zu hängen.
/// Deshalb zählt nur Ersteres, und davon nur die Gründe, die tatsächlich
/// „jemand anderes hat den Ref fortgeschrieben" bedeuten — ein „hook declined"
/// oder ein Netz-/Auth-Fehler öffnet den Zweig nicht.
fn is_divergence(porcelain: &str) -> bool {
    porcelain.lines().any(|line| {
        // Ref-Namen können weder Tab noch `!` enthalten — die zwei `\t` sind
        // verlässliche Feldgrenzen.
        let Some(rest) = line.strip_prefix("!\t") else {
            return false;
        };
        let Some((_refspec, summary)) = rest.split_once('\t') else {
            return false;
        };
        summary
            .strip_prefix("[rejected] (")
            .and_then(|reason| reason.strip_suffix(')'))
            .is_some_and(|reason| {
                matches!(reason, "non-fast-forward" | "fetch first" | "stale info")
            })
    })
}

// ---------------------------------------------------------------------------
// Lock — damit eine Agent-Flotte nicht N Verbindungen gleichzeitig öffnet
// ---------------------------------------------------------------------------

/// Ein Lock, das sich beim Fallenlassen selbst entfernt.
///
/// Absichtlich primitiv: `create_new` ist auf jedem Dateisystem atomar, und
/// mehr als „einer nach dem anderen" wird hier nicht gebraucht. Ein Lock, das
/// älter ist als [`LOCK_STALE`], stammt von einem abgestürzten Lauf und wird
/// übergangen — ein Rekorder darf sich nicht selbst dauerhaft aussperren.
///
/// `pub(crate)`, weil `minds forget` dasselbe Lock nimmt (#102): Tilgte es
/// mitten in einem laufenden Sync, könnte dessen `record` den eben gelöschten
/// Tracking-Ref am Klartext-Commit neu erschaffen — der nächste Sync heilte
/// das zwar (lokal Tombstone, getrackt Klartext → Force), aber bis dahin
/// ankerte die Buchhaltung wieder Klartext und die Forge trüge ihn weiter.
pub(crate) struct Lock {
    path: PathBuf,
}

impl Lock {
    pub(crate) fn acquire(git_dir: &Path) -> std::io::Result<Option<Self>> {
        let dir = git_dir.join("minds");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("sync.lock");

        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Some(Self { path })),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !is_stale(&path) {
                        return Ok(None);
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .map(|age| age > LOCK_STALE)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Kleinkram
// ---------------------------------------------------------------------------

fn repo_root(repo: &Repo) -> PathBuf {
    let git_dir = repo.git_dir();
    if git_dir.file_name().is_some_and(|name| name == ".git") {
        git_dir.parent().unwrap_or(git_dir).to_path_buf()
    } else {
        git_dir.to_path_buf()
    }
}

/// Ob `dir` einen Remote dieses Namens kennt. Rein lokal (`git remote`), kein
/// `ls-remote`.
fn has_remote(dir: &Path, remote: &str) -> bool {
    git_output(dir, &["remote"])
        .unwrap_or_default()
        .lines()
        .any(|line| line.trim() == remote)
}

fn git_output(dir: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Eine URL im Label würde ein Token mitdrucken, das jemand in die Remote-URL
/// geschrieben hat. Nur Namen anzeigen.
fn display(label: &str) -> &str {
    if label.contains("://") || label.contains('@') {
        "Remote"
    } else {
        label
    }
}

/// Nimmt dem `git`-Kindprozess die Trace-Schalter aus der Umgebung.
///
/// `GIT_TRACE` & Co. lassen Git seinen gesamten Verkehr auf stderr
/// protokollieren — samt `Authorization: Basic …`, und das ist keine URL, die
/// [`crate::text::without_url_credentials`] fassen könnte. Seit dieses stderr
/// eingefangen und in eine Datei geschrieben wird (#10), genügte ein gesetztes
/// `GIT_TRACE` in der Umgebung des Entwicklers, um ein Token dauerhaft auf die
/// Platte zu legen.
///
/// **`env_remove`, nicht `env(…, "0")`.** Das ist der Unterschied, an dem der
/// erste Versuch scheiterte: `GIT_CURL_VERBOSE` prüft Git auf *Existenz*, nicht
/// auf den Wert — ein `=0` schaltet den Dump also **ein** statt aus (verifiziert
/// mit Git 2.51). Nur `GIT_TRACE_REDACT` wird gesetzt: Es ist wertbasiert, `1`
/// ist der Default, und so schlägt ein `0` von außen nicht durch.
fn quiet_trace(cmd: &mut Command) -> &mut Command {
    for key in [
        "GIT_TRACE",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE2_PERF",
        "GIT_TRACE_CURL",
        "GIT_TRACE_PACKET",
        "GIT_CURL_VERBOSE",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("GIT_TRACE_REDACT", "1")
}

/// Eine angefangene Fortschrittszeile, die sich beim Verlassen selbst schließt.
///
/// Zwei Dinge stecken darin, die einzeln leicht danebengehen:
///
/// **Der `flush`.** stdout ist am Terminal zeilengepuffert; ein `print!` ohne
/// Umbruch stünde erst da, wenn der Push längst durch ist — also genau dann
/// nicht, wenn er gebraucht wird. stderr brauchte das nicht, weil es
/// ungepuffert ist; beim Umzug nach stdout kommt es dazu.
///
/// **Das `Drop`.** Zwischen Anfang und Ende der Zeile liegen mehrere `?`. Bliebe
/// die Zeile bei einem davon offen, klebte die nächste Ausgabe daran — im
/// pre-push-Hook ist das die von `git push` selbst, und der Nutzer läse einen
/// Satz, den nie jemand beendet hat. Ein Zeilenende gehört nicht an jeden
/// Rückweg einzeln, sondern an genau eine Stelle.
struct ProgressLine {
    open: bool,
}

impl ProgressLine {
    fn start(line: &str) -> Self {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "{line}");
        let _ = out.flush();
        Self { open: true }
    }

    /// Beendet die Zeile mit ihrem Schlusswort.
    fn finish(mut self, tail: &str) {
        // Das Flag **vor** der Ausgabe: Bricht das Schreiben weg (`git push |
        // head` schließt stdout), liefe sonst `Drop` im Unwind, sähe `open`
        // noch gesetzt und panickte erneut — ein zweiter Panic während des
        // Aufräumens ist ein `abort`, und dann käme `hooklog::guarded` nie zum
        // Zug.
        self.open = false;
        Self::write(&format!("{tail}\n"));
    }

    /// Schreibt, ohne bei einem Schreibfehler zu panicken — anders als
    /// `println!`, das genau das tut.
    fn write(text: &str) {
        use std::io::Write;
        let _ = std::io::stdout().write_all(text.as_bytes());
    }
}

impl Drop for ProgressLine {
    fn drop(&mut self) {
        if self.open {
            Self::write("\n");
        }
    }
}

fn vln(verbose: bool, line: &str) {
    if verbose {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tracking_ref_mirrors_the_local_name() {
        assert_eq!(
            tracking_ref("origin", "refs/minds/context"),
            "refs/minds/remotes/origin/context"
        );
        assert_eq!(
            tracking_ref("origin", "refs/minds/sessions/abc"),
            "refs/minds/remotes/origin/sessions/abc"
        );
    }

    #[test]
    fn tracking_refs_are_never_pushed() {
        // Sonst spiegelte sich die eigene Buchhaltung ins Remote — und beim
        // nächsten Lauf hielte sie sich selbst für zu pushenden Kontext.
        assert!(tracking_ref("origin", "refs/minds/context").starts_with(TRACKING_PREFIX));
    }

    #[test]
    fn the_identity_mapping_keeps_the_name() {
        assert_eq!(identity("refs/minds/context"), "refs/minds/context");
    }

    #[test]
    fn a_session_ref_becomes_a_branch_in_the_child_repo() {
        assert_eq!(
            session_branch("refs/minds/sessions/ab12"),
            "refs/heads/minds/session/ab12"
        );
        // Fremdes bleibt unangetastet.
        assert_eq!(session_branch("refs/minds/context"), "refs/minds/context");
    }

    #[test]
    fn a_divergence_is_read_from_the_porcelain_structure() {
        // Nur was git selbst im lokalen Vergleich feststellt, zählt.
        assert!(is_divergence(
            "To gitlab.com:x/y.git\n!\trefs/minds/reviews:refs/minds/reviews\t[rejected] (non-fast-forward)\nDone"
        ));
        assert!(is_divergence(
            "!\tabc123:refs/minds/reviews\t[rejected] (fetch first)"
        ));
        assert!(is_divergence(
            "!\tabc123:refs/minds/reviews\t[rejected] (stale info)"
        ));
        // Eine kaputte `!`-Zeile daneben stört die echte nicht — die
        // Feldgrenzen-Annahme (`split_once('\t')`) ist fehlertolerant.
        assert!(is_divergence(
            "!\tkein zweites Tabfeld\n!\tabc123:refs/minds/reviews\t[rejected] (fetch first)"
        ));
    }

    #[test]
    fn the_reason_whitelist_stays_narrow() {
        // Auch git-lokal festgestellt, aber keine vereinigbare Divergenz —
        // reconcile könnte hier nichts retten.
        assert!(!is_divergence(
            "!\tabc123:refs/minds/reviews\t[rejected] (already exists)"
        ));
    }

    #[test]
    fn server_written_text_is_never_a_divergence() {
        // Die Regression aus #71: `remote:`-Zeilen und der Grund hinter
        // `[remote rejected]` kommen wörtlich vom Server — beides darf den
        // Reconcile-Zweig nicht öffnen, auch nicht mit dem „richtigen"
        // Wortlaut.
        assert!(!is_divergence(
            "remote: rejected — Updates were rejected (non-fast-forward)"
        ));
        assert!(!is_divergence(
            "!\trefs/minds/reviews:refs/minds/reviews\t[remote rejected] (hook declined)"
        ));
        assert!(!is_divergence(
            "!\trefs/minds/reviews:refs/minds/reviews\t[remote rejected] (non-fast-forward)"
        ));
        // Auch neben einer Erfolgszeile für einen anderen Ref bleibt das
        // Server-Zitat wirkungslos.
        assert!(!is_divergence(
            "To gitlab.com:x/y.git\n*\tabc123:refs/minds/context\t[new reference]\n!\trefs/minds/reviews:refs/minds/reviews\t[remote rejected] (non-fast-forward)\nDone"
        ));
    }

    #[test]
    fn a_network_error_is_not_a_divergence() {
        assert!(!is_divergence(
            "ssh: connect to host gitlab.com port 22: timeout"
        ));
        assert!(!is_divergence("Permission denied (publickey)."));
    }

    /// Git mit fixierter Identität in `path` ausführen — für die Tests, die
    /// echte Repositories brauchen.
    fn run_git(path: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Ein Job, der genau den Review-Ref auf `oid` schieben will.
    fn review_push_job(work: &Path, remote: &Path, oid: &str) -> Job {
        Job {
            dir: work.to_path_buf(),
            remote: remote.display().to_string(),
            label: "origin".into(),
            updates: vec![Update {
                local: DEFAULT_REVIEW_REF.to_string(),
                oid: oid.to_string(),
                destination: DEFAULT_REVIEW_REF.to_string(),
                tracking: tracking_ref("origin", DEFAULT_REVIEW_REF),
                force: false,
            }],
        }
    }

    #[test]
    fn a_real_non_fast_forward_still_opens_the_reconcile_path() {
        // Akzeptanzkriterium aus #71: Eine echte Divergenz öffnet den
        // Reconcile-Zweig weiterhin — jetzt über die Struktur statt über den
        // Wortlaut.
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        run_git(dir.path(), &["init", "--bare", "--quiet", "remote.git"]);
        std::fs::create_dir(&work).unwrap();
        run_git(&work, &["init", "--quiet", "-b", "main"]);
        run_git(&work, &["commit", "--allow-empty", "--quiet", "-m", "a"]);
        let a = run_git(&work, &["rev-parse", "HEAD"]);
        let tree = run_git(&work, &["rev-parse", "HEAD^{tree}"]);
        // Die Forge steht auf `a` …
        run_git(
            &work,
            &[
                "push",
                "--quiet",
                remote.to_str().unwrap(),
                &format!("{a}:{DEFAULT_REVIEW_REF}"),
            ],
        );
        // … lokal soll ein elternloser Stand hin — kein Nachfahre von `a`.
        let orphan = run_git(&work, &["commit-tree", &tree, "-m", "b"]);

        let job = review_push_job(&work, &remote, &orphan);
        let err = job.push(&job.updates).unwrap_err();
        assert!(
            err.diverged,
            "non-fast-forward muss als Divergenz gelten: {}",
            err.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_shouting_rejected_does_not_open_the_reconcile_path() {
        use std::os::unix::fs::PermissionsExt;

        // Der Regressionstest aus #71: Der Server schreibt „rejected" in
        // seine Meldung. Strukturell ist das ein `[remote rejected]` — und
        // damit keine Divergenz, egal was der Text behauptet.
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        run_git(dir.path(), &["init", "--bare", "--quiet", "remote.git"]);
        let hook = remote.join("hooks").join("pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'rejected: non-fast-forward, fetch first' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(&work).unwrap();
        run_git(&work, &["init", "--quiet", "-b", "main"]);
        run_git(&work, &["commit", "--allow-empty", "--quiet", "-m", "a"]);
        let a = run_git(&work, &["rev-parse", "HEAD"]);

        // Ohne den Hook ginge dieser Push glatt durch — neuer Ref, kein
        // Konflikt. Abgewiesen wird er allein vom Server.
        let job = review_push_job(&work, &remote, &a);
        let err = job.push(&job.updates).unwrap_err();
        assert!(
            !err.diverged,
            "server-kontrollierter Text darf keine Divergenz melden: {}",
            err.message
        );
    }

    #[test]
    fn a_url_label_is_not_printed() {
        assert_eq!(display("origin"), "origin");
        assert_eq!(display("https://token@example.org/x.git"), "Remote");
        assert_eq!(display("git@example.org:x.git"), "Remote");
    }

    #[test]
    fn a_non_fast_forward_ref_is_deferred_while_the_others_still_push() {
        // Das Sicherheitsnetz aus #14: Ein non-fast-forward Ref (hier per Hand als
        // divergierender Orphan gebaut) darf `minds sync` nicht reißen — er wird
        // zurückgestellt, die übrigen Refs (neu oder sauberes Fast-Forward) bleiben
        // pushbar. Der Orphan trägt bewusst **keinen** Tombstone: Nur ein
        // nachgewiesener Tombstone dürfte per Force mit (#102, nächster Test);
        // alles andere bleibt auch dann liegen, wenn es elternlos ist.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00")
                .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };

        git(&["init", "--quiet", "-b", "main"]);
        git(&["commit", "--allow-empty", "--quiet", "-m", "a"]);
        let a = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        // Ein elternloser Commit (wie ein Tombstone): hat `a` nicht als Vorfahr.
        let orphan = git(&["commit-tree", &tree, "-m", "tombstone"]);
        // Ein Fast-Forward über `a`.
        let forward = git(&["commit-tree", &tree, "-p", &a, "-m", "forward"]);

        // gone: getrackt auf `a`, lokal auf den elternlosen `orphan` → non-ff.
        git(&["update-ref", "refs/minds/store/gone", &orphan]);
        git(&["update-ref", "refs/minds/remotes/origin/store/gone", &a]);
        // moved: getrackt auf `a`, lokal auf `forward` → Fast-Forward.
        git(&["update-ref", "refs/minds/store/moved", &forward]);
        git(&["update-ref", "refs/minds/remotes/origin/store/moved", &a]);
        // fresh: nie getrackt → regulär pushen.
        git(&["update-ref", "refs/minds/store/fresh", &a]);

        let repo = Repo::open(path).unwrap();
        let (updates, deferred) = due(&repo, "origin", MINDS_REF_NAMESPACE, identity).unwrap();

        // Der non-ff-Ref ist zurückgestellt, nicht in den Updates.
        assert_eq!(deferred, vec!["refs/minds/store/gone".to_string()]);
        let pushed: std::collections::BTreeSet<&str> =
            updates.iter().map(|u| u.local.as_str()).collect();
        assert_eq!(
            pushed,
            ["refs/minds/store/fresh", "refs/minds/store/moved"]
                .into_iter()
                .collect()
        );
        // Und keiner davon mit Force — hier ist nirgends ein Tombstone im Spiel.
        assert!(updates.iter().all(|u| !u.force), "{updates:?}");
    }

    #[test]
    fn only_a_verified_tombstone_travels_with_force() {
        // Die eine Ausnahme vom „nie --force" (#102): Ein session-exklusiver Ref,
        // dessen Spitze nachweislich ein Tombstone ist, geht mit `+`-Refspec —
        // egal ob sein Tracking-Ref noch am Klartext hängt (Umsetzen schlug einst
        // fehl) oder schon gelöscht ist (der reguläre `forget`-Pfad). Klartext
        // dagegen bekommt das Flag nie, auch nicht als frischer Ref.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = |args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00")
                .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };

        git(&["init", "--quiet", "-b", "main"]);
        git(&["commit", "--allow-empty", "--quiet", "-m", "a"]);
        let a = git(&["rev-parse", "HEAD"]);

        // Ein echter Tombstone-Commit und ein Klartext-Commit, beide elternlos.
        let repo = Repo::open(path).unwrap();
        let tomb_blob = repo
            .write_blob(&minds_store::tombstone::bytes("DSGVO"))
            .unwrap();
        let tomb_tree = repo
            .write_tree(None, [("session.json", tomb_blob)])
            .unwrap();
        let tomb = git(&["commit-tree", &tomb_tree.to_string(), "-m", "tombstone"]);
        let plain_blob = repo.write_blob(br#"{"agent":{"name":"x"}}"#).unwrap();
        let plain_tree = repo
            .write_tree(None, [("session.json", plain_blob)])
            .unwrap();
        let plain = git(&["commit-tree", &plain_tree.to_string(), "-m", "klartext"]);

        // erased: Tracking hängt noch am Klartext `a`, lokal der Tombstone → Force.
        git(&["update-ref", "refs/minds/store/erased", &tomb]);
        git(&["update-ref", "refs/minds/remotes/origin/store/erased", &a]);
        // branch: ungetrackt (forget löschte die Buchhaltung), Tombstone → Force.
        git(&["update-ref", "refs/minds/sessions/deadbeef", &tomb]);
        // fresh: ungetrackt, Klartext → regulär, ohne Force.
        git(&["update-ref", "refs/minds/store/fresh", &plain]);
        // childed: Tombstone-Inhalt, aber **mit** Eltern — die Historie reiste
        // beim Force mit. Divergiert (getrackt auf `a`) → zurückstellen.
        let childed = git(&[
            "commit-tree",
            &tomb_tree.to_string(),
            "-p",
            &plain,
            "-m",
            "tombstone mit eltern",
        ]);
        git(&["update-ref", "refs/minds/store/childed", &childed]);
        git(&["update-ref", "refs/minds/remotes/origin/store/childed", &a]);

        let repo = Repo::open(path).unwrap();
        let (updates, deferred) = due(&repo, "origin", MINDS_REF_NAMESPACE, identity).unwrap();

        assert_eq!(deferred, vec!["refs/minds/store/childed".to_string()]);
        let force: BTreeMap<&str, bool> = updates
            .iter()
            .map(|u| (u.local.as_str(), u.force))
            .collect();
        assert_eq!(force.get("refs/minds/store/erased"), Some(&true));
        assert_eq!(force.get("refs/minds/sessions/deadbeef"), Some(&true));
        assert_eq!(force.get("refs/minds/store/fresh"), Some(&false));
    }
}
