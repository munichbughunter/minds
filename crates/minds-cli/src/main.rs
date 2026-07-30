//! Das `minds`-Binary.
//!
//! Die Kommandos: [`enable`] (Setup — Hooks + Store-Config), [`hook`] (heißer
//! Pfad), `checkpoint`/`show`/`why`/`fsck` (kalter Pfad, M6).
//!
//! # Warum hier kein clap steht
//!
//! Ein Argument-Parser ist eine Aussage über die Kommandostruktur. Die
//! Kommandostruktur von Minds ist noch nicht entworfen; sie jetzt an einem
//! einzigen Unterkommando auszurichten hieße, sie in M6 wieder umzubauen. Der
//! Parser unten ist deshalb absichtlich zu dumm, um zu bleiben: kein
//! Subkommando-Baum, keine Kurzformen, keine Gruppierung.
//!
//! Der zweite Grund ist der heiße Pfad. `minds hook` startet bei jedem
//! Tool-Call des Agenten neu — der Prozess soll stdin lesen, eine Datei
//! schreiben und enden.
//!
//! # Die Ausnahme bei den Rückgabewerten
//!
//! Alle künftigen Unterkommandos melden Fehler über den Rückgabewert, wie es
//! sich gehört. `hook` nicht: Es endet **immer** mit 0, auch bei falschen
//! Argumenten. Der Grund steht in [`hook`] — bei Claude Code bedeutet Exit 2
//! „blockiere diese Aktion", und ein Rekorder, der wegen eines fehlenden
//! `--agent` die Arbeit des Nutzers stoppt, hat seinen Zweck verfehlt.
//!
//! Der Preis ist, dass eine kaputte Hook-Konfiguration still bleibt. Deshalb
//! landet sie in `<git-dir>/minds/hook.log`, und deshalb wird `minds fsck` in
//! M6 danach sehen.

mod agent_help;
mod audit;
mod blame;
mod brief_cmd;
mod checkpoint;
mod config;
mod context;
mod distill;
mod enable;
mod forget_cmd;
mod fsck;
mod gitlab_cmd;
mod hook;
mod import_cmd;
mod metrics;
mod prepare_commit_msg;
mod recall;
mod recap;
mod render;
mod render_cmd;
mod review_cmd;
mod search;
mod show;
mod sign_cmd;
mod signing;
mod stack;
mod sync;
mod verify_cmd;
mod why;

use std::process::ExitCode;

use minds_core::Decision;
use minds_store::StoreConfig;

const USAGE: &str = "\
minds — dauerhafter Kontext für Agent-Sessions, in Git.

Verwendung:
  minds enable [--agent <name>] [--child-repo <pfad>] [--child-remote <url>] [-v] [--ref <name>] [--recall]
        Richtet das Repo Minds-fähig ein: registriert die Hooks im Agenten
        und im Repo und schreibt die Store-Config nach .git/config.
        Läuft still; -v/--verbose zeigt, was im Einzelnen passiert.
        Ohne --agent: alle bekannten Agents. Idempotent, fremdschonend.
        Agents: claude-code, codex, cursor, gemini, opencode, all.
        --child-repo legt den Kontext in ein separates Repo statt in-repo;
        es wird angelegt (bare) oder von --child-remote geklont.
        --recall (Claude Code): SessionStart-Hook, der den Kontext-Brief der
        vorigen Sessions der neuen Session voranstellt. Opt-in (kostet Tokens).

  minds hook --agent <name> [--event <name>]
        Nimmt ein Agent-Hook-Event auf stdin entgegen und legt es im
        lokalen Journal ab. Endet immer mit 0.

  minds checkpoint [--commit <id>]
        Deutet das Journal, redigiert (Policy optional aus .minds/redact.json:
        allow/deny_secrets/deny_pii/secret_keys …), legt die Sessions im Store ab
        und hängt den Minds-Session-Id-Trailer an HEAD. Ruft der post-commit-Hook.

  minds show [<commit>] [--full]
        Zeigt Intent und Attribution der Session(s) hinter einem Commit
        (Default HEAD). Kompakt; --full zeigt Prompt, alle Dateien und Kanten.

  minds why <datei>:<zeile> [--full]
        Zeigt die Session hinter einer einzelnen Zeile (blame → Trailer).

  minds blame <datei>
        Überblick, welche Session hinter welchen Zeilen einer Datei steckt,
        nach Session aggregiert, mit Kontext-Abdeckung in Prozent.

  minds recall <ziel>
        Verdichtet die Session(s) hinter einer Datei, einer Zeile
        (<datei>:<zeile>) oder einem Commit zu einem knappen Kontext-Brief.
        Deterministisch, 0 Tokens — die Agent-Schwester von why.

  minds distill [--path <verzeichnis>] [--out <datei>]
        Verdichtet die Historie des Repos (oder eines Pfades) zu einem
        AGENTS.md-Entwurf: Befehle, Hot-Files, Sackgassen, Korrekturen.
        Ohne --out nach stdout.

  minds brief [<datei>...]
        Größenbegrenzter Kontext-Block für den Start einer Agent-Session.
        Ohne Pfade das ganze Repo.

  minds recap [--limit <n>] [--all]
        Die jüngsten Sessions auf einen Blick (Default 10; --all zeigt alle).

  minds search <query>
        Durchsucht Absicht, Verlauf und Dateien der erfassten Sessions.

  minds agent-help
        Maschinenlesbare Kommando-Karte (JSON) — für Agents, nicht Menschen.

  minds metrics [--format prometheus|openmetrics|json]
        Kennzahlen aus dem Store (Throughput, Iteration, Continuity, Streak,
        Redaction, Kontext-Abdeckung). Default Prometheus, für Grafana.

  minds fsck [--require-review]
        Prüft, ob jeder Trailer auflösbar ist, und meldet Journal-Lücken.
        Rückgabewert ≠ 0 bei verwaisten Trailern. --require-review: verlangt für
        jeden agent-authored Change ein Approve (Policy-Gate für die CI).

  minds forget <session> [--reason <text>]
        DSGVO-Löschung: ersetzt die Nutzlast einer Session durch einen Tombstone.
        Die Referenz bleibt auflösbar, der Inhalt verschwindet aus dem Store.

  minds sign <session> [--key <pfad>]
        Signiert die Attribution einer Session (ssh-sig) nach stdout.
        Schlüssel aus --key oder git config user.signingkey.

  minds verify <session> --sig <datei> [--signers <datei>] [--identity <id>]
        Prüft eine signierte Attribution. Rückgabewert ist nicht 0 bei ungültig.

  minds review <subject> --approve|--reject|--needs-work [--summary <text>]
                          [--sign] [--key <pfad>]
        Legt ein Review-Verdict als Git-Objekt an (refs/minds/reviews).
        <subject> ist eine Change-Id (I…) oder Session-Id (b3…).
        --sign unterschreibt es (ssh-sig) — aus einer Behauptung wird ein
        Nachweis. Schlüssel aus --key oder git config user.signingkey.

  minds reviews <subject> [--signers <datei>] [--identity <id>]
        Zeigt Verdicts und Thread zu einer Change-Id oder Session-Id.
        Mit --signers werden die Signaturen geprüft statt nur gemeldet.

  minds comment <subject> [--on <datei:zeile|turn:<n>>] \"<text>\"
        Hängt eine Anmerkung an den Review-Thread. Der Thread ist ein
        append-only Log content-adressierter Einträge — zwei Reviewer offline
        ergeben keinen Konflikt, sondern eine Vereinigung.

  minds sync [--remote <name>] [-v]
        Schickt Kontext und Reviews an das Remote — alle fälligen Refs in
        einer Verbindung, nie mit --force. Ruft der pre-push-Hook; ohne neue
        Refs kostet der Aufruf keine Verbindung.

  minds stack [--base <ref>]
        Zeigt die abhängigen Changes ab der Basis und ihren jeweiligen
        Review-Stand. Weil das Verdict an der Change-Id hängt, überlebt es
        Rebase und Force-Push.

  minds gitlab mirror <subject> --mr <nr> [--url <basis>] [--project <id>]
                      [--token-env <var>] [--approve]
        Spiegelt die Verdicts eines Changes als MR-Note nach GitLab —
        einweg und idempotent. Quelle bleibt das Repo. Token nur aus der
        Umgebung (Default MINDS_GITLAB_TOKEN), nie als Argument.

  minds gitlab webhook [--write]
        Liest eine GitLab-Webhook-Nutzlast von stdin und deutet einen
        MR-Kommentar (/minds approve|reject|needs-work) als Verdict.
        Ohne --write wird nur gezeigt, was entstünde. Opt-in, kein Dienst.

  minds audit --export [--out <datei>] [--base <ref>]
        Bündelt die Provenienz-Kette (Change → Session → Attribution →
        Verdict) als portable JSON-Datei. Enthält die kanonischen Payloads
        und Signaturen — prüfbar ohne dieses Werkzeug. Ohne --out nach stdout.

  minds render [--out <verzeichnis>]
        Baut eine statische HTML-Seite über den Kontext (Default ./site):
        Zeile anklicken → Prompt dahinter sehen. Zustandslos.

  minds --version
  minds --help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("enable") => {
            // Verstecktes internes Flag: `minds enable` startet den Backfill als
            // losgelösten Hintergrundprozess, der sich selbst hiermit aufruft.
            // Kein öffentliches `minds import` — der Nutzer sieht davon nichts
            // (steht bewusst nicht in USAGE).
            if args.iter().any(|a| a == enable::BACKGROUND_IMPORT_FLAG) {
                import_cmd::run()
            } else {
                let agent = flag(&args, "--agent");
                let store = store_config_from(&args);
                let child_remote = flag(&args, "--child-remote");
                let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
                let recall = args.iter().any(|a| a == "--recall");
                enable::run(
                    agent.as_deref(),
                    &store,
                    child_remote.as_deref(),
                    verbose,
                    recall,
                )
            }
        }

        Some("checkpoint") => {
            let commit = flag(&args, "--commit");
            checkpoint::run(commit.as_deref())
        }

        Some("show") => {
            let full = args.iter().any(|a| a == "--full");
            show::run(positional(&args), full)
        }

        Some("why") => {
            let full = args.iter().any(|a| a == "--full");
            why::run(positional(&args), full)
        }

        Some("blame") => blame::run(positional(&args)),

        Some("recall") => recall::run(positional(&args)),

        Some("distill") => {
            let path = flag(&args, "--path");
            let out = flag(&args, "--out");
            distill::run(path.as_deref(), out.as_deref())
        }

        Some("brief") => {
            let paths: Vec<String> = args
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-'))
                .cloned()
                .collect();
            let hook = args.iter().any(|a| a == "--hook");
            brief_cmd::run(&paths, hook)
        }

        Some("recap") => {
            let limit = flag(&args, "--limit");
            let all = args.iter().any(|a| a == "--all");
            recap::run(limit.as_deref(), all)
        }

        Some("search") => search::run(positional(&args)),

        Some("agent-help") => agent_help::run(),

        Some("metrics") => {
            let format = flag(&args, "--format");
            metrics::run(format.as_deref())
        }

        Some("review") => {
            let decision = if args.iter().any(|a| a == "--approve") {
                Some(Decision::Approve)
            } else if args.iter().any(|a| a == "--reject") {
                Some(Decision::Reject)
            } else if args.iter().any(|a| a == "--needs-work") {
                Some(Decision::NeedsWork)
            } else {
                None
            };
            let summary = flag(&args, "--summary");
            let sign = args.iter().any(|a| a == "--sign");
            let key = flag(&args, "--key");
            review_cmd::run_review(
                positional(&args),
                decision,
                summary.as_deref(),
                sign,
                key.as_deref(),
            )
        }

        Some("audit") => {
            let export = args.iter().any(|a| a == "--export");
            let out = flag(&args, "--out");
            let base = flag(&args, "--base");
            audit::run(export, out.as_deref(), base.as_deref())
        }

        Some("gitlab") => {
            let sub = args
                .get(1)
                .map(String::as_str)
                .filter(|a| !a.starts_with('-'));
            let subject = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with('-'))
                .map(String::as_str);
            gitlab_cmd::run(
                sub,
                gitlab_cmd::Options {
                    subject,
                    merge_request: flag(&args, "--mr").as_deref(),
                    url: flag(&args, "--url").as_deref(),
                    project: flag(&args, "--project").as_deref(),
                    token_env: flag(&args, "--token-env").as_deref(),
                    approve: args.iter().any(|a| a == "--approve"),
                    write: args.iter().any(|a| a == "--write"),
                },
            )
        }

        Some("stack") => {
            let base = flag(&args, "--base");
            stack::run(base.as_deref())
        }

        Some("comment") => {
            let on = flag(&args, "--on");
            // Der Text ist das erste positionale Argument nach dem Subjekt.
            let body = args
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-'))
                .filter(|a| Some(a.as_str()) != on.as_deref())
                .nth(1)
                .cloned();
            review_cmd::run_comment(positional(&args), on.as_deref(), body.as_deref())
        }

        Some("reviews") => {
            let signers = flag(&args, "--signers");
            let identity = flag(&args, "--identity");
            review_cmd::run_reviews(positional(&args), signers.as_deref(), identity.as_deref())
        }

        Some("fsck") => {
            let require_review = args.iter().any(|a| a == "--require-review");
            fsck::run(require_review)
        }

        Some("forget") => {
            let reason = flag(&args, "--reason");
            forget_cmd::run(positional(&args), reason.as_deref())
        }

        Some("sign") => {
            let key = flag(&args, "--key");
            sign_cmd::run(positional(&args), key.as_deref())
        }

        Some("verify") => {
            let sig = flag(&args, "--sig");
            let signers = flag(&args, "--signers");
            let identity = flag(&args, "--identity");
            verify_cmd::run(
                positional(&args),
                sig.as_deref(),
                signers.as_deref(),
                identity.as_deref(),
            )
        }

        Some("render") => {
            let out = flag(&args, "--out");
            render_cmd::run(out.as_deref())
        }

        Some("sync") => {
            let remote = flag(&args, "--remote");
            let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
            sync::run(remote.as_deref(), verbose)
        }

        Some("hook") => {
            let agent = flag(&args, "--agent");
            let event = flag(&args, "--event");
            hook::run(agent.as_deref(), event.as_deref())
        }

        // Interner Git-Hook: sorgt für eine stabile Change-Id (nicht in USAGE).
        Some("prepare-commit-msg") => prepare_commit_msg::run(positional(&args)),

        Some("--version" | "-V") => {
            println!("minds {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }

        Some("--help" | "-h") | None => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }

        Some(other) => {
            eprintln!("unbekanntes Unterkommando: {other}\n");
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Das erste positionale Argument nach dem Unterkommando (das erste, das nicht
/// mit `-` beginnt). So darf `--full` vor *oder* nach dem Commit stehen.
fn positional(args: &[String]) -> Option<&str> {
    args.iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
}

/// Liest `--name wert` aus den Argumenten.
///
/// Bewusst ohne `--name=wert`, ohne Kurzformen und ohne Fehlermeldung bei
/// Unbekanntem: ein absichtlich dummer Parser, solange die Kommandostruktur
/// jung ist.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Baut die [`StoreConfig`] aus `--child-repo`/`--ref`. Ohne Flags: In-Repo mit
/// Default-Ref — die Einstellung, für die niemand etwas tun muss.
fn store_config_from(args: &[String]) -> StoreConfig {
    let base = match flag(args, "--child-repo") {
        Some(path) => StoreConfig::child_repo(path),
        None => StoreConfig::in_repo(),
    };
    match flag(args, "--ref") {
        Some(reference) => base.with_ref(reference),
        None => base,
    }
}
