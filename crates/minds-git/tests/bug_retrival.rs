//! Der Bug-Retrieval-Flow aus dem Plan, eine Ebene unter der CLI.
//!
//! ```text
//! Buggy-Zeile → git blame → Commit → Trailer → SessionId → Store → Prompt
//! ```
//!
//! Die Unit-Tests prüfen jeden Pfeil für sich; hier läuft die Kette am Stück
//! und ausschließlich über die öffentliche API. Das ist der Unterschied, der
//! diese Ebene rechtfertigt: Ein Modul kann für sich richtig sein und trotzdem
//! nicht zum nächsten passen — etwa wenn `blame` Zeilen ab 0 zählte und
//! `session_ids_of` das nie zu sehen bekäme.
//!
//! Was hier nicht geprüft wird: ob die SessionId zum Inhalt der Session passt.
//! Das ist Content-Adressierung und gehört `minds-core` und M4; auf dieser
//! Ebene ist die Id ein Wert, der unverändert durchlaufen muss.

mod support;

use minds_git::{BlameProvider, DEFAULT_CONTEXT_REF, Repo};
use support::{TempRepo, session_id, session_path, store_session};

/// Der Text, der am Ende wieder herauskommen muss — ASCII, damit der Vergleich
/// auf Byte-Ebene lesbar bleibt.
const SESSION_BODY: &str = r#"{"intent":{"request":"Backoff verlaengern"}}"#;

#[test]
fn from_a_buggy_line_to_the_prompt_behind_it() {
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "fn retry() {}\nfn backoff() {}\n");
    fixture.commit("feat: Retry mit Backoff");
    let repo = Repo::open(fixture.path()).unwrap();

    // Hinweg: Session ablegen, Production-Commit verlinken.
    let session = session_id('a');
    let path = store_session(&repo, session, SESSION_BODY);
    let update = repo.amend_head_with_sessions(&[session]).unwrap();
    assert!(update.rewrote_head());

    // Rückweg, wie ihn `minds why src/retry.rs:2` gehen wird.
    let head = repo.head().unwrap().commit().unwrap();
    let commit = repo
        .blame()
        .blame_line(head, "src/retry.rs", 2)
        .unwrap()
        .expect("Zeile 2 gibt es");
    assert_eq!(commit, head);

    assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);

    let stored = repo.read_blob_at(DEFAULT_CONTEXT_REF, &path).unwrap();
    assert_eq!(stored.as_deref(), Some(SESSION_BODY.as_bytes()));
}

#[test]
fn the_loop_survives_a_rebase() {
    // Punkt 3 der Definition of Done: Der Verweis übersteht einen Rebase — und
    // zwar so, dass er danach *auflösbar* bleibt, nicht nur vorhanden ist.
    let fixture = TempRepo::init();
    fixture.write_file("a.txt", "a\n");
    fixture.commit("base");

    fixture.git(&["checkout", "--quiet", "-b", "feature"]);
    fixture.write_file("src/retry.rs", "fn retry() {}\n");
    fixture.commit("feat: Retry");

    let repo = Repo::open(fixture.path()).unwrap();
    let session = session_id('b');
    let path = store_session(&repo, session, SESSION_BODY);
    let before = repo.amend_head_with_sessions(&[session]).unwrap().commit();

    fixture.git(&["checkout", "--quiet", "main"]);
    fixture.write_file("c.txt", "c\n");
    fixture.commit("main läuft weiter");
    fixture.git(&["checkout", "--quiet", "feature"]);
    fixture.git(&["rebase", "--quiet", "main"]);

    let head = repo.head().unwrap().commit().unwrap();
    assert_ne!(head, before, "der Rebase hat den Hash geändert");

    let commit = repo
        .blame()
        .blame_line(head, "src/retry.rs", 1)
        .unwrap()
        .expect("Zeile 1 gibt es");
    assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
    assert!(
        repo.read_blob_at(DEFAULT_CONTEXT_REF, &path)
            .unwrap()
            .is_some(),
        "der Kontext-Ref hat den Rebase des Code-Branches nicht bemerkt"
    );
}

#[test]
fn two_sessions_in_one_file_stay_apart() {
    // Der Grund, warum `minds why` Zeilen braucht und nicht Dateien: Zwei
    // Agent-Läufe an derselben Datei müssen an verschiedenen Zeilen hängen.
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "erste Zeile\nzweite Zeile\n");
    fixture.commit("feat: erste Fassung");
    let repo = Repo::open(fixture.path()).unwrap();

    let first = session_id('c');
    store_session(&repo, first, SESSION_BODY);
    repo.amend_head_with_sessions(&[first]).unwrap();

    fixture.write_file("src/retry.rs", "erste Zeile\nZWEITE ZEILE\n");
    fixture.commit("fix: zweite Zeile");
    let second = session_id('d');
    store_session(&repo, second, SESSION_BODY);
    repo.amend_head_with_sessions(&[second]).unwrap();

    let head = repo.head().unwrap().commit().unwrap();
    let blame = repo.blame();
    let from_line_one = blame.blame_line(head, "src/retry.rs", 1).unwrap().unwrap();
    let from_line_two = blame.blame_line(head, "src/retry.rs", 2).unwrap().unwrap();

    assert_ne!(from_line_one, from_line_two);
    assert_eq!(repo.session_ids_of(from_line_one).unwrap(), vec![first]);
    assert_eq!(repo.session_ids_of(from_line_two).unwrap(), vec![second]);
}

#[test]
fn several_sessions_on_one_commit_all_resolve() {
    // Mehrere Läufe, ein Commit — beide Verweise müssen bis in den Store
    // durchgehen.
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "fn retry() {}\n");
    fixture.commit("feat: Retry");
    let repo = Repo::open(fixture.path()).unwrap();

    let (first, second) = (session_id('e'), session_id('f'));
    store_session(&repo, first, SESSION_BODY);
    store_session(&repo, second, SESSION_BODY);
    repo.amend_head_with_sessions(&[first, second]).unwrap();

    let head = repo.head().unwrap().commit().unwrap();
    assert_eq!(repo.session_ids_of(head).unwrap(), vec![first, second]);

    for session in [first, second] {
        assert!(
            repo.read_blob_at(DEFAULT_CONTEXT_REF, &session_path(session))
                .unwrap()
                .is_some(),
            "{session} fehlt im Store"
        );
    }
}

#[test]
fn a_line_from_a_hand_written_commit_carries_no_session() {
    // Der häufigste Fall in jedem echten Repo — und kein Fehler, sondern eine
    // leere Antwort. `minds fsck` unterscheidet später genau daran zwischen
    // „ohne Kontext" und „Waise".
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "von Hand geschrieben\n");
    let commit = fixture.commit("feat: ganz normal von Hand");
    let repo = Repo::open(fixture.path()).unwrap();

    let found = repo
        .blame()
        .blame_line(commit, "src/retry.rs", 1)
        .unwrap()
        .unwrap();

    assert_eq!(found, commit);
    assert!(repo.session_ids_of(found).unwrap().is_empty());
}

#[test]
fn a_trailer_without_a_stored_session_is_visible_as_an_orphan() {
    // Die Vorarbeit für `minds fsck`: Der Verweis steht am Commit, der Store
    // kennt ihn nicht. Beides zusammen ist auf dieser Ebene erkennbar — ohne
    // dass irgendetwas fehlschlägt.
    let fixture = TempRepo::init();
    fixture.write_file("src/retry.rs", "fn retry() {}\n");
    fixture.commit("feat: Retry");
    let repo = Repo::open(fixture.path()).unwrap();

    let session = session_id('9');
    repo.amend_head_with_sessions(&[session]).unwrap();

    let head = repo.head().unwrap().commit().unwrap();
    assert_eq!(repo.session_ids_of(head).unwrap(), vec![session]);
    assert_eq!(
        repo.read_blob_at(DEFAULT_CONTEXT_REF, &session_path(session))
            .unwrap(),
        None
    );
}
