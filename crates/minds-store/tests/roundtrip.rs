//! Der Kern-Loop über drei Crates: erfassen, verlinken, Historie umschreiben,
//! wiederfinden.
//!
//! ```text
//! Session → redact → Store → SessionId → Trailer im Commit
//!                                   ↓ (rebase, cherry-pick, squash)
//! Commit → Trailer → SessionId → Store → Session
//! ```
//!
//! # Warum das hier steht und nicht in `minds-git`
//!
//! Dass ein Trailer einen Rebase überlebt, weist `minds-git` schon nach — mit
//! echtem `git`, in seinen eigenen Tests. Das wird hier nicht wiederholt. Neu
//! ist die andere Hälfte der Aussage: dass die ID nach dem Umschreiben **noch
//! auf etwas zeigt**. Ein Trailer, der einen Rebase übersteht und ins Leere
//! zeigt, wäre schlimmer als keiner.
//!
//! Dahinter stehen zwei Eigenschaften, die zusammen erst hier sichtbar werden:
//!
//! - Die [`SessionId`] hängt am **Inhalt** der Session, nicht am Commit. Ein
//!   neuer Commit-Hash ändert an ihr nichts.
//! - Der Kontext-Ref ist ein **Orphan**. Ein Rebase auf `main` läuft an ihm
//!   vorbei; die Nutzlast bewegt sich nicht mit, weil sie nie an der
//!   Code-Historie hing.
//!
//! # Nur öffentliche API
//!
//! Als Integrationstest sieht diese Datei von `minds-store` genau das, was
//! `minds-cli` (M6) auch sehen wird. Was hier umständlich ist, ist es dort
//! auch.
//!
//! # Aufbau und Fixture
//!
//! Beides liegt in `common` — dort steht auch, warum die Fixture eingebunden
//! und nicht kopiert ist.

mod common;

use minds_core::SessionId;
use minds_store::{ContextStore, InRepoStore};

use common::{
    TempRepo, advance_main, ids_at_head, rebase_topic_onto_main, redacted, repo_with_code,
    session_ref, start_topic,
};

/// Erfassen mit dem In-Repo-Backend: Kontext und Code teilen sich das
/// Repository.
fn capture(fixture: &TempRepo, request: &str) -> SessionId {
    let store = InRepoStore::open(fixture.path()).unwrap();
    common::capture(&store, fixture, request)
}

/// Ein Commit auf einem Topic-Branch, der eine Session erzeugt hat.
fn topic_with_session(fixture: &TempRepo, request: &str) -> SessionId {
    start_topic(fixture);
    capture(fixture, request)
}

// --- Roundtrip ---------------------------------------------------------------

#[test]
fn the_chain_from_commit_to_prompt_holds() {
    // Der Moment aus der Vision, nur ohne Oberfläche: von einem Commit zu dem,
    // was verlangt wurde.
    let fixture = repo_with_code();
    let id = capture(&fixture, "Der Retry-Test flackert, bitte fixen.");

    assert_eq!(ids_at_head(&fixture), vec![id]);

    let store = InRepoStore::open(fixture.path()).unwrap();
    let session = store.get(id).unwrap().expect("die Session liegt im Store");

    assert_eq!(
        session.intent.request,
        "Der Retry-Test flackert, bitte fixen."
    );
    assert!(session.redaction.applied);
}

#[test]
fn a_commit_without_minds_points_nowhere() {
    // Punkt 8 der Definition of Done von der Leseseite: Wer Minds nicht nutzt,
    // hat keine Trailer — und das ist kein Fehler, sondern eine leere Liste.
    let fixture = repo_with_code();
    let store = InRepoStore::open(fixture.path()).unwrap();

    assert!(ids_at_head(&fixture).is_empty());
    assert!(store.list().unwrap().is_empty());
}

// --- Historie umschreiben ----------------------------------------------------

#[test]
fn a_rebase_does_not_break_the_link() {
    let fixture = repo_with_code();
    let id = topic_with_session(&fixture, "Der Retry-Test flackert, bitte fixen.");
    let before = fixture.hash("HEAD");

    rebase_topic_onto_main(&fixture);

    assert_ne!(
        fixture.hash("HEAD"),
        before,
        "ohne neuen Commit-Hash prüft dieser Test nichts"
    );
    assert_eq!(ids_at_head(&fixture), vec![id]);

    let store = InRepoStore::open(fixture.path()).unwrap();
    assert!(store.get(id).unwrap().is_some(), "die ID zeigt ins Leere");
}

#[test]
fn a_rebase_never_moves_the_context_ref() {
    // Der Grund, warum die Nutzlast unbeeindruckt bleibt: Sie hängt an keinem
    // Branch. Ein Rebase auf main läuft an ihr vorbei.
    let fixture = repo_with_code();
    let id = topic_with_session(&fixture, "Der Retry-Test flackert, bitte fixen.");
    let context_before = fixture.hash(&session_ref(id));

    rebase_topic_onto_main(&fixture);

    assert_eq!(fixture.hash(&session_ref(id)), context_before);
}

#[test]
fn a_cherry_pick_carries_the_link_along() {
    let fixture = repo_with_code();
    let id = topic_with_session(&fixture, "Der Retry-Test flackert, bitte fixen.");
    let picked = fixture.hash("HEAD");

    // main muss sich bewegt haben, sonst entsteht beim Pflücken buchstäblich
    // derselbe Commit — siehe `advance_main`.
    advance_main(&fixture);
    fixture.git(&["cherry-pick", &picked]);

    assert_ne!(
        fixture.hash("HEAD"),
        picked,
        "ohne neuen Commit-Hash prüft dieser Test nichts"
    );
    assert_eq!(ids_at_head(&fixture), vec![id]);

    let store = InRepoStore::open(fixture.path()).unwrap();
    assert!(store.get(id).unwrap().is_some(), "die ID zeigt ins Leere");
}

#[test]
fn a_squash_collects_the_sessions_of_both_commits() {
    // „Mehrere Sessions haben beigetragen" — beim Squash sammeln sich die
    // Trailer, und das ist genau richtig.
    let fixture = repo_with_code();
    let first = topic_with_session(&fixture, "Der Retry-Test flackert, bitte fixen.");

    fixture.write_file("src/retry.rs", "fn retry() { /* zweiter Anlauf */ }\n");
    fixture.commit("fix: Backoff doch exponentiell");
    let second = capture(
        &fixture,
        "Der Fix von eben reicht nicht, er hängt jetzt ganz.",
    );

    fixture.git(&["checkout", "-q", "main"]);
    fixture.git(&["merge", "--squash", "-q", "topic"]);
    fixture.git(&["commit", "--no-edit", "-q"]);

    let ids = ids_at_head(&fixture);
    assert_eq!(ids.len(), 2, "beide Sessions müssen genannt sein: {ids:?}");
    assert!(ids.contains(&first));
    assert!(ids.contains(&second));

    // Und beide sind auflösbar — die Reihenfolge in der Message ist dabei egal.
    let store = InRepoStore::open(fixture.path()).unwrap();
    for id in ids {
        assert!(store.get(id).unwrap().is_some());
    }
}

// --- Dedup über einen Rewrite hinweg -----------------------------------------

#[test]
fn re_capturing_after_a_rebase_stores_nothing_new() {
    // Der Hook läuft nach dem Rebase noch einmal über dieselbe Session. Weil
    // die ID am Inhalt hängt und nicht am Commit, ist das ein Treffer im Store
    // und kein zweiter Eintrag.
    let request = "Der Retry-Test flackert, bitte fixen.";
    let fixture = repo_with_code();
    let id = topic_with_session(&fixture, request);

    rebase_topic_onto_main(&fixture);

    let objects_before = fixture.object_count();
    let store = InRepoStore::open(fixture.path()).unwrap();
    let again = store.put(&redacted(request)).unwrap();

    assert_eq!(again.id(), id, "derselbe Inhalt, dieselbe ID");
    assert!(!again.was_written());
    assert_eq!(fixture.object_count(), objects_before);
    assert_eq!(store.list().unwrap(), vec![id]);
}

#[test]
fn the_trailer_is_not_duplicated_when_the_hook_runs_again() {
    // Dieselbe Frage eine Ebene höher: Der zweite Lauf darf die Message nicht
    // ein zweites Mal verlängern.
    let fixture = repo_with_code();
    let id = capture(&fixture, "Der Retry-Test flackert, bitte fixen.");
    let after_first = fixture.hash("HEAD");

    let same_again = capture(&fixture, "Der Retry-Test flackert, bitte fixen.");

    assert_eq!(same_again, id);
    assert_eq!(
        fixture.hash("HEAD"),
        after_first,
        "HEAD wurde neu geschrieben"
    );
    assert_eq!(ids_at_head(&fixture), vec![id]);
}
