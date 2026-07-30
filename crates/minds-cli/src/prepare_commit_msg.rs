//! `minds prepare-commit-msg <datei>` — sorgt für eine stabile Change-Id.
//!
//! Der `prepare-commit-msg`-Hook (von `minds enable` installiert) ruft das hier
//! mit der Message-Datei auf. Fehlt eine `Minds-Change-Id`, wird eine erzeugt und
//! als Trailer angehängt; ist schon eine da (amend, rebase, cherry-pick — die
//! Message wird mitgeführt), bleibt sie unangetastet. So trägt eine logische
//! Änderung über all diese Operationen hinweg **dieselbe** Identität.
//!
//! # Grenze, ehrlich benannt
//!
//! Bei einem interaktiven Commit *ohne* `-m` ist die Message zum Zeitpunkt von
//! `prepare-commit-msg` noch leer (nur das Editor-Template). Dann wird **nichts**
//! angehängt — ein Trailer würde sonst zum Betreff. Erfasst werden damit sicher
//! `-m`, `amend`, `rebase`, `cherry-pick` und `squash` — genau die Operationen,
//! um deren Überleben es bei der Change-Id geht. Der interaktive Erst-Commit ohne
//! `-m` bleibt außen vor (dort wäre ein `commit-msg`-Hook der richtigere Ort).

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use minds_core::{ChangeId, Trailer};

/// Führt `minds prepare-commit-msg` aus. `file` ist die Message-Datei (`$1`).
pub fn run(file: Option<&str>) -> ExitCode {
    // Ohne Datei nichts tun — der Hook reicht sie immer herein. Wie der ganze
    // Hook-Pfad: im Zweifel geräuschlos durchlassen, nie den Commit blockieren.
    let Some(file) = file else {
        return ExitCode::SUCCESS;
    };
    match prepare(file) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds prepare-commit-msg: {err}");
            ExitCode::FAILURE
        }
    }
}

fn prepare(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(file)?;
    if let Some(updated) = ensure_change_id(&content, &generate()) {
        std::fs::write(file, updated)?;
    }
    Ok(())
}

/// Fügt eine Change-Id ein, falls nötig. `None`, wenn die Message unverändert
/// bleibt (schon eine da, oder noch kein Betreff).
fn ensure_change_id(content: &str, change_id: &ChangeId) -> Option<String> {
    let (body, comments) = split_comments(content);
    if Trailer::change_id(body).is_some() {
        return None;
    }
    // Kein Betreff (interaktiver Editor vor der Eingabe) → nichts anhängen, der
    // Trailer würde sonst zum Betreff.
    if body.trim().is_empty() {
        return None;
    }

    let mut out = Trailer::append(body, &Trailer::ChangeId(change_id.clone()));
    if !comments.is_empty() {
        out.push('\n');
        out.push_str(comments);
    }
    Some(out)
}

/// Trennt die Nachricht vom Kommentarblock. Git-Kommentarzeilen beginnen mit `#`
/// (in Spalte 0) und stehen als Block am Ende; alles davor ist die Nachricht.
fn split_comments(content: &str) -> (&str, &str) {
    let mut offset = content.len();
    let mut pos = 0;
    for line in content.split_inclusive('\n') {
        if line.starts_with('#') {
            offset = pos;
            break;
        }
        pos += line.len();
    }
    (&content[..offset], &content[offset..])
}

/// Erzeugt eine Change-Id aus Zeit und Prozess-Id — eindeutig genug (eine
/// Kollision bräuchte zwei Commits in derselben Nanosekunde aus demselben
/// Prozess). Kein Geheimnis, deshalb reicht Eindeutigkeit statt Zufall; ein
/// splitmix64 verteilt die Bits gleichmäßig, damit die Id wie eine echte
/// aussieht (statt einer Zahl mit vielen Null-Bytes).
fn generate() -> ChangeId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos ^ (u64::from(std::process::id())).rotate_left(32);

    let mut bytes = [0u8; 20];
    for chunk in bytes.chunks_mut(8) {
        let word = splitmix64(&mut state).to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
    ChangeId::from_bytes(bytes)
}

/// Ein Schritt des splitmix64-Generators.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid() -> ChangeId {
        format!("I{}", "ab".repeat(20)).parse().unwrap()
    }

    #[test]
    fn a_message_with_a_subject_gets_a_change_id() {
        let out = ensure_change_id("fix: etwas", &cid()).unwrap();
        assert!(out.contains(&format!("Minds-Change-Id: I{}", "ab".repeat(20))));
        assert!(out.starts_with("fix: etwas\n\nMinds-Change-Id:"));
    }

    #[test]
    fn an_existing_change_id_is_preserved() {
        // amend/rebase führen die Message mit — die Id bleibt dieselbe.
        let content = format!("fix: etwas\n\nMinds-Change-Id: I{}\n", "cd".repeat(20));
        assert_eq!(ensure_change_id(&content, &cid()), None);
    }

    #[test]
    fn the_change_id_lands_before_the_comment_block() {
        let content = "fix: etwas\n# bitte gib die Commit-Nachricht ein\n# mit '#'\n";
        let out = ensure_change_id(content, &cid()).unwrap();
        let id_pos = out.find("Minds-Change-Id").unwrap();
        let comment_pos = out.find("# bitte").unwrap();
        assert!(
            id_pos < comment_pos,
            "Trailer muss vor die Kommentare:\n{out}"
        );
        // Die Kommentare bleiben erhalten (Git streift sie selbst).
        assert!(out.contains("# bitte gib die Commit-Nachricht ein"));
    }

    #[test]
    fn an_empty_template_gets_no_change_id() {
        // Interaktiver Commit vor der Eingabe: nur Kommentare, kein Betreff.
        let content = "\n# bitte gib die Commit-Nachricht ein\n";
        assert_eq!(ensure_change_id(content, &cid()), None);
    }

    #[test]
    fn ensure_is_idempotent() {
        let once = ensure_change_id("fix: etwas", &cid()).unwrap();
        // Ein zweiter Lauf (mit einer *anderen* Id) sieht die vorhandene und tut
        // nichts.
        let other: ChangeId = format!("I{}", "12".repeat(20)).parse().unwrap();
        assert_eq!(ensure_change_id(&once, &other), None);
    }

    #[test]
    fn generated_ids_are_wellformed() {
        let id = generate();
        assert!(id.to_string().starts_with('I'));
        assert_eq!(id.hex().len(), 40);
    }
}
