//! Derselbe Kern-Loop, aber mit geteilter Ablage: **Verweis im Parent,
//! Nutzlast im Child.**
//!
//! ```text
//! Parent-Repo                          Child-Repo
//! ├─ Code                              └─ refs/minds/context
//! └─ Commit mit Minds-Session-Id  ─────────→ sessions/b3/<hex>.json
//! ```
//!
//! `roundtrip.rs` prüft dieselbe Kette im einen Klon. Was hier dazukommt, ist
//! die Trennung — und mit ihr die drei Fragen, die es vorher nicht gab:
//!
//! - Bleibt der Parent wirklich sauber, oder rutscht doch eine Session hinein?
//! - Übersteht die Verlinkung einen Rewrite im Parent, obwohl die Nutzlast in
//!   einem Repo liegt, das von dem Rewrite nichts mitbekommt?
//! - Was passiert, wenn das Child gerade **nicht** da ist? Der Plan verspricht
//!   *graceful degradation*: Trailer und Commit bleiben, nachgeladen wird
//!   später. Ein harter Fehler an der falschen Stelle würde daraus einen
//!   Ausfall machen.
//!
//! Die vierte Frage ist die aus der Vision: Wandert der Kontext mit dem Repo?
//! `the_context_travels_over_a_plain_git_fetch` beantwortet sie mit einem
//! gewöhnlichen `git fetch` und einer expliziten Refspec — ohne Minds, ohne
//! Dienst, ohne Format, das jemand kennen müsste.

mod common;

use std::fs;

use minds_store::{ChildRepoStore, ContextStore, InRepoStore, StoreConfig, StoreError};

use common::{
    TempRepo, capture, ids_at_head, init_bare_at, rebase_topic_onto_main, redacted, repo_with_code,
    session_ref, start_topic,
};

/// Ein Parent mit Code und ein bares Child daneben — die Aufteilung, die die
/// Konfiguration meint.
fn parent_and_child() -> (TempRepo, TempRepo, ChildRepoStore) {
    let parent = repo_with_code();
    let child = TempRepo::init_bare();
    let store = ChildRepoStore::open(child.path()).unwrap();

    (parent, child, store)
}

// --- Die Kette über zwei Repositories ----------------------------------------

#[test]
fn the_chain_holds_across_two_repositories() {
    let (parent, child, store) = parent_and_child();

    let id = capture(&store, &parent, "Der Retry-Test flackert, bitte fixen.");

    // Der Verweis steht im Parent …
    assert_eq!(ids_at_head(&parent), vec![id]);
    // … die Nutzlast liegt im Child.
    let session = store.get(id).unwrap().expect("die Session liegt im Child");
    assert_eq!(
        session.intent.request,
        "Der Retry-Test flackert, bitte fixen."
    );
    // Ein Ref je Session — im Child, nicht im Parent.
    let refs = child.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
    assert!(refs.contains("refs/minds/store/"), "{refs}");
}

#[test]
fn the_parent_carries_only_the_reference() {
    // Der ganze Grund für dieses Backend: Das Repo des Codes bekommt eine
    // Trailer-Zeile und sonst nichts — kein Ref, keine Session, kein
    // Klon-Ballast.
    let (parent, _child, store) = parent_and_child();

    let id = capture(&store, &parent, "Der Retry-Test flackert, bitte fixen.");

    assert!(
        parent
            .git(&["for-each-ref", "--format=%(refname)", "refs/minds/"])
            .trim()
            .is_empty(),
        "der Parent hat einen Minds-Ref bekommen"
    );
    // Und wer im Parent nachsieht, findet nichts — kein Fehler, nur leer.
    let in_parent = InRepoStore::open(parent.path()).unwrap();
    assert_eq!(in_parent.get(id).unwrap(), None);
    assert!(in_parent.list().unwrap().is_empty());
}

// --- Historie umschreiben ----------------------------------------------------

#[test]
fn a_rebase_in_the_parent_leaves_the_context_alone() {
    // Der Rewrite passiert in einem Repository, das vom Child nichts weiß —
    // und umgekehrt. Genau deshalb hält die Verbindung: Sie hängt an einem
    // Inhalt, nicht an einem Commit.
    let (parent, child, store) = parent_and_child();
    start_topic(&parent);
    let id = capture(&store, &parent, "Der Retry-Test flackert, bitte fixen.");
    let before = parent.hash("HEAD");
    let context_before = child.hash(&session_ref(id));

    rebase_topic_onto_main(&parent);

    assert_ne!(
        parent.hash("HEAD"),
        before,
        "ohne neuen Commit-Hash prüft dieser Test nichts"
    );
    assert_eq!(ids_at_head(&parent), vec![id]);
    assert_eq!(child.hash(&session_ref(id)), context_before);
    assert!(store.get(id).unwrap().is_some(), "die ID zeigt ins Leere");
}

// --- Wenn das Child nicht da ist ---------------------------------------------

#[test]
fn an_unreachable_child_repository_degrades_gracefully() {
    // Air-Gap, abgehängtes Laufwerk, noch nicht geklont: Die Nutzlast fehlt,
    // der Verweis nicht. Wer den Commit hat, weiß weiterhin, *welche* Session
    // ihn erzeugt hat, und kann sie später nachladen.
    let parent = repo_with_code();
    let child_path = parent.path().join("kontext.git");
    init_bare_at(&child_path);

    let config = StoreConfig::child_repo("kontext.git");
    let id = {
        let store = config.open(parent.path()).unwrap();
        capture(
            store.as_ref(),
            &parent,
            "Der Retry-Test flackert, bitte fixen.",
        )
    };

    fs::remove_dir_all(&child_path).unwrap();

    // Der Verweis ist unbeeindruckt — er liegt im Commit.
    assert_eq!(ids_at_head(&parent), vec![id]);

    // Nur das Nachschlagen scheitert, und zwar mit Ansage statt mit einem
    // leeren Ergebnis: „nicht da" und „hier nicht drin" dürfen sich nicht
    // gleich anfühlen.
    let Err(err) = config.open(parent.path()) else {
        panic!("ein verschwundenes Kontext-Repository muss auffallen");
    };
    assert!(
        matches!(err, StoreError::ChildRepo { .. }),
        "erwartet ChildRepo, war: {err:?}"
    );
}

// --- Der Kontext wandert mit dem Repo ----------------------------------------

#[test]
fn the_context_travels_over_a_plain_git_fetch() {
    // Die Zusage der Vision, nachgerechnet: Der Kontext ist gewöhnliches Git.
    // Ein `fetch` mit expliziter Refspec holt ihn — kein Dienst, kein Format,
    // das jemand kennen müsste. Genau diese Refspec richtet `minds init` ein.
    let parent = repo_with_code();
    let in_repo = InRepoStore::open(parent.path()).unwrap();
    let id = capture(&in_repo, &parent, "Der Retry-Test flackert, bitte fixen.");

    let elsewhere = TempRepo::init_bare();
    elsewhere.git(&[
        "fetch",
        parent.path().to_str().unwrap(),
        "refs/minds/store/*:refs/minds/store/*",
    ]);

    // Dasselbe Repo, das eben nur Kontext geholt hat, ist jetzt ein Child-Store.
    let child = ChildRepoStore::open(elsewhere.path()).unwrap();
    let session = child.get(id).unwrap().expect("die Session ist mitgereist");

    assert_eq!(
        session.intent.request,
        "Der Retry-Test flackert, bitte fixen."
    );
    assert_eq!(child.list().unwrap(), vec![id]);

    // Und nur Kontext: Der Code ist nicht mitgekommen.
    let refs = elsewhere.git(&["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs.trim(), session_ref(id));
}

// --- Umschalten --------------------------------------------------------------

#[test]
fn switching_the_backend_keeps_the_session_ids() {
    // „Umschaltbar ohne Code-Änderung": Dieselbe Session, zweimal abgelegt,
    // bekommt beide Male dieselbe ID — die Verweise in bestehenden Commits
    // bleiben also gültig, wenn jemand das Backend wechselt.
    let parent = repo_with_code();
    let child = TempRepo::init_bare();
    let session = redacted("Der Retry-Test flackert, bitte fixen.");

    let in_repo = StoreConfig::in_repo().open(parent.path()).unwrap();
    let in_child = StoreConfig::child_repo(child.path())
        .open(parent.path())
        .unwrap();

    let here = in_repo.put(&session).unwrap().id();
    let there = in_child.put(&session).unwrap().id();

    assert_eq!(here, there);
    assert!(in_repo.get(here).unwrap().is_some());
    assert!(in_child.get(here).unwrap().is_some());
}
