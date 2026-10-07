//! `minds-gitlab` — die Plattform wird zum **Cache** (Schicht 3, R4).
//!
//! Die Quelle der Wahrheit ist das Repo: Ein Verdict liegt content-adressiert
//! und signierbar unter `refs/minds/reviews`. Nur sitzen viele Teams den ganzen
//! Tag in der GitLab-Oberfläche und sollen dort sehen, was im Repo steht. Also
//! **spiegeln** wir es dorthin.
//!
//! # Einweg, und zwar mit Absicht
//!
//! Verdict → MR-Note ist die Richtung, die nichts kaputtmachen kann. Ginge es in
//! beide Richtungen automatisch, hätte man zwei Quellen und müsste entscheiden,
//! welche gewinnt — genau der Zustand, den dieses Projekt vermeiden will.
//!
//! Die Gegenrichtung gibt es trotzdem, aber **opt-in und ohne Automatik**:
//! [`webhook`] deutet einen MR-Kommentar als Verdict und gibt ein
//! [`Review`] zurück. Wer das einschaltet, entscheidet sich bewusst dafür, dass
//! ein Kommentar in der Oberfläche ein Objekt im Repo erzeugt — und das Objekt
//! ist danach die Wahrheit, nicht der Kommentar.
//!
//! # Idempotent über einen Marker
//!
//! Jede gespiegelte Note trägt `<!-- minds:review:<hash> -->`. Vor dem Schreiben
//! wird gelesen: Steht der Marker schon da, passiert nichts. Weil der Hash das
//! Verdict content-adressiert, heißt „derselbe Marker" auch „derselbe Inhalt" —
//! ein wiederholter Lauf kann also weder doppeln noch etwas Falsches
//! überschreiben. Das ist die Eigenschaft, die eine Spiegelung braucht, die in
//! einer CI bei jedem Push läuft.
//!
//! # Lesen: Issue-Fassungen für Intent-Anker (EA-16)
//!
//! Die einzige Richtung **herein** ohne Opt-in ist lesend und erzeugt selbst
//! nichts: [`issue`] holt Titel und Beschreibung eines Issues für
//! `minds intent bind --issue` und prüft für `minds verify --online`, ob eine
//! gebundene Fassung (noch) existiert. Der Anker entsteht erst in der CLI,
//! über die Redaction.
//!
//! # Warum `curl` und kein HTTP-Stack
//!
//! Dieselbe Linie wie beim Signieren, das `ssh-keygen` aufruft: Die eine harte
//! Abhängigkeit ist ohnehin da, und ein HTTP-Client zöge hundert Kisten in einen
//! Build, der heute mit `serde` und `gix` auskommt. `curl` liegt in jedem
//! CI-Image, in dem auch `git` liegt. Vorausgesetzt wird curl ≥ 7.76 (2021,
//! wegen `--fail-with-body`).
//!
//! # Der Token kommt nie über die Kommandozeile
//!
//! Nur über eine Umgebungsvariable. Ein Argument steht in `ps` und in der
//! Shell-History; eine Variable nicht. Sie wird an `curl` über eine
//! `--header @-`-Eingabe auf stdin gereicht, damit sie auch nicht in dessen
//! Argumentliste auftaucht.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use minds_core::{ContentHash, Decision, Review, Subject};

pub mod issue;
#[cfg(test)]
mod stub;
pub mod webhook;

pub use issue::{GitlabError, History, IssueSnapshot, check_version};

/// Der HTML-Kommentar, an dem eine gespiegelte Note wiedererkannt wird.
///
/// HTML-Kommentar, weil GitLab ihn rendert, aber nicht anzeigt: Der Mensch sieht
/// das Verdict, das Werkzeug sieht den Marker.
pub fn marker(hash: &ContentHash) -> String {
    format!("<!-- minds:review:{hash} -->")
}

/// Der Text der MR-Note zu einem Verdict.
///
/// Bewusst kurz und ohne Deutung: Was hier steht, steht so auch im Repo. Der
/// Link zurück ist der Hash — wer ihn hat, kann das Verdict offline verifizieren.
pub fn note_body(hash: &ContentHash, review: &Review) -> String {
    let symbol = match review.decision {
        Decision::Approve => "✅",
        Decision::Reject => "❌",
        Decision::NeedsWork => "🔁",
    };
    let subject = match &review.subject {
        Subject::Change(id) => format!("Change `{id}`"),
        Subject::Session(id) => format!("Session `{id}`"),
    };
    let summary = if review.summary.is_empty() {
        String::new()
    } else {
        format!("\n\n> {}", review.summary.replace('\n', "\n> "))
    };

    format!(
        "{}\n\n\
         {symbol} **{}** — {}\n\n\
         {subject}{summary}\n\n\
         <sub>Mirrored from `refs/minds/reviews` · `{hash}` · \
         The repository is the source of truth, not this note.</sub>",
        marker(hash),
        review.decision.as_str(),
        review.reviewer,
    )
}

/// Ein Zugang zur GitLab-API eines Projekts.
///
/// `Debug` ist von Hand geschrieben: Ein `{:?}` (auch in einem `unwrap`)
/// zeigt den Token nie.
#[derive(Clone)]
pub struct Project {
    /// Basis-URL der Instanz, z. B. `https://gitlab.com`.
    pub base_url: String,
    /// Projekt-Id oder URL-kodierter Pfad (`gruppe%2Fprojekt`).
    pub project: String,
    /// Der Token. Kommt aus einer Umgebungsvariablen, nie aus einem Argument.
    token: String,
    /// Der Name dieser Variablen — curl bekommt sie nicht mit: Der Token
    /// geht nur über stdin.
    token_env: String,
}

impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("base_url", &self.base_url)
            .field("project", &self.project)
            .field("token", &"[token]")
            .finish()
    }
}

impl Project {
    /// Baut einen Zugang; der Token kommt aus der Umgebungsvariablen `token_env`.
    ///
    /// # Fehler
    ///
    /// Wenn die Variable fehlt oder leer ist. Das ist der häufigste
    /// Konfigurationsfehler, und er soll benannt werden, statt sich als HTTP 401
    /// zu zeigen.
    pub fn new(base_url: &str, project: &str, token_env: &str) -> Result<Self, TokenError> {
        let token = read_token(std::env::var(token_env).ok(), token_env)?;
        let mut project = Self::with_token(base_url, project, token);
        project.token_env = token_env.to_owned();
        Ok(project)
    }

    /// Wie [`new`](Self::new), aber mit schon gelesenem Token.
    ///
    /// Bewusst **nicht** öffentlich: Ein Token, den man als Argument reichen
    /// kann, landet irgendwann in einem `ps`-Listing oder einer Shell-History.
    /// Von außen führt der Weg nur über die Umgebungsvariable.
    fn with_token(base_url: &str, project: &str, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            project: project.to_string(),
            token,
            token_env: String::new(),
        }
    }

    /// Spiegelt ein Verdict als Note an den Merge Request `mr` — **idempotent**.
    ///
    /// Gibt `false` zurück, wenn die Note schon da war (dann wurde nichts
    /// geschickt).
    pub fn mirror(&self, mr: u64, hash: &ContentHash, review: &Review) -> Result<bool, String> {
        if self.has_note(mr, hash)? {
            return Ok(false);
        }
        let body = note_body(hash, review);
        let payload = serde_json::json!({ "body": body });
        self.post(
            &format!("/projects/{}/merge_requests/{mr}/notes", self.project),
            &payload.to_string(),
        )?;
        Ok(true)
    }

    /// Setzt zusätzlich das GitLab-Approval — nur bei `approve`.
    ///
    /// Getrennt von [`mirror`](Self::mirror), weil es etwas anderes ist: Die Note
    /// ist eine **Wiedergabe**, das Approval ein **Eingriff** in den Zustand des
    /// MR. Wer das eine will, will nicht zwingend das andere.
    pub fn approve(&self, mr: u64) -> Result<(), String> {
        self.post(
            &format!("/projects/{}/merge_requests/{mr}/approve", self.project),
            "{}",
        )
        .map(|_| ())
    }

    /// Ob die Note zu diesem Verdict schon am MR hängt.
    fn has_note(&self, mr: u64, hash: &ContentHash) -> Result<bool, String> {
        let body = self.get(&format!(
            "/projects/{}/merge_requests/{mr}/notes?per_page=100",
            self.project
        ))?;
        Ok(body.contains(&marker(hash)))
    }

    fn get(&self, path: &str) -> Result<String, String> {
        self.curl(GET, path, None)
    }

    fn post(&self, path: &str, json: &str) -> Result<String, String> {
        self.curl(POST_JSON, path, Some(json))
    }

    /// Ein Aufruf der REST-API (`/api/v4…`), der nur Erfolg kennt: Jeder
    /// Status außerhalb von 2xx wird ein Fehler, der GitLabs eigene Ursache
    /// (`{"message": …}`) zitiert.
    fn curl(&self, extra: &[&str], path: &str, body: Option<&str>) -> Result<String, String> {
        let response = self.exchange(extra, &self.rest_url(path), body)?;
        if !(200..300).contains(&response.status) {
            let mut message = format!("GitLab call failed ({path}): HTTP {}", response.status);
            // Erst den Token entfernen, dann kürzen: Läge er über der
            // Schnittkante, bliebe sonst sein Anfang stehen.
            let scrubbed = self.scrub(&response.body);
            let text = scrubbed.trim();
            if !text.is_empty() {
                message.push_str(" — response: ");
                let mut chars = text.chars();
                message.extend(chars.by_ref().take(500));
                if chars.next().is_some() {
                    message.push('…');
                }
            }
            return Err(message);
        }
        Ok(response.body)
    }

    /// `<basis>/api/v4<pfad>`.
    fn rest_url(&self, path: &str) -> String {
        format!("{}/api/v4{path}", self.base_url)
    }

    /// Entfernt den Token aus einem Text, der nach außen geht (Fehler,
    /// Diagnosen). Der Token geht zwar nur über stdin hinaus — aber ein
    /// Server oder Proxy, der Request-Header in seiner Antwort spiegelt,
    /// brächte ihn über das Zitat der Antwort zurück.
    fn scrub(&self, text: &str) -> String {
        if self.token.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.token, "[token]")
    }

    /// Der eine Ort, an dem das Netz angefasst wird. Gibt Status und Body
    /// zurück; ein `Err` heißt: keine HTTP-Antwort (Verbindung, Zeitlimit,
    /// Größenlimit) — sein Text ist schon vom Token befreit.
    ///
    /// Der Token geht über stdin an `--header @-`, damit er nicht in der
    /// Argumentliste des Prozesses steht. `curl` liest `@-` dabei bis EOF —
    /// stdin gehört damit vollständig dem Header und kann nicht zusätzlich
    /// den Body tragen (genau das war #7: beide `@-` teilten sich stdin, der
    /// Body kam als Header an und der POST blieb leer). Der Body geht deshalb
    /// über `--data-binary @<datei>` aus einer Tempdatei, die nur für den
    /// Besitzer lesbar ist und mit dem Ende des Aufrufs verschwindet. Der
    /// Token bleibt stdin — er soll weder auf die Platte noch in die
    /// Argumentliste.
    ///
    /// Den Status hängt `--write-out` als letzte Zeile an stdout an: Was
    /// danach kommt, kann nur curl geschrieben haben, nie der Server.
    /// Umleitungen folgt curl ohne `--location` nicht — der Token geht nie
    /// an einen anderen Host als den genannten.
    ///
    /// `-q` steht **zuerst**: curl liest dann keine `.curlrc` (`$CURL_HOME`,
    /// `$XDG_CONFIG_HOME`, `$HOME`). Die kann ein Agent unter demselben
    /// Nutzer schreiben — `trace-ascii` schriebe den Token in eine Datei,
    /// `location` schickte ihn einem Umleitungsziel hinterher, `proxy` mit
    /// `insecure` ließe einen fremden Server ein „passendes" Issue liefern.
    /// `--proto` nennt nur das Schema der Basis-URL, `--proto-redir -all`
    /// schließt jede Umleitung aus, auch wenn eine Option sie doch erlaubte.
    /// `SSLKEYLOGFILE` fällt weg: Damit ließe sich TLS mitlesen.
    ///
    /// Die übrige Umgebung (Proxy-, CA-Variablen) ist die des Menschen und
    /// gilt als vertrauenswürdig — außer für Loopback: Dorthin geht nie ein
    /// Proxy, sonst reiste der Token im Klartext zu ihm.
    ///
    /// stdout wird mit fester Obergrenze gelesen ([`MAX_RESPONSE_BYTES`]):
    /// `--max-filesize` bricht eine Übertragung ohne `Content-Length` erst
    /// ab curl 8.4 ab.
    fn exchange(&self, extra: &[&str], url: &str, body: Option<&str>) -> Result<Response, String> {
        let proto = if url.starts_with("https://") {
            "=https"
        } else {
            "=http"
        };
        let mut command = Command::new("curl");
        command
            .arg("-q")
            .env_remove("SSLKEYLOGFILE")
            .env_remove("CURL_HOME")
            .env_remove(if self.token_env.is_empty() {
                "MINDS_GITLAB_TOKEN"
            } else {
                &self.token_env
            })
            .args(["--proto", proto, "--proto-redir", "-all"])
            .args(["--silent", "--show-error"])
            .args(["--connect-timeout", CONNECT_TIMEOUT])
            .args(["--max-time", MAX_TIME])
            .args(["--max-filesize", MAX_RESPONSE])
            .args(["--write-out", STATUS_TRAILER])
            .args(["--header", "@-"])
            .args(extra);
        if is_loopback_http(url) {
            for proxy in [
                "http_proxy",
                "HTTP_PROXY",
                "https_proxy",
                "HTTPS_PROXY",
                "all_proxy",
                "ALL_PROXY",
            ] {
                command.env_remove(proxy);
            }
            command.arg("--noproxy").arg("*");
        }

        let body_file = match body {
            Some(body) => {
                // `NamedTempFile` legt unter Unix mit 0600 an; niemand außer
                // dem Besitzer liest mit, solange die Datei lebt.
                let mut file = tempfile::NamedTempFile::new()
                    .map_err(|err| format!("curl: cannot create body file: {err}"))?;
                file.write_all(body.as_bytes())
                    .map_err(|err| format!("curl: cannot write body: {err}"))?;
                // Als `OsString`, nicht über `format!`: Ein Nicht-UTF-8-TMPDIR
                // würde lossy konvertiert auf eine Datei zeigen, die es nicht
                // gibt — und der Body käme wieder leer an.
                let mut at_path = std::ffi::OsString::from("@");
                at_path.push(file.path());
                command.arg("--data-binary").arg(at_path);
                Some(file)
            }
            None => None,
        };

        command
            .arg(url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| format!("curl cannot be started: {err}"))?;
        {
            let Some(mut stdin) = child.stdin.take() else {
                reap(&mut child);
                return Err("curl: no stdin".into());
            };
            writeln!(stdin, "PRIVATE-TOKEN: {}", self.token)
                .map_err(|err| format!("curl: cannot write header: {err}"))?;
        }
        // stderr nebenher, damit eine volle Pipe curl nicht anhält.
        let Some(mut stderr) = child.stderr.take() else {
            reap(&mut child);
            return Err("curl: no stderr".into());
        };
        let diagnostics = std::thread::spawn(move || {
            let mut text = Vec::new();
            let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut text);
            // Den Rest verwerfen, damit curl nie an einer vollen Pipe hängt.
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            text
        });
        let mut stdout = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or("curl: no stdout")?
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut stdout);
        let too_large = stdout.len() as u64 > MAX_RESPONSE_BYTES;
        if too_large || read.is_err() {
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|err| format!("curl did not finish: {err}"))?;
        let stderr = diagnostics.join().unwrap_or_default();
        // Erst wenn curl fertig ist, darf die Body-Datei verschwinden.
        drop(body_file);
        if too_large {
            return Err(format!(
                "GitLab not reachable at {url}: response larger than {MAX_RESPONSE_BYTES} bytes"
            ));
        }

        let stdout = String::from_utf8_lossy(&stdout);
        let parsed = stdout
            .rsplit_once(STATUS_MARKER)
            .and_then(|(body, status)| Some((body, status.trim().parse::<u16>().ok()?)));
        match parsed {
            // `000`: curl bekam keine Antwort.
            Some((body, code)) if status.success() && code != 0 => Ok(Response {
                status: code,
                body: body.to_owned(),
            }),
            _ => Err(self.scrub(&format!(
                "GitLab not reachable at {url}: {}",
                String::from_utf8_lossy(&stderr).trim()
            ))),
        }
    }
}

/// Der Token aus dem Wert der Variablen `token_env`.
///
/// Ein abschließender Zeilenumbruch (CI-Datei-Variablen, `$(cat …)`) gehört
/// nicht zum Token — tolerant gelesen. Der Token wird eine Header-Zeile: Ein
/// Zeilenumbruch **darin** schöbe weitere Header ein, Leerraum wäre ohnehin
/// kein Token.
fn read_token(raw: Option<String>, token_env: &str) -> Result<String, TokenError> {
    let token = raw
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| TokenError::Missing(token_env.to_owned()))?;
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(TokenError::Malformed(token_env.to_owned()));
    }
    Ok(token)
}

/// Warum aus der Umgebungsvariablen kein Token wurde. Nennt die Variable,
/// nie ihren Wert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// Die Variable fehlt oder ist leer.
    Missing(String),
    /// Der Wert enthält Leer- oder Steuerzeichen.
    Malformed(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(env) => write!(f, "environment variable {env} is not set"),
            Self::Malformed(env) => write!(
                f,
                "environment variable {env} contains whitespace or control characters"
            ),
        }
    }
}

impl std::error::Error for TokenError {}

/// Ob `url` Klartext-HTTP an ein Loopback-Literal ist (`127.0.0.1`,
/// `[::1]`) — über den Host, nicht über ein Präfix: `127.0.0.1.example`
/// ist ein fremder Host.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or("", |(host, _)| host),
        None => authority.split(':').next().unwrap_or_default(),
    };
    matches!(host, "127.0.0.1" | "::1")
}

/// Beendet einen curl-Prozess, der nicht zu Ende laufen soll, und wartet
/// auf ihn — kein Zombie.
fn reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Die Antwort eines Aufrufs: Status und Body.
#[derive(Debug)]
struct Response {
    status: u16,
    body: String,
}

/// `curl`-Argumente eines GET.
const GET: &[&str] = &["--request", "GET"];

/// `curl`-Argumente eines POST mit JSON-Body.
const POST_JSON: &[&str] = &[
    "--request",
    "POST",
    "--header",
    "Content-Type: application/json",
];

/// Frist für den Verbindungsaufbau (Sekunden).
const CONNECT_TIMEOUT: &str = "10";

/// Frist für einen ganzen Aufruf (Sekunden): Ein hängender Server hält
/// weder eine CI-Spiegelung noch `minds verify --online` unbegrenzt auf.
const MAX_TIME: &str = "60";

/// Höchstgröße einer Antwort (Bytes) — ein Issue-Snapshot darf ohnehin
/// höchstens [`minds_core::intent_anchor::MAX_SNAPSHOT`] groß sein; eine
/// Seite Notes mit Beschreibungs-Fassungen kann ein Vielfaches davon tragen.
const MAX_RESPONSE: &str = "67108864";

/// Dieselbe Grenze als Zahl — so viel liest [`Project::exchange`] höchstens
/// (plus der Status-Zeile, die curl anhängt).
const MAX_RESPONSE_BYTES: u64 = 67_108_864 + 64;

/// Steht vor dem HTTP-Status, den `--write-out` an stdout anhängt.
const STATUS_MARKER: &str = "\nminds-http-status:";

/// Das Format für `--write-out`.
const STATUS_TRAILER: &str = "\nminds-http-status:%{http_code}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::stub_server;
    use minds_core::Review;

    fn review(decision: Decision, summary: &str) -> Review {
        Review::new(
            Subject::Change(format!("I{}", "ab".repeat(20))),
            decision,
            "anna@example.org",
            summary,
            Some("2026-07-28T10:00:00Z".into()),
        )
    }

    #[test]
    fn the_note_carries_the_marker_verdict_and_source() {
        let verdict = review(Decision::Approve, "Backoff ist jetzt korrekt");
        let hash = verdict.content_hash().unwrap();
        let body = note_body(&hash, &verdict);

        assert!(body.contains(&marker(&hash)));
        assert!(body.contains("approve"));
        assert!(body.contains("anna@example.org"));
        assert!(body.contains("Backoff ist jetzt korrekt"));
        // Die Note sagt selbst, dass sie nicht die Quelle ist.
        assert!(body.contains("The repository is the source of truth"));
    }

    #[test]
    fn the_marker_is_bound_to_the_content() {
        // Zwei verschiedene Verdicts dürfen nie denselben Marker bekommen —
        // sonst hielte die Idempotenz das eine für das andere.
        let approved = review(Decision::Approve, "gut");
        let rejected = review(Decision::Reject, "gut");
        assert_ne!(
            marker(&approved.content_hash().unwrap()),
            marker(&rejected.content_hash().unwrap())
        );
    }

    #[test]
    fn a_multiline_summary_stays_a_quote() {
        let verdict = review(Decision::NeedsWork, "erste Zeile\nzweite Zeile");
        let body = note_body(&verdict.content_hash().unwrap(), &verdict);
        assert!(body.contains("> erste Zeile\n> zweite Zeile"), "{body}");
    }

    #[test]
    fn an_empty_summary_leaves_no_dangling_quote() {
        let verdict = review(Decision::Approve, "");
        let body = note_body(&verdict.content_hash().unwrap(), &verdict);
        assert!(!body.contains("> \n"), "{body}");
    }

    #[test]
    fn the_note_travels_as_body_and_the_token_as_header() {
        let verdict = review(Decision::Approve, "Backoff ist jetzt korrekt");
        let hash = verdict.content_hash().unwrap();
        let (url, received) = stub_server(vec![
            (200, "[]".into()),          // has_note: noch keine Notes am MR
            (201, r#"{"id":1}"#.into()), // die angelegte Note
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());

        let created = project.mirror(4, &hash, &verdict).unwrap();
        assert!(created);

        let get = received.recv().unwrap();
        assert_eq!(get.method, "GET");
        assert!(
            get.headers.iter().any(|h| h == "PRIVATE-TOKEN: geheim123"),
            "{:?}",
            get.headers
        );

        let post = received.recv().unwrap();
        assert_eq!(post.method, "POST");
        assert!(post.path.ends_with("/projects/1/merge_requests/4/notes"));
        assert!(
            post.headers.iter().any(|h| h == "PRIVATE-TOKEN: geheim123"),
            "{:?}",
            post.headers
        );
        // Der Kern von #7: Die Note steht im Body — als JSON, das GitLab
        // versteht — und taucht in keinem Header auf.
        let payload: serde_json::Value = serde_json::from_str(&post.body)
            .unwrap_or_else(|err| panic!("POST-Body ist kein JSON ({err}): {:?}", post.body));
        let note = payload["body"].as_str().unwrap();
        assert!(note.contains(&marker(&hash)));
        assert!(
            !post.headers.iter().any(|h| h.contains("minds:review")),
            "Note im Header statt im Body: {:?}",
            post.headers
        );
    }

    #[test]
    fn an_existing_marker_prevents_the_post() {
        let verdict = review(Decision::Approve, "gut");
        let hash = verdict.content_hash().unwrap();
        let existing = format!(r#"[{{"body":"{} schon gespiegelt"}}]"#, marker(&hash));
        let (url, received) = stub_server(vec![(200, existing)]);
        let project = Project::with_token(&url, "1", "geheim123".into());

        let created = project.mirror(4, &hash, &verdict).unwrap();
        assert!(!created);

        assert_eq!(received.recv().unwrap().method, "GET");
        // Der Server hat genau eine Antwort — käme ein POST, wäre er hier
        // noch erreichbar. Der geschlossene Kanal heißt: kein weiterer Request.
        assert!(received.recv().is_err());
    }

    #[test]
    fn a_gitlab_error_names_the_cause_from_the_response_body() {
        let verdict = review(Decision::Approve, "gut");
        let hash = verdict.content_hash().unwrap();
        let (url, _received) =
            stub_server(vec![(404, r#"{"message":"404 Project Not Found"}"#.into())]);
        let project = Project::with_token(&url, "kein%2Fprojekt", "geheim123".into());

        let err = project.mirror(4, &hash, &verdict).unwrap_err();
        // `--fail-with-body` legt die Server-Antwort auf stdout — die eigentliche
        // Ursache steht dort, nicht in curls stderr.
        assert!(err.contains("404 Project Not Found"), "{err}");
    }

    #[test]
    fn the_error_quotes_the_response_but_never_the_token() {
        let verdict = review(Decision::Approve, "gut");
        let hash = verdict.content_hash().unwrap();
        // Ein Body, der das Wort „PRIVATE-TOKEN" führt — der echte Wert darf
        // trotzdem nie in einem Err-String auftauchen: Er ging über stdin und
        // steht weder in URL noch argv noch Body.
        let (url, _received) = stub_server(vec![(
            401,
            r#"{"message":"401 Unauthorized (PRIVATE-TOKEN invalid)"}"#.into(),
        )]);
        let project = Project::with_token(&url, "1", "geheim123".into());

        let err = project.mirror(4, &hash, &verdict).unwrap_err();
        assert!(err.contains("401 Unauthorized"), "{err}");
        assert!(!err.contains("geheim123"), "{err}");
    }

    /// EA-16: Spiegelt ein Server (oder ein Proxy davor) die Request-Header
    /// in seiner Fehlerantwort, käme der Token über das Zitat zurück — er
    /// wird vorher entfernt. Der Rest der Ursache bleibt lesbar.
    #[test]
    fn a_reflected_token_is_scrubbed_from_the_error() {
        let verdict = review(Decision::Approve, "gut");
        let hash = verdict.content_hash().unwrap();
        let (url, _received) = stub_server(vec![(
            403,
            r#"{"message":"403 Forbidden","request_headers":{"PRIVATE-TOKEN":"geheim123"}}"#.into(),
        )]);
        let project = Project::with_token(&url, "1", "geheim123".into());

        let err = project.mirror(4, &hash, &verdict).unwrap_err();
        assert!(err.contains("403 Forbidden"), "{err}");
        assert!(err.contains("[token]"), "{err}");
        assert!(!err.contains("geheim123"), "{err}");
    }

    /// Keine Antwort (niemand lauscht): Auch curls Diagnose nennt den Token
    /// nicht.
    #[test]
    fn an_unreachable_instance_is_named_without_the_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let verdict = review(Decision::Approve, "gut");
        let project = Project::with_token(&url, "1", "geheim123".into());
        let err = project
            .mirror(4, &verdict.content_hash().unwrap(), &verdict)
            .unwrap_err();
        assert!(err.contains("not reachable"), "{err}");
        assert!(!err.contains("geheim123"), "{err}");
    }

    /// Security-Review EA-16: Ein gespiegelter Token, der über der
    /// 500-Zeichen-Kante des Zitats liegt, hinterlässt keinen Anfang.
    #[test]
    fn a_token_straddling_the_quote_limit_leaves_no_prefix() {
        let verdict = review(Decision::Approve, "gut");
        let token = "glpat-AbCdEfGhIjKlMnOpQrSt";
        let body = format!("{}{token}", "x".repeat(490));
        let (url, _received) = stub_server(vec![(500, body)]);
        let project = Project::with_token(&url, "1", token.into());
        let err = project
            .mirror(4, &verdict.content_hash().unwrap(), &verdict)
            .unwrap_err();
        assert!(!err.contains(&token[..12]), "{err}");
    }

    #[test]
    fn loopback_is_judged_by_the_host() {
        for yes in [
            "http://127.0.0.1:8080/x",
            "http://[::1]:9000",
            "http://127.0.0.1",
        ] {
            assert!(is_loopback_http(yes), "{yes}");
        }
        for no in [
            "http://127.0.0.1.evil.example/",
            "https://127.0.0.1",
            "http://[::1",
            "http://localhost",
        ] {
            assert!(!is_loopback_http(no), "{no}");
        }
    }

    #[test]
    fn debug_never_shows_the_token() {
        let project = Project::with_token("https://gitlab.example", "1", "geheim123".into());
        let shown = format!("{project:?}");
        assert!(!shown.contains("geheim123"), "{shown}");
        assert!(shown.contains("[token]"), "{shown}");
    }

    /// Ein abschließender Zeilenumbruch ist kein Teil des Tokens; einer
    /// mitten darin (Header-Injektion) und Leerraum sind ein Fehler.
    #[test]
    fn the_token_is_read_tolerantly_but_never_injects_headers() {
        let read = |raw: &str| read_token(Some(raw.to_owned()), "T");
        assert_eq!(read("glpat-abc\n").unwrap(), "glpat-abc");
        assert_eq!(read("glpat-abc\r\n").unwrap(), "glpat-abc");
        for bad in ["glpat-abc\nX-Evil: 1", "glpat abc", "glpat-\tabc"] {
            assert_eq!(read(bad), Err(TokenError::Malformed("T".into())), "{bad:?}");
        }
        for empty in ["", "  ", "\n"] {
            assert_eq!(
                read(empty),
                Err(TokenError::Missing("T".into())),
                "{empty:?}"
            );
        }
        assert_eq!(read_token(None, "T"), Err(TokenError::Missing("T".into())));
        assert_eq!(
            TokenError::Malformed("T".into()).to_string(),
            "environment variable T contains whitespace or control characters"
        );
    }

    #[test]
    fn a_missing_token_is_named_not_deferred_to_a_401() {
        // SAFETY-freie Variante: eine Variable, die es sicher nicht gibt.
        let err = Project::new(
            "https://gitlab.example",
            "1",
            "MINDS_TEST_TOKEN_GIBT_ES_NICHT",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("MINDS_TEST_TOKEN_GIBT_ES_NICHT"),
            "{err}"
        );
    }

    #[test]
    fn the_base_url_loses_its_trailing_slash() {
        let project = Project::with_token("https://gitlab.example/", "1", "geheim".into());
        assert_eq!(project.base_url, "https://gitlab.example");
    }
}
