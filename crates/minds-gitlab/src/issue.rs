//! Issue-Snapshot und Versionsprüfung (EA-16): ein Intent-Anker, der auf
//! eine bestimmte Fassung eines GitLab-Issues zeigt.
//!
//! # Was gelesen wird
//!
//! - [`Project::issue_snapshot`]: Titel, Beschreibung, `updated_at` und
//!   `web_url` über die REST-API (`GET /projects/:id/issues/:iid`). Daraus
//!   baut `minds intent bind --issue` den Anker.
//! - [`Project::description_history`]: frühere Fassungen der Beschreibung
//!   über GraphQL (`SystemNoteMetadata.descriptionVersion.description`).
//!
//! # Die Historie wird nicht vorausgesetzt
//!
//! Nicht jede GitLab-Instanz bietet sie an: Ältere Versionen kennen das
//! Feld nicht (GraphQL antwortet mit `errors`), manche Editionen oder
//! Lizenzen liefern die Fassungen nicht aus (`descriptionVersion` bzw. sein
//! `description` bleibt `null`, obwohl die Beschreibung geändert wurde),
//! und ein Projekt, das über seine numerische Id angesprochen wird, ist für
//! GraphQL kein Pfad. Jeder dieser Fälle heißt [`History::Unavailable`] —
//! nie „nicht gefunden".
//!
//! Titel haben bei GitLab keine strukturierte Historie (nur Systemnotizen in
//! Markdown). Geprüft wird deshalb jede frühere Beschreibung zusammen mit
//! dem **heutigen** Titel. Wurde der Titel seit dem Binden geändert, ist
//! der Befund „nicht im lesbaren Teil der Historie" — weder „confirmed" noch
//! „not found". Bleiben die neuesten Notizen ungelesen (Seiten- oder
//! Zeitgrenze), kann eine solche Titeländerung unbemerkt sein; ein falsches
//! „confirmed" entsteht daraus nicht: Gepaart wird mit dem heutigen Titel,
//! ein Treffer heißt also, der Titel ist derselbe.
//!
//! # Was zurückkommt
//!
//! [`check_version`] gibt immer ein [`IssueVersion`] — auch im Fehlerfall:
//! Eine gescheiterte Prüfung ist ein Befund (`version check unavailable`),
//! nie ein Abbruch und nie eine Bestätigung.

use std::fmt;

use minds_core::intent_anchor::{IssueHistory, IssueVersion, MAX_SNAPSHOT};
use serde::Deserialize;

use crate::{GET, POST_JSON, Project};

// `GitlabError` mit eigenem `Display` statt `thiserror`: `minds-gitlab`
// kommt bisher ohne aus, und die Texte sind wenige feste Sätze.

/// Ein Issue, wie GitLab es heute liefert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSnapshot {
    /// Der Titel.
    pub title: String,
    /// Die Beschreibung; eine fehlende (`null`) ist der leere String.
    pub description: String,
    /// `updated_at`, RFC 3339, so wie GitLab es schreibt.
    pub updated_at: String,
    /// Der Link zur Ansicht im Browser (nur Anzeige).
    pub web_url: String,
    /// Das Issue ist vertraulich (`confidential`). Fehlt das Feld in der
    /// Antwort, gilt es als vertraulich — fail-closed.
    pub confidential: bool,
}

/// Was die Beschreibungs-Historie eines Issues hergibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum History {
    /// Diese Instanz bietet sie nicht an (siehe Modul-Doku).
    Unavailable,
    /// Die Fassungen der Beschreibung, die GitLab zu ihren Änderungen
    /// liefert — jede ist der Text **nach** einer Änderung.
    Versions {
        /// Die Texte.
        descriptions: Vec<String>,
        /// `false`, wenn nicht jede Änderung einen lesbaren Text hatte
        /// (gelöschte oder zu große Fassung), die Notizen mehr Seiten hatten,
        /// als gelesen werden, oder die Frist ablief.
        complete: bool,
        /// Wann die Beschreibung geändert wurde (`createdAt` jeder
        /// Änderungsnotiz). Den Text vor der ersten Änderung — die
        /// ursprüngliche Beschreibung — listet GitLab zu keiner Notiz; und
        /// Änderungen in kurzer Folge fasst es in einer Notiz zusammen, deren
        /// Fassung dabei überschrieben wird.
        edits_at: Vec<String>,
        /// Wann der Titel geändert wurde (`createdAt` jeder Notiz mit
        /// `action == "title"`; leer, wenn ohne Zeitpunkt). Titel haben keine
        /// Fassungen — eine Änderung nach dem Binden heißt: Welcher Titel
        /// damals galt, ist offen.
        title_changes: Vec<String>,
    },
}

/// Warum ein Aufruf keine verwertbare Antwort brachte. Der Text nennt nie
/// den Token und zitiert nichts aus der Antwort des Servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitlabError {
    /// HTTP 401 — der Token fehlt, ist abgelaufen oder ungültig.
    Unauthorized,
    /// HTTP 403 — der Token darf das Issue nicht lesen.
    Forbidden,
    /// HTTP 404 — Projekt oder Issue gibt es nicht (oder nicht sichtbar).
    NotFound,
    /// Ein anderer Status außerhalb von 2xx.
    Status(u16),
    /// Keine HTTP-Antwort (Verbindung, Zeitlimit, Größenlimit). Der Text
    /// ist curls Diagnose, ohne Token.
    Unreachable(String),
    /// Die Antwort hat nicht die erwartete Form.
    Malformed(&'static str),
}

impl GitlabError {
    fn from_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            other => Self::Status(other),
        }
    }

    /// Der feste Grund für [`IssueVersion::Unavailable`].
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized (HTTP 401)",
            Self::Forbidden => "forbidden (HTTP 403)",
            Self::NotFound => "issue not found (HTTP 404)",
            Self::Status(300..=399) => "redirected (HTTP 3xx) — wrong URL or a sign-in page?",
            Self::Status(429) => "rate limited (HTTP 429)",
            Self::Status(500..=599) => "GitLab server error (HTTP 5xx)",
            Self::Status(_) => "unexpected HTTP status",
            Self::Unreachable(_) => "GitLab not reachable",
            Self::Malformed(_) => "unexpected response",
        }
    }
}

impl fmt::Display for GitlabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Status(status) => write!(f, "unexpected HTTP status {status}"),
            // Der Text nennt schon „not reachable at <url>".
            Self::Unreachable(detail) => f.write_str(detail),
            Self::Malformed(what) => write!(f, "unexpected response from GitLab: {what}"),
            other => f.write_str(other.reason()),
        }
    }
}

impl std::error::Error for GitlabError {}

/// Wie viele Notizen GraphQL je Seite liefert (das Maximum der API).
const PAGE: u32 = 100;

/// Wie viele Seiten Notizen höchstens gelesen werden.
const MAX_PAGES: usize = 20;

/// Ab wann keine weitere Seite der Historie mehr angefragt wird. Geprüft
/// wird zwischen den Seiten: Im ungünstigsten Fall kommt eine Anfrage
/// (höchstens `MAX_TIME`) dazu.
const HISTORY_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// GitLab fasst Beschreibungs-Änderungen desselben Autors in kurzer Folge
/// zu einer Notiz zusammen und überschreibt deren Fassung. Eine Fassung,
/// die so kurz nach einer Änderung gebunden wurde, kann aus der Historie
/// verschwunden sein — großzügig bemessen.
const SQUASH_WINDOW_SECS: i64 = 15 * 60;

/// Die GraphQL-Fehlercodes, die heißen: Diese Instanz kennt die Abfrage
/// nicht (ältere Version, anderes Schema) — anders als ein vorübergehender
/// Fehler (Zeitlimit, Ratenbegrenzung, interner Fehler).
const SCHEMA_ERRORS: &[&str] = &[
    "undefinedField",
    "undefinedType",
    "argumentNotAccepted",
    "argumentLiteralsIncompatible",
    "variableNotDefined",
];

/// Die Abfrage der Beschreibungs-Fassungen. Nur Felder, die es seit der
/// Einführung von `DescriptionVersion.description` gibt.
const HISTORY_QUERY: &str = "query($path: ID!, $iid: String!, $first: Int!, $after: String) { \
     project(fullPath: $path) { issue(iid: $iid) { \
     notes(first: $first, after: $after) { \
     pageInfo { hasNextPage endCursor } \
     nodes { createdAt systemNoteMetadata { action descriptionVersion { description } } } \
     } } } }";

impl Project {
    /// Titel, Beschreibung, `updated_at` und `web_url` des Issues `iid`.
    pub fn issue_snapshot(&self, iid: u64) -> Result<IssueSnapshot, GitlabError> {
        #[derive(Deserialize)]
        struct Raw {
            iid: u64,
            title: String,
            description: Option<String>,
            updated_at: String,
            #[serde(default)]
            web_url: Option<String>,
            #[serde(default)]
            confidential: Option<bool>,
        }
        let response = self
            .exchange(
                GET,
                &self.rest_url(&format!("/projects/{}/issues/{iid}", self.project)),
                None,
            )
            .map_err(GitlabError::Unreachable)?;
        if !(200..300).contains(&response.status) {
            return Err(GitlabError::from_status(response.status));
        }
        let raw: Raw = serde_json::from_str(&response.body)
            .map_err(|_| GitlabError::Malformed("not an issue object"))?;
        // Die Antwort muss das gefragte Issue sein — sonst bände der Anker
        // einen fremden Text an diese Nummer.
        if raw.iid != iid {
            return Err(GitlabError::Malformed("the response names another issue"));
        }
        Ok(IssueSnapshot {
            title: raw.title,
            description: raw.description.unwrap_or_default(),
            updated_at: raw.updated_at,
            web_url: raw.web_url.unwrap_or_default(),
            confidential: raw.confidential.unwrap_or(true),
        })
    }

    /// Die Fassungen der Beschreibung — sofern diese Instanz sie anbietet
    /// (siehe Modul-Doku). `Err` nur, wenn GitLab den Zugriff verweigert,
    /// nicht antwortet oder die Abfrage vorübergehend scheitert.
    pub fn description_history(&self, iid: u64) -> Result<History, GitlabError> {
        let deadline = std::time::Instant::now() + HISTORY_BUDGET;
        let path = self.full_path();
        let mut descriptions = Vec::new();
        let mut edits = 0usize;
        let mut edits_at = Vec::new();
        let mut title_changes = Vec::new();
        let mut complete = true;
        let mut after: Option<String> = None;
        let mut pages = 0usize;
        loop {
            if pages == MAX_PAGES || std::time::Instant::now() >= deadline {
                complete = false;
                break;
            }
            pages += 1;
            let body = serde_json::json!({
                "query": HISTORY_QUERY,
                "variables": { "path": path, "iid": iid.to_string(), "first": PAGE, "after": after },
            });
            let response = self
                .exchange(
                    POST_JSON,
                    &format!("{}/api/graphql", self.base_url),
                    Some(&body.to_string()),
                )
                .map_err(GitlabError::Unreachable)?;
            match response.status {
                200..=299 => {}
                // Kein GraphQL-Endpunkt (oder abgeschaltet): keine Historie.
                404 => return Ok(History::Unavailable),
                status => return Err(GitlabError::from_status(status)),
            }
            let Some(page) = parse_page(&response.body)? else {
                return Ok(History::Unavailable);
            };
            for note in page.nodes {
                let Some(meta) = note.system_note_metadata else {
                    continue;
                };
                match meta.action.as_deref() {
                    Some("description") => {}
                    Some("title") => {
                        title_changes.push(note.created_at.unwrap_or_default());
                        continue;
                    }
                    _ => continue,
                }
                edits += 1;
                // Ohne lesbaren Zeitpunkt ist offen, was davor stand.
                if note.created_at.as_deref().and_then(seconds).is_none() {
                    complete = false;
                }
                edits_at.push(note.created_at.unwrap_or_default());
                match meta.description_version.and_then(|v| v.description) {
                    // Größer, als ein Anker sein kann: nicht behalten.
                    Some(text) if text.len() <= MAX_SNAPSHOT => descriptions.push(text),
                    // Gelöscht, nicht ausgeliefert oder zu groß: Die
                    // gebundene Fassung kann genau diese sein.
                    _ => complete = false,
                }
            }
            if !page.has_next_page {
                break;
            }
            after = page.end_cursor;
            if after.is_none() {
                complete = false;
                break;
            }
        }
        // Geändert, aber keine Fassung lesbar: Die Instanz liefert sie nicht
        // aus — das ist „nicht verfügbar", nicht „leer".
        if edits > 0 && descriptions.is_empty() {
            return Ok(History::Unavailable);
        }
        Ok(History::Versions {
            descriptions,
            complete,
            edits_at,
            title_changes,
        })
    }

    /// Der Projektpfad für GraphQL (`gruppe/projekt`): Die REST-Form trägt
    /// `/` als `%2F`.
    fn full_path(&self) -> String {
        self.project.replace("%2F", "/").replace("%2f", "/")
    }
}

/// Eine Seite Notizen.
struct Page {
    nodes: Vec<Note>,
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    #[serde(default)]
    created_at: Option<String>,
    system_note_metadata: Option<Metadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    action: Option<String>,
    description_version: Option<Version>,
}

#[derive(Deserialize)]
struct Version {
    description: Option<String>,
}

/// Liest eine GraphQL-Antwort. `Ok(None)`: Diese Instanz kann die Frage
/// nicht beantworten (ein Schemafehler wie ein unbekanntes Feld, oder
/// Projekt bzw. Issue für GraphQL nicht auffindbar). Jeder andere Fehler
/// (Zeitlimit, Ratenbegrenzung, interner Fehler) ist vorübergehend: `Err`.
fn parse_page(body: &str) -> Result<Option<Page>, GitlabError> {
    #[derive(Deserialize)]
    struct Root {
        data: Option<Data>,
        #[serde(default)]
        errors: Option<Vec<GraphqlError>>,
    }
    #[derive(Deserialize)]
    struct GraphqlError {
        #[serde(default)]
        extensions: Option<Extensions>,
    }
    #[derive(Deserialize)]
    struct Extensions {
        #[serde(default)]
        code: Option<String>,
    }
    #[derive(Deserialize)]
    struct Data {
        project: Option<ProjectNode>,
    }
    #[derive(Deserialize)]
    struct ProjectNode {
        issue: Option<IssueNode>,
    }
    #[derive(Deserialize)]
    struct IssueNode {
        notes: Notes,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Notes {
        page_info: PageInfo,
        nodes: Vec<Note>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PageInfo {
        has_next_page: bool,
        end_cursor: Option<String>,
    }

    let root: Root =
        serde_json::from_str(body).map_err(|_| GitlabError::Malformed("not a GraphQL response"))?;
    if let Some(errors) = root.errors.filter(|errors| !errors.is_empty()) {
        let schema = errors.iter().all(|error| {
            error
                .extensions
                .as_ref()
                .and_then(|e| e.code.as_deref())
                .is_some_and(|code| SCHEMA_ERRORS.contains(&code))
        });
        return if schema {
            Ok(None)
        } else {
            Err(GitlabError::Malformed("GraphQL error"))
        };
    }
    let Some(notes) = root
        .data
        .and_then(|data| data.project)
        .and_then(|project| project.issue)
        .map(|issue| issue.notes)
    else {
        return Ok(None);
    };
    Ok(Some(Page {
        nodes: notes.nodes,
        has_next_page: notes.page_info.has_next_page,
        end_cursor: notes.page_info.end_cursor,
    }))
}

/// Prüft, ob die gebundene Fassung des Issues `iid` existiert (hat).
///
/// `bound_updated_at` ist `updated_at` aus der Quelle des Ankers.
/// `matches(title, description)` sagt, ob diese Fassung genau den
/// `content=` des Ankers ergibt — der Aufrufer rechnet das mit derselben
/// Redaction nach, mit der gebunden wurde; `Err` heißt „nicht
/// nachrechenbar" und nennt den festen Grund.
///
/// Reihenfolge: Erst das heutige Issue (`current`), dann — nur wenn es sich
/// geändert hat — die Historie, sofern die Instanz sie anbietet.
/// `not found` nur, wenn sie nachweislich vollständig ist: jede Änderung
/// mit lesbarem, nachrechenbarem Text, die gebundene Fassung jünger als die
/// erste Änderung (die ursprüngliche Beschreibung listet GitLab nicht) und
/// der Titel seit dem Binden unverändert — sonst ist auch `confirmed`
/// unbelegt: Gepaart wird mit dem heutigen Titel.
///
/// Zeitpunkte werden sekundengenau verglichen, im Zweifel zur vorsichtigen
/// Seite: GraphQL liefert `createdAt` ohne Sekundenbruchteile, REST
/// `updated_at` mit — dieselbe Sekunde zählt als „womöglich danach".
pub fn check_version(
    project: &Project,
    iid: u64,
    bound_updated_at: &str,
    matches: impl Fn(&str, &str) -> Result<bool, &'static str>,
) -> IssueVersion {
    let current = match project.issue_snapshot(iid) {
        Ok(current) => current,
        Err(err) => return IssueVersion::Unavailable(err.reason()),
    };
    match matches(&current.title, &current.description) {
        Err(reason) => return IssueVersion::Unavailable(reason),
        Ok(true) => return IssueVersion::Current,
        Ok(false) => {}
    }
    let history = match project.description_history(iid) {
        Ok(History::Versions {
            descriptions,
            mut complete,
            edits_at,
            title_changes,
        }) => {
            let mut confirmed = false;
            for description in &descriptions {
                match matches(&current.title, description) {
                    Ok(true) => {
                        confirmed = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => complete = false,
                }
            }
            let bound = seconds(bound_updated_at);
            // `at` liegt womöglich nicht vor dem Binden.
            let not_before = |at: &str| match (bound, seconds(at)) {
                (Some(bound), Some(at)) => at >= bound,
                _ => true,
            };
            // Vor der ersten Änderung gebunden: Die gebundene Beschreibung
            // kann die ursprüngliche sein, die keine Notiz trägt.
            if edits_at.iter().all(|at| not_before(at)) && !edits_at.is_empty() {
                complete = false;
            }
            // Kurz nach einer Änderung gebunden: Eine weitere Änderung im
            // Fenster kann deren Fassung überschrieben haben.
            let squashable = |at: &str| match (bound, seconds(at)) {
                (Some(bound), Some(at)) => at <= bound && bound - at <= SQUASH_WINDOW_SECS,
                _ => true,
            };
            if edits_at.iter().any(|at| squashable(at)) {
                complete = false;
            }
            // Titel seit dem Binden geändert: Der Titel von damals ist
            // unbekannt — weder „bestätigt" noch „nicht gefunden" ist belegt.
            if title_changes.iter().any(|at| not_before(at)) {
                complete = false;
                confirmed = false;
            }
            if confirmed {
                IssueHistory::Confirmed
            } else if complete {
                IssueHistory::NotFound
            } else {
                IssueHistory::Incomplete
            }
        }
        Ok(History::Unavailable) => IssueHistory::Unavailable,
        // Ein verweigerter oder gescheiterter GraphQL-Zugriff: Dass sich
        // das Issue geändert hat, steht trotzdem fest.
        Err(_) => IssueHistory::QueryFailed,
    };
    IssueVersion::Changed(history)
}

/// Ein RFC-3339-Zeitpunkt in ganzen Sekunden.
fn seconds(at: &str) -> Option<i64> {
    at.parse::<jiff::Timestamp>().ok().map(|at| at.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::stub_server;

    const TOKEN: &str = "glpat-geheim123geheim123";

    fn project(url: &str) -> Project {
        Project::with_token(url, "team%2Fminds", TOKEN.into())
    }

    // Die Fixtures: was GitLab (REST bzw. GraphQL) für Issue 42 liefert.
    const ISSUE: &str = include_str!("../fixtures/issue_42.json");
    const ISSUE_CHANGED: &str = include_str!("../fixtures/issue_42_changed.json");
    const HISTORY: &str = include_str!("../fixtures/graphql_history.json");
    const NO_HISTORY_FIELD: &str = include_str!("../fixtures/graphql_unknown_field.json");
    const HISTORY_UNLICENSED: &str = include_str!("../fixtures/graphql_unlicensed.json");
    const UNAUTHORIZED: &str = include_str!("../fixtures/error_401.json");
    const NOT_FOUND: &str = include_str!("../fixtures/error_404.json");

    /// Die Beschreibung zur Bindungszeit (in `ISSUE` und als frühere
    /// Fassung in `HISTORY`).
    const BOUND_TITLE: &str = "Retry mit exponentiellem Backoff";
    const BOUND_DESCRIPTION: &str = "Max. 5 Versuche, 200 ms Basis.";
    /// `updated_at` der gebundenen Fassung (wie in `ISSUE`).
    const BOUND_AT: &str = "2026-10-01T08:15:00.123Z";
    const ONE_EDIT: &str = include_str!("../fixtures/graphql_one_edit_after_binding.json");
    const DELETED_VERSION: &str = include_str!("../fixtures/graphql_deleted_version.json");
    const TRANSIENT: &str = include_str!("../fixtures/graphql_transient_error.json");

    /// Der Vergleich, wie der Aufrufer ihn rechnet — hier ohne Redaction.
    fn bound(title: &str, description: &str) -> Result<bool, &'static str> {
        Ok(title == BOUND_TITLE && description == BOUND_DESCRIPTION)
    }

    #[test]
    fn the_snapshot_reads_title_description_and_version() {
        let (url, received) = stub_server(vec![(200, ISSUE.into())]);
        let snapshot = project(&url).issue_snapshot(42).unwrap();
        assert_eq!(
            snapshot,
            IssueSnapshot {
                title: BOUND_TITLE.into(),
                description: BOUND_DESCRIPTION.into(),
                updated_at: "2026-10-01T08:15:00.123Z".into(),
                web_url: "https://gitlab.example/team/minds/-/issues/42".into(),
                confidential: false,
            }
        );
        let request = received.recv().unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/v4/projects/team%2Fminds/issues/42");
        assert!(
            request
                .headers
                .iter()
                .any(|h| h == &format!("PRIVATE-TOKEN: {TOKEN}"))
        );
    }

    #[test]
    fn a_null_description_is_empty_and_another_iid_is_refused() {
        let body =
            r#"{"iid":7,"title":"t","description":null,"updated_at":"2026-10-01T08:15:00Z"}"#;
        let (url, _received) = stub_server(vec![(200, body.into()), (200, body.into())]);
        let snapshot = project(&url).issue_snapshot(7).unwrap();
        assert_eq!(snapshot.description, "");
        assert_eq!(snapshot.web_url, "");
        assert_eq!(
            project(&url).issue_snapshot(8),
            Err(GitlabError::Malformed("the response names another issue"))
        );
    }

    /// Fixture „unverändertes Issue": `current`, ohne die Historie zu fragen.
    #[test]
    fn an_unchanged_issue_is_current() {
        let (url, received) = stub_server(vec![(200, ISSUE.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Current
        );
        assert_eq!(received.recv().unwrap().method, "GET");
        assert!(received.recv().is_err(), "keine weitere Anfrage");
    }

    /// Fixture „geändertes Issue" mit Historie: Die gebundene Fassung steht
    /// darin — `changed since binding`, aber bestätigt.
    #[test]
    fn a_changed_issue_is_confirmed_from_the_history() {
        let (url, received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, HISTORY.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Confirmed)
        );
        received.recv().unwrap();
        let graphql = received.recv().unwrap();
        assert_eq!(graphql.method, "POST");
        assert_eq!(graphql.path, "/api/graphql");
        let body: serde_json::Value = serde_json::from_str(&graphql.body).unwrap();
        assert_eq!(body["variables"]["path"], "team/minds");
        assert_eq!(body["variables"]["iid"], "42");
        assert!(
            graphql
                .headers
                .iter()
                .any(|h| h == &format!("PRIVATE-TOKEN: {TOKEN}"))
        );
    }

    /// Dieselbe Historie, aber ein Anker über eine Fassung, die es nie gab.
    #[test]
    fn a_version_that_never_existed_is_not_found() {
        let (url, _received) =
            stub_server(vec![(200, ISSUE_CHANGED.into()), (200, HISTORY.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, |_, d| Ok(
                d == "nie geschrieben"
            )),
            IssueVersion::Changed(IssueHistory::NotFound)
        );
    }

    /// Fixture „API ohne Historie": ein unbekanntes Feld (ältere Instanz),
    /// eine Instanz ohne GraphQL (404) und eine, die die Fassungen nicht
    /// ausliefert — jedes Mal nur `changed since binding`, nie „not found".
    #[test]
    fn an_api_without_history_reports_changed_only() {
        for (status, graphql) in [
            (200, NO_HISTORY_FIELD),
            (404, NOT_FOUND),
            (200, HISTORY_UNLICENSED),
        ] {
            let (url, _received) =
                stub_server(vec![(200, ISSUE_CHANGED.into()), (status, graphql.into())]);
            assert_eq!(
                check_version(&project(&url), 42, BOUND_AT, bound),
                IssueVersion::Changed(IssueHistory::Unavailable),
                "{graphql}"
            );
        }
    }

    /// Fixture 401: nicht geprüft, mit festem Grund — der Token steht weder
    /// im Befund noch im Fehlertext, auch wenn der Server ihn spiegelt.
    #[test]
    fn unauthorized_is_unavailable_and_never_names_the_token() {
        let (url, _received) = stub_server(vec![(401, UNAUTHORIZED.into())]);
        let version = check_version(&project(&url), 42, BOUND_AT, bound);
        assert_eq!(
            version,
            IssueVersion::Unavailable("unauthorized (HTTP 401)")
        );
        assert!(!version.text().contains(TOKEN));

        let echo = format!(r#"{{"message":"401 Unauthorized","echo":"PRIVATE-TOKEN: {TOKEN}"}}"#);
        let (url, _received) = stub_server(vec![(401, echo)]);
        let err = project(&url).issue_snapshot(42).unwrap_err();
        assert_eq!(err, GitlabError::Unauthorized);
        assert!(!err.to_string().contains(TOKEN), "{err}");
    }

    /// Fixture 404.
    #[test]
    fn not_found_is_unavailable() {
        let (url, _received) = stub_server(vec![(404, NOT_FOUND.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Unavailable("issue not found (HTTP 404)")
        );
    }

    #[test]
    fn an_unreachable_gitlab_is_unavailable_without_the_token() {
        // Ein Port, auf dem niemand lauscht.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = project(&url).issue_snapshot(42).unwrap_err();
        assert!(matches!(err, GitlabError::Unreachable(_)), "{err:?}");
        assert!(!err.to_string().contains(TOKEN), "{err}");
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Unavailable("GitLab not reachable")
        );
    }

    #[test]
    fn a_text_the_policy_refuses_is_unavailable() {
        let (url, _received) = stub_server(vec![(200, ISSUE.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, |_, _| {
                Err("issue text refused by the redaction policy")
            }),
            IssueVersion::Unavailable("issue text refused by the redaction policy")
        );
    }

    #[test]
    fn a_long_history_is_paged_and_reported_incomplete() {
        let page = |next: bool| {
            format!(
                r#"{{"data":{{"project":{{"issue":{{"notes":{{"pageInfo":{{"hasNextPage":{next},"endCursor":"c"}},"nodes":[{{"createdAt":"2026-09-29T09:12:44Z","systemNoteMetadata":{{"action":"description","descriptionVersion":{{"description":"alt"}}}}}}]}}}}}}}}}}"#
            )
        };
        let mut responses = vec![(200, ISSUE_CHANGED.to_owned())];
        responses.extend((0..MAX_PAGES).map(|_| (200, page(true))));
        let (url, received) = stub_server(responses);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
        received.recv().unwrap();
        // Ab der zweiten Seite trägt die Anfrage den Cursor.
        received.recv().unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&received.recv().unwrap().body).unwrap();
        assert_eq!(second["variables"]["after"], "c");
    }

    #[test]
    fn a_numeric_project_id_has_no_graphql_path_and_no_history() {
        // GraphQL kennt das Projekt `1234` nicht als Pfad: `project: null`.
        let (url, _received) = stub_server(vec![(200, r#"{"data":{"project":null}}"#.into())]);
        let numeric = Project::with_token(&url, "1234", TOKEN.into());
        assert_eq!(numeric.description_history(42), Ok(History::Unavailable));
    }

    /// Review EA-16: Gebunden wurde die ursprüngliche Beschreibung, danach
    /// eine Änderung. Den Text vor der ersten Änderung listet GitLab nicht —
    /// das ist „nicht im lesbaren Teil", nie „nicht gefunden".
    #[test]
    fn a_version_bound_before_the_first_edit_is_never_not_found() {
        let (url, _received) =
            stub_server(vec![(200, ISSUE_CHANGED.into()), (200, ONE_EDIT.into())]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    /// Review EA-16: Eine gelöschte Fassung unter lesbaren — die gebundene
    /// kann genau diese sein.
    #[test]
    fn a_deleted_version_makes_the_history_incomplete() {
        let (url, _received) = stub_server(vec![
            (200, ISSUE_CHANGED.into()),
            (200, DELETED_VERSION.into()),
        ]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    /// Eine Fassung, die die Policy nicht nachrechnen kann, zählt nicht als
    /// „anders".
    #[test]
    fn an_uncheckable_version_makes_the_history_incomplete() {
        let (url, _received) =
            stub_server(vec![(200, ISSUE_CHANGED.into()), (200, HISTORY.into())]);
        let version = check_version(&project(&url), 42, BOUND_AT, |_, d| {
            if d.starts_with("Max. 5") {
                Err("issue text refused by the redaction policy")
            } else {
                Ok(false)
            }
        });
        assert_eq!(version, IssueVersion::Changed(IssueHistory::Incomplete));
    }

    /// REST antwortet, GraphQL verweigert (401) oder scheitert vorübergehend:
    /// geändert — aber die Historie ist nicht „nicht verfügbar".
    #[test]
    fn a_failed_history_query_is_named_as_such() {
        for (status, body) in [(401, UNAUTHORIZED), (200, TRANSIENT), (500, NOT_FOUND)] {
            let (url, _received) =
                stub_server(vec![(200, ISSUE_CHANGED.into()), (status, body.into())]);
            assert_eq!(
                check_version(&project(&url), 42, BOUND_AT, bound),
                IssueVersion::Changed(IssueHistory::QueryFailed),
                "{status} {body}"
            );
        }
    }

    #[test]
    fn a_confidential_issue_is_flagged() {
        let body = r#"{"iid":42,"title":"t","description":"d","updated_at":"2026-10-01T08:15:00Z","confidential":true}"#;
        let (url, _received) = stub_server(vec![(200, body.into())]);
        assert!(project(&url).issue_snapshot(42).unwrap().confidential);
        // Ohne das Feld: im Zweifel vertraulich.
        let body =
            r#"{"iid":42,"title":"t","description":"d","updated_at":"2026-10-01T08:15:00Z"}"#;
        let (url, _received) = stub_server(vec![(200, body.into())]);
        assert!(project(&url).issue_snapshot(42).unwrap().confidential);
    }

    /// Eine Seite Notizen aus `(createdAt, action, description)`.
    fn notes(items: &[(&str, &str, Option<&str>)]) -> String {
        let nodes: Vec<_> = items
            .iter()
            .map(|(at, action, description)| {
                serde_json::json!({
                    "createdAt": at,
                    "systemNoteMetadata": {
                        "action": action,
                        "descriptionVersion": description.map(|d| serde_json::json!({ "description": d })),
                    },
                })
            })
            .collect();
        serde_json::json!({ "data": { "project": { "issue": { "notes": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": nodes,
        } } } } })
        .to_string()
    }

    /// Review EA-16: Nur der Titel wurde nach dem Binden geändert — das ist
    /// nie „nicht gefunden".
    #[test]
    fn a_title_renamed_after_binding_is_never_not_found() {
        let history = notes(&[("2026-10-02T10:00:00Z", "title", None)]);
        let (url, _received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, history)]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, |_, _| Ok(false)),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    /// Review EA-16: Titel nach dem Binden geändert — eine frühere
    /// Beschreibung mit dem heutigen Titel ist keine Bestätigung.
    #[test]
    fn a_title_renamed_after_binding_never_confirms() {
        let history = notes(&[
            (
                "2026-10-01T08:15:00Z",
                "description",
                Some(BOUND_DESCRIPTION),
            ),
            ("2026-10-02T10:00:00Z", "title", None),
            (
                "2026-10-03T11:40:27Z",
                "description",
                Some("Max. 7 Versuche, 500 ms Basis, mit Jitter."),
            ),
        ]);
        let (url, _received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, history)]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    /// Ein Titel, der **vor** dem Binden geändert wurde, war damals schon
    /// der heutige: Die Historie bleibt aussagekräftig.
    #[test]
    fn a_title_renamed_before_binding_still_confirms() {
        let history = notes(&[
            (
                "2026-09-29T09:12:44Z",
                "description",
                Some("Max. 3 Versuche."),
            ),
            ("2026-09-30T10:00:00Z", "title", None),
            (
                "2026-10-01T08:14:59Z",
                "description",
                Some(BOUND_DESCRIPTION),
            ),
            (
                "2026-10-03T11:40:27Z",
                "description",
                Some("Max. 7 Versuche, 500 ms Basis, mit Jitter."),
            ),
        ]);
        let (url, _received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, history)]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Confirmed)
        );
    }

    /// Review EA-16: GraphQL nennt Sekunden, REST Millisekunden. Die erste
    /// Änderung in derselben Sekunde wie das Binden kann danach liegen —
    /// dann ist die ursprüngliche Beschreibung womöglich die gebundene.
    #[test]
    fn an_edit_in_the_binding_second_counts_as_possibly_later() {
        let history = notes(&[(
            "2026-10-01T08:15:00Z",
            "description",
            Some("Max. 7 Versuche, 500 ms Basis, mit Jitter."),
        )]);
        let (url, _received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, history)]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, bound),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    /// Security-Review EA-16: Gebunden kurz nach einer Änderung — eine
    /// zweite Änderung im Fenster kann die gebundene Fassung überschrieben
    /// haben. Das ist nie „nicht gefunden".
    #[test]
    fn a_version_bound_inside_the_squash_window_is_never_not_found() {
        let history = notes(&[
            (
                "2026-09-29T09:12:44Z",
                "description",
                Some("Max. 3 Versuche."),
            ),
            (
                "2026-10-01T08:10:00Z",
                "description",
                Some("Max. 7 Versuche, 500 ms Basis, mit Jitter."),
            ),
        ]);
        let (url, _received) = stub_server(vec![(200, ISSUE_CHANGED.into()), (200, history)]);
        assert_eq!(
            check_version(&project(&url), 42, BOUND_AT, |_, _| Ok(false)),
            IssueVersion::Changed(IssueHistory::Incomplete)
        );
    }

    #[test]
    fn common_statuses_have_their_own_reason() {
        assert!(GitlabError::from_status(302).reason().contains("3xx"));
        assert!(GitlabError::from_status(429).reason().contains("429"));
        assert!(GitlabError::from_status(503).reason().contains("5xx"));
        assert_eq!(
            GitlabError::from_status(418).reason(),
            "unexpected HTTP status"
        );
    }
}
