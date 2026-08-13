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

use std::io::Write;
use std::process::{Command, Stdio};

use minds_core::{ContentHash, Decision, Review, Subject};

pub mod webhook;

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
         <sub>Gespiegelt aus `refs/minds/reviews` · `{hash}` · \
         Quelle ist das Repository, nicht diese Note.</sub>",
        marker(hash),
        review.decision.as_str(),
        review.reviewer,
    )
}

/// Ein Zugang zur GitLab-API eines Projekts.
#[derive(Debug, Clone)]
pub struct Project {
    /// Basis-URL der Instanz, z. B. `https://gitlab.com`.
    pub base_url: String,
    /// Projekt-Id oder URL-kodierter Pfad (`gruppe%2Fprojekt`).
    pub project: String,
    /// Der Token. Kommt aus einer Umgebungsvariablen, nie aus einem Argument.
    token: String,
}

impl Project {
    /// Baut einen Zugang; der Token kommt aus der Umgebungsvariablen `token_env`.
    ///
    /// # Fehler
    ///
    /// Wenn die Variable fehlt oder leer ist. Das ist der häufigste
    /// Konfigurationsfehler, und er soll benannt werden, statt sich als HTTP 401
    /// zu zeigen.
    pub fn new(base_url: &str, project: &str, token_env: &str) -> Result<Self, String> {
        let token = std::env::var(token_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("Umgebungsvariable {token_env} ist nicht gesetzt"))?;
        Ok(Self::with_token(base_url, project, token))
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
        self.curl(&["--request", "GET"], path, None)
    }

    fn post(&self, path: &str, json: &str) -> Result<String, String> {
        self.curl(
            &[
                "--request",
                "POST",
                "--header",
                "Content-Type: application/json",
            ],
            path,
            Some(json),
        )
    }

    /// Der eine Ort, an dem das Netz angefasst wird.
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
    fn curl(&self, extra: &[&str], path: &str, body: Option<&str>) -> Result<String, String> {
        let url = format!("{}/api/v4{path}", self.base_url);
        let mut command = Command::new("curl");
        command
            .args(["--silent", "--show-error", "--fail-with-body"])
            .args(["--header", "@-"])
            .args(extra);

        let body_file = match body {
            Some(body) => {
                // `NamedTempFile` legt unter Unix mit 0600 an; niemand außer
                // dem Besitzer liest mit, solange die Datei lebt.
                let mut file = tempfile::NamedTempFile::new()
                    .map_err(|err| format!("curl: Body-Datei nicht anlegbar: {err}"))?;
                file.write_all(body.as_bytes())
                    .map_err(|err| format!("curl: Body nicht schreibbar: {err}"))?;
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
            .arg(&url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| format!("curl lässt sich nicht starten: {err}"))?;
        {
            let mut stdin = child.stdin.take().ok_or("curl: kein stdin")?;
            writeln!(stdin, "PRIVATE-TOKEN: {}", self.token)
                .map_err(|err| format!("curl: Header nicht schreibbar: {err}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| format!("curl endet nicht: {err}"))?;
        // Erst wenn curl fertig ist, darf die Body-Datei verschwinden.
        drop(body_file);

        if !output.status.success() {
            // stderr trägt curls Diagnose, stdout dank `--fail-with-body` die
            // Antwort des Servers — dort steht die eigentliche Ursache, etwa
            // GitLabs `{"message": …}`. Der Token kann in beidem nicht stehen,
            // er ging über stdin.
            let mut message = format!(
                "GitLab-Aufruf fehlgeschlagen ({}): {}",
                path,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let response = String::from_utf8_lossy(&output.stdout);
            let response = response.trim();
            if !response.is_empty() {
                message.push_str(" — Antwort: ");
                let mut chars = response.chars();
                message.extend(chars.by_ref().take(500));
                if chars.next().is_some() {
                    message.push('…');
                }
            }
            return Err(message);
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(body.contains("Quelle ist das Repository"));
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

    // --- Stub-Server für den curl-Pfad --------------------------------------
    //
    // Ein echter `curl`-Prozess gegen einen lokalen `TcpListener`: Nur so ist
    // sichtbar, was wirklich über die Leitung geht — die Aufteilung von stdin
    // zwischen Header und Body war genau der Fehler, den ein Mock der eigenen
    // Abstraktion nie gefunden hätte (#7).

    struct Received {
        method: String,
        path: String,
        headers: Vec<String>,
        body: String,
    }

    /// Startet einen Server, der die `responses` der Reihe nach ausliefert und
    /// jeden empfangenen Request in den Kanal legt. Nach der letzten Antwort
    /// endet der Thread; der Kanal meldet dann `Err` — „kein weiterer Request".
    fn stub_server(responses: Vec<(u16, String)>) -> (String, std::sync::mpsc::Receiver<Received>) {
        use std::io::Write as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let request = read_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = sender.send(request);
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Received {
        use std::io::{BufRead, BufReader, Read};

        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let mut headers = Vec::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
            headers.push(line);
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        Received {
            method,
            path,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
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

    #[test]
    fn a_missing_token_is_named_not_deferred_to_a_401() {
        // SAFETY-freie Variante: eine Variable, die es sicher nicht gibt.
        let err = Project::new(
            "https://gitlab.example",
            "1",
            "MINDS_TEST_TOKEN_GIBT_ES_NICHT",
        )
        .unwrap_err();
        assert!(err.contains("MINDS_TEST_TOKEN_GIBT_ES_NICHT"), "{err}");
    }

    #[test]
    fn the_base_url_loses_its_trailing_slash() {
        let project = Project::with_token("https://gitlab.example/", "1", "geheim".into());
        assert_eq!(project.base_url, "https://gitlab.example");
    }
}
