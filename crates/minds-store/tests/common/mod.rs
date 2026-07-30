//! Gemeinsame Helfer der Integrationstests.
//!
//! Zwei Test-Binaries (`roundtrip.rs` für den einen Klon, `child_repo_roundtrip.rs`
//! für die geteilte Ablage) brauchen denselben Aufbau: ein Repo mit Code, ein
//! Topic-Branch, ein Rewrite, der Weg vom Commit zur SessionId. Cargo übersetzt
//! jede Datei in `tests/` zu einem eigenen Crate; geteilt wird über ein
//! Unterverzeichnis-Modul wie dieses.
//!
//! Die Fixture ist eingebunden statt kopiert — siehe [`fixture`]. Der Preis des
//! Musters steht in der ersten Zeile: Beide Binaries übersetzen dieses Modul
//! vollständig, benutzen aber je nur einen Teil davon. Ungenutzte Helfer wären
//! sonst Warnungen und mit `-D warnings` Fehler — als toter Code die Funktionen,
//! als ungenutzter Import die Re-Exporte. Die Ausnahme gilt nur hier; in den
//! Testdateien selbst fällt beides weiterhin auf.

#![allow(dead_code, unused_imports)]

#[path = "../../src/fixture.rs"]
mod fixture;

pub(crate) use fixture::{TempRepo, init_bare_at, redacted};

use minds_core::SessionId;
use minds_git::Repo;
use minds_store::ContextStore;

/// Ein Repository mit einem Commit auf `main`.
pub(crate) fn repo_with_code() -> TempRepo {
    let fixture = TempRepo::init();
    fixture.write_file("src/lib.rs", "fn main() {}\n");
    fixture.commit("chore: Grundgerüst");
    fixture
}

/// Das Repository des **Codes** — die Quelle der Commits, nicht die des
/// Kontexts.
///
/// Bewusst getrennt vom Store geöffnet: Beim Child-Backend ist das Repo des
/// Stores ein anderes, und der Trailer gehört immer hierher.
pub(crate) fn open_code_repo(fixture: &TempRepo) -> Repo {
    Repo::open(fixture.path()).unwrap()
}

/// Der Weg, den `minds capture` (M6) gehen wird, hier von Hand: Session
/// ablegen — wo auch immer `store` sie hinlegt — und den Trailer an den
/// Produktions-Commit hängen.
pub(crate) fn capture(store: &dyn ContextStore, code: &TempRepo, request: &str) -> SessionId {
    let id = store.put(&redacted(request)).unwrap().id();

    open_code_repo(code)
        .amend_head_with_sessions(&[id])
        .expect("HEAD nimmt den Trailer auf");

    id
}

/// Der Rückweg: Commit an HEAD → Trailer → SessionIds.
pub(crate) fn ids_at_head(fixture: &TempRepo) -> Vec<SessionId> {
    let repo = open_code_repo(fixture);
    let head = repo
        .head()
        .unwrap()
        .commit()
        .expect("HEAD hat einen Commit");

    repo.session_ids_of(head).unwrap()
}

/// Legt einen Topic-Branch mit einem Commit an und bleibt dort stehen.
pub(crate) fn start_topic(fixture: &TempRepo) {
    fixture.git(&["checkout", "-q", "-b", "topic"]);
    fixture.write_file("src/retry.rs", "fn retry() {}\n");
    fixture.commit("fix: Retry-Backoff verlängert");
}

/// Lässt `main` weiterlaufen und bleibt dort stehen.
///
/// Ohne diesen Schritt sind Rebase und Cherry-Pick entartet: Der Commit bekäme
/// denselben Elter, denselben Baum und dieselbe Message — und weil die Fixture
/// auch die Zeitstempel festhält, wäre das Commit-Objekt byte-identisch und Git
/// gäbe denselben Hash zurück. Ein Test, der den Rewrite prüfen will, braucht
/// einen echten Rewrite.
pub(crate) fn advance_main(fixture: &TempRepo) {
    fixture.git(&["checkout", "-q", "main"]);
    fixture.write_file("README.md", "# minds\n");
    fixture.commit("docs: README");
}

/// Schreibt den Topic-Commit neu: `main` läuft weiter, dann `git rebase main`.
pub(crate) fn rebase_topic_onto_main(fixture: &TempRepo) {
    advance_main(fixture);
    fixture.git(&["checkout", "-q", "topic"]);
    fixture.git(&["rebase", "-q", "main"]);
}

/// Der Ref, unter dem die Nutzlast einer Session liegt.
///
/// Spiegelt `minds_store`'s internes Layout — bewusst hier nachgebaut statt
/// exportiert: Was der Store innen tut, soll er ändern dürfen, ohne dass es
/// öffentliche API wird. Fällt es auseinander, fallen genau die Tests um, die
/// das Layout zusichern.
pub(crate) fn session_ref(id: SessionId) -> String {
    format!(
        "refs/minds/store/{}",
        id.to_string().strip_prefix("b3-").expect("Präfix")
    )
}
