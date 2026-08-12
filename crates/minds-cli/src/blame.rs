//! `minds blame <datei>` — welcher Agent, welche Session steckt hinter welchen
//! Zeilen einer Datei.
//!
//! Die git-vertraute Frage („wer war das?"), aber bis zur Session
//! weitergedacht: pro Zeile das Blame → Commit → Session, dann nach Session
//! aggregiert. `why` beantwortet *eine* Zeile im Detail, `blame` gibt den
//! Überblick über die *ganze* Datei — inklusive der ehrlichen Zahl, wie viel
//! davon überhaupt erfassten Kontext hat.
//!
//! Geblamed wird **HEAD**, nicht der Arbeitsstand (siehe `minds-git::blame`):
//! eine uncommittete Änderung darf die Zeilennummern nicht verschieben.

use std::collections::BTreeMap;
use std::process::ExitCode;

use minds_core::{Session, SessionId};
use minds_git::{BlameProvider, CommitId};

use crate::context::{Context, Skipped};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds blame` aus. `target` ist ein repo-relativer Dateipfad.
pub fn run(target: Option<&str>) -> ExitCode {
    let Some(path) = target else {
        eprintln!("minds blame: erwartet <datei>");
        return ExitCode::FAILURE;
    };
    match blame(path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds blame: {err}");
            ExitCode::FAILURE
        }
    }
}

fn blame(path: &str) -> Fallible<()> {
    let ctx = Context::open()?;
    let Some(head) = ctx.repo.head()?.commit() else {
        return Err("HEAD hat noch keinen Commit".into());
    };

    let lines = ctx.repo.blame().blame_file(head, path)?;
    if lines.is_empty() {
        return Err(format!("{path} ist im Blame nicht auflösbar (nicht im Commit?)").into());
    }
    let total = lines.len();

    // Jede Zeile ihrer Session zuordnen (über den Commit). Commit→Sessions wird
    // gecacht, damit eine 1000-Zeilen-Datei nicht 1000 Store-Lookups auslöst.
    let mut lines_per_session: BTreeMap<SessionId, u32> = BTreeMap::new();
    let mut session_of: BTreeMap<SessionId, Session> = BTreeMap::new();
    let mut commit_cache: BTreeMap<CommitId, Vec<SessionId>> = BTreeMap::new();
    let mut without = 0u32;
    let mut skipped = Skipped::default();

    for entry in &lines {
        let ids = match commit_cache.get(&entry.commit) {
            Some(ids) => ids.clone(),
            None => {
                let (linked, s) = ctx.linked_sessions(entry.commit)?;
                skipped.merge(s);
                let ids: Vec<SessionId> = linked
                    .into_iter()
                    .filter(|(_, session)| !session.intent.request.trim().is_empty())
                    .map(|(id, session)| {
                        session_of.entry(id).or_insert(session);
                        id
                    })
                    .collect();
                commit_cache.insert(entry.commit, ids.clone());
                ids
            }
        };
        // Mehrere Sessions am selben Commit: die Zeile der ersten zuschreiben,
        // damit die Summe der Zeilen die Dateigröße nicht übersteigt.
        match ids.first() {
            Some(id) => *lines_per_session.entry(*id).or_default() += 1,
            None => without += 1,
        }
    }

    if let Some(note) = skipped.note() {
        eprintln!("minds blame: {note}");
    }

    let with_context = total as u32 - without;
    let pct = with_context as usize * 100 / total;
    println!("{path} — {total} Zeilen, {with_context} mit erfasstem Kontext ({pct}%)\n");

    let mut ranked: Vec<(SessionId, u32)> = lines_per_session.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    for (id, count) in &ranked {
        let session = &session_of[id];
        let headline = minds_reader::summary::headline(&session.intent.request, 70);
        println!("▸ {headline}");
        println!(
            "  {count} Zeile(n) · {} · {} · {}",
            session.agent.name,
            session.model.id,
            short_id(*id),
        );
    }

    if without > 0 {
        println!("\n{without} Zeile(n) ohne erfassten Kontext");
    }
    Ok(())
}

/// `b3-` plus die ersten zwölf Hex-Zeichen — genug, um Sessions zu unterscheiden.
fn short_id(id: SessionId) -> String {
    let s = id.to_string();
    if s.len() <= 15 {
        s
    } else {
        format!("{}…", &s[..15])
    }
}
