//! `minds disable` — nimmt zurück, was `minds enable` eingerichtet hat:
//! die Hook-Registrierungen des Agenten, die Git-Hooks, den Witness dieses
//! Repos (Dienst und Home; beim geschützten mit **einer** Erhöhung, die auch
//! seinen Eintrag in den Managed Settings nimmt). `--all` räumt jeden
//! geschützten Witness des Rechners ab, samt Managed Settings und
//! root-eigenem Binary.
//!
//! Was bleibt: der Store (`refs/minds/*`) und die Store-Konfiguration in
//! `.git/config` — das sind Daten, keine Einrichtung; `minds show` liest sie
//! weiter. Nutzer und Gruppe des Witness bleiben ebenfalls (ein Konto zu
//! löschen ist mehr, als ein `disable` tun sollte); die Vertrauensdatei und
//! der Intent-Schlüssel des Nutzers gelten für alle Repos und bleiben.

use std::process::ExitCode;

pub fn run(all: bool, verbose: bool) -> ExitCode {
    let paths = match crate::enable::locate() {
        Ok(paths) => Some(paths),
        Err(err) if all => {
            eprintln!("minds: not in a repository ({err}) — removing the protected witnesses only");
            None
        }
        Err(err) => {
            eprintln!("minds disable: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut lines = Vec::new();
    let mut failed = false;
    if let Some(paths) = &paths {
        let root = paths.root();
        match crate::enable::disable_agents(root) {
            Ok(done) => lines.extend(done),
            Err(err) => {
                failed = true;
                crate::text::warn(&format!(
                    "agent hooks not removed: {}",
                    crate::text::sanitize(&err.to_string())
                ));
            }
        }
        match crate::enable::disable_git_hooks(paths) {
            Ok(done) => lines.extend(done),
            Err(err) => {
                failed = true;
                crate::text::warn(&format!(
                    "git hooks not removed: {}",
                    crate::text::sanitize(&err.to_string())
                ));
            }
        }
        match crate::enable_witness::disable_local(root) {
            Ok(done) => {
                failed |= done.failed;
                lines.extend(done.lines);
            }
            Err(err) => {
                failed = true;
                crate::text::warn(&format!(
                    "local witness not removed: {}",
                    crate::text::sanitize(&err.to_string())
                ));
            }
        }
    }
    #[cfg(unix)]
    {
        let root = paths.as_ref().map(|paths| paths.root().to_path_buf());
        let protected = root
            .as_deref()
            .is_some_and(crate::enable_protected::installed_for);
        if all || protected {
            match crate::enable_protected::uninstall(root.as_deref(), all) {
                Ok(done) => lines.extend(done),
                Err(err) => {
                    failed = true;
                    crate::text::warn(&format!(
                        "protected witness not removed: {}",
                        crate::text::sanitize(&err.to_string())
                    ));
                }
            }
        } else if verbose {
            lines.push("no protected witness for this repository".to_owned());
        }
    }
    #[cfg(not(unix))]
    let _ = verbose;
    for line in &lines {
        println!("minds: {}", crate::text::sanitize(line));
    }
    if lines.is_empty() {
        println!("minds: nothing to remove");
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
