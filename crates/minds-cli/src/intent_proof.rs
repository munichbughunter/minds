//! Der Beleg, dass ein Intent-Anker über **redigiertes** Material gebaut
//! wurde (EA-14) — bevor seine Id in gespeicherte, gesyncte Sessions gelangt.
//!
//! `redact_intent` hält die Regel beim Bauen ein. Witness-Aktivierung und die
//! lokale Datei (A1) nennen aber nur eine Id bzw. einen Text; wer sie
//! erzeugt hat, ist offen. Ein Anker, dessen `content=` über einen
//! unredigierten Prompt gerechnet wurde oder dessen Blob-SHA eine Datei mit
//! Secret hasht, wäre neben einer bereinigten Kopie ein Orakel. Deshalb
//! zählt eine Id nur, wenn sie hier belegt ist:
//!
//! 1. Anker im Store, hash-geprüft (`get_intent`);
//! 2. der abgelegte Snapshot hasht auf `content=`;
//! 3. die Policy findet im Snapshot nichts (er ist schon bereinigt);
//! 4. die Policy findet in Quelle, Scope und Textform nichts
//!    (`check_intent_anchor`, dieselbe Prüfung wie beim Bauen);
//! 5. bei `file:` ist die Blob-Id genau der Git-Hash dieser Snapshot-Bytes —
//!    sie hasht also nichts anderes als den bereinigten Text.

use minds_core::ContentHash;
use minds_core::intent_anchor::{IntentSource, content_hash};
use minds_git::Repo;
use minds_redact::RedactionPipeline;
use minds_store::ContextStore;

/// Prüft den Beleg für `id`. Der Fehler ist ein fester Grund — nie ein
/// Wert aus dem Store.
pub(crate) fn proven(
    store: &dyn ContextStore,
    repo: &Repo,
    pipeline: &RedactionPipeline,
    id: &ContentHash,
) -> Result<(), &'static str> {
    let stored = match store.get_intent(id) {
        Ok(Some(stored)) => stored,
        Ok(None) => return Err("intent anchor is not in the store"),
        Err(_) => return Err("intent anchor in the store is unreadable or altered"),
    };
    if content_hash(&stored.snapshot) != stored.anchor.content {
        return Err("intent snapshot does not match its anchor");
    }
    let snapshot =
        std::str::from_utf8(&stored.snapshot).map_err(|_| "intent snapshot is not valid UTF-8")?;
    // Wie beim Bauen (`redact_field`): Auch ein verworfener, vertragswidriger
    // Fund heißt „nicht nachweislich sauber".
    let checked = pipeline.redact(snapshot);
    if pipeline.is_empty() || checked.invalid_findings > 0 || checked.text != snapshot {
        return Err("intent snapshot is not clean under the redaction policy");
    }
    match pipeline.check_intent_anchor(&stored.anchor) {
        Ok(text) if text == stored.text => {}
        _ => return Err("intent anchor refused by the redaction policy"),
    }
    if let IntentSource::File { blob, .. } = &stored.anchor.source {
        match repo.blob_id_of(&stored.snapshot) {
            Ok(actual) if actual == *blob => {}
            _ => return Err("intent file snapshot is not the named blob"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::intent_anchor::IntentAnchor;
    use minds_store::InRepoStore;

    fn fixture() -> (tempfile::TempDir, Repo, InRepoStore, RedactionPipeline) {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
        let repo = Repo::discover(dir.path()).unwrap();
        let store = InRepoStore::open(dir.path()).unwrap();
        let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
        (dir, repo, store, pipeline)
    }

    #[test]
    fn an_anchor_built_by_redact_intent_is_proven() {
        let (_dir, repo, store, pipeline) = fixture();
        let text = b"Retry exponentiell.\n";
        let blob = repo.blob_id_of(text).unwrap();
        for source in [
            IntentSource::Prompt,
            IntentSource::File {
                path: "docs/spec.md".into(),
                blob,
            },
        ] {
            let intent = pipeline
                .redact_intent(source, vec!["src/**".into()], text.to_vec())
                .unwrap();
            let id = store.put_intent(&intent).unwrap();
            assert_eq!(proven(&store, &repo, &pipeline, &id), Ok(()));
        }
    }

    /// Security-Review Iteration 4: Ids ohne Beleg werden nie gebunden.
    #[test]
    fn unproven_anchors_are_refused() {
        let (_dir, repo, store, pipeline) = fixture();
        // Nicht im Store: etwa `content=` über einen unredigierten Prompt,
        // von einem fremden Client gerechnet.
        let raw = IntentAnchor {
            source: IntentSource::Prompt,
            content: content_hash(b"deploy with DB_PASSWORD=hunter2\n"),
            scope: Vec::new(),
        }
        .to_text()
        .unwrap();
        assert_eq!(
            proven(&store, &repo, &pipeline, &IntentAnchor::id_of_text(&raw)),
            Err("intent anchor is not in the store")
        );

        // Datei-Quelle, deren Blob-Id nicht der Snapshot ist (etwa die
        // Fassung mit Secret aus dem Index, der Snapshot aus dem bereinigten
        // Worktree).
        let other_blob = repo.blob_id_of(b"DB_PASSWORD=hunter2\n").unwrap();
        let intent = pipeline
            .redact_intent(
                IntentSource::File {
                    path: "docs/spec.md".into(),
                    blob: other_blob,
                },
                Vec::new(),
                b"Retry exponentiell.\n".to_vec(),
            )
            .unwrap();
        let id = store.put_intent(&intent).unwrap();
        assert_eq!(
            proven(&store, &repo, &pipeline, &id),
            Err("intent file snapshot is not the named blob")
        );
    }

    /// Ein schon redigierter Snapshot (mit Platzhaltern) ist belegt; ein
    /// gepflanzter mit Klartext-Secret nicht.
    #[test]
    fn placeholders_pass_and_planted_secrets_fail() {
        let (dir, repo, store, pipeline) = fixture();
        let intent = pipeline
            .redact_intent(
                IntentSource::Prompt,
                Vec::new(),
                b"Deploy with token ghp_R4nd0mT0k3nV4lu3F0rT3st1ngPurp0s3s00\n".to_vec(),
            )
            .unwrap();
        let id = store.put_intent(&intent).unwrap();
        assert_eq!(proven(&store, &repo, &pipeline, &id), Ok(()));

        // Von Hand gepflanzt: `content=` passt, der Snapshot ist aber Klartext.
        let snapshot = b"DB_PASSWORD=hunter2\n";
        let text = IntentAnchor {
            source: IntentSource::Prompt,
            content: content_hash(snapshot),
            scope: Vec::new(),
        }
        .to_text()
        .unwrap();
        let id = IntentAnchor::id_of_text(&text);
        let git = |args: &[&str], stdin: Option<&str>| {
            use std::io::Write;
            let mut child = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(input) = stdin {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.as_bytes())
                    .unwrap();
            }
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        let a = git(&["hash-object", "-w", "--stdin"], Some(&text));
        let s = git(
            &["hash-object", "-w", "--stdin"],
            Some(std::str::from_utf8(snapshot).unwrap()),
        );
        let tree = git(
            &["mktree"],
            Some(&format!(
                "100644 blob {a}\tanchor\n100644 blob {s}\tsnapshot\n"
            )),
        );
        let commit = git(&["commit-tree", &tree, "-m", "planted"], None);
        git(
            &[
                "update-ref",
                &format!("refs/minds/intents/{}", id.hex()),
                &commit,
            ],
            None,
        );
        assert_eq!(
            proven(&store, &repo, &pipeline, &id),
            Err("intent snapshot is not clean under the redaction policy")
        );
    }

    #[test]
    fn blob_ids_are_git_blob_ids() {
        let (dir, repo, _store, _pipeline) = fixture();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, b"hello\n").unwrap();
        let out = std::process::Command::new("git")
            .args(["hash-object", path.to_str().unwrap()])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(
            repo.blob_id_of(b"hello\n").unwrap(),
            String::from_utf8(out.stdout).unwrap().trim()
        );
    }
}
