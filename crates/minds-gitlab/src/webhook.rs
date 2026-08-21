//! Die Gegenrichtung: ein MR-Kommentar wird zum Review-Objekt — **opt-in**.
//!
//! # Zustandslos heißt hier: kein Dienst
//!
//! Es gibt keinen Empfänger, den jemand betreiben müsste. Dieses Modul deutet
//! eine GitLab-Webhook-Nutzlast, die auf **stdin** hereinkommt, und gibt ein
//! [`Review`] zurück. Wer einen HTTP-Endpunkt will, stellt einen beliebigen vor
//! das Binary; wer keinen will, kippt gespeicherte Nutzlasten hinein. Wir hosten
//! nichts.
//!
//! # Warum ein Kommando im Text und nicht jeder Kommentar
//!
//! Ein Review-Objekt ist ein Audit-Record. Jeden Kommentar dazu zu machen hieße,
//! Geplauder als Verdict abzulegen. Es zählt deshalb nur, was ausdrücklich eines
//! sein will:
//!
//! ```text
//! /minds approve     Backoff ist jetzt korrekt
//! /minds reject      so nicht
//! /minds needs-work  bitte den Test nachziehen
//! ```
//!
//! # Woran das Verdict hängt
//!
//! An der **Change-Id** — sonst überlebte es den nächsten Force-Push des MR
//! nicht. Sie kommt aus dem Kommentar selbst, wenn sie dort steht (`I` + 40
//! Hex). Sonst nennt der Aufrufer den Commit des MR, und die CLI löst ihn lokal
//! zu seiner Change-Id auf. Findet sich keine, entsteht **kein** Review: Lieber
//! nichts als ein Verdict, das an nichts hängt.
//!
//! # Wem geglaubt wird
//!
//! Der Nutzlast selbst: niemandem. `user.email` ist ein JSON-Feld, keine
//! Identität — wer die Nutzlast schreiben darf, schreibt hinein, was er will.
//! Herkunft beweist GitLab über den `X-Gitlab-Token`-Header, und diese Prüfung
//! gehört zur **Deutung**, nicht zum Transport: Ein Audit-Objekt unter fremdem
//! Namen entsteht beim Parsen, nicht beim Empfangen. Nutzlasten aus dem Netz
//! gehen deshalb durch [`parse_verified`]; das nackte [`parse`] bleibt für
//! Nutzlasten, deren Herkunft der Aufrufer schon anders sichergestellt hat.

use minds_core::{Decision, Review, Subject};
use serde::Deserialize;

/// Das Präfix, an dem ein Kommentar als Verdict gemeint ist.
pub const COMMAND: &str = "/minds";

/// Was aus einer Webhook-Nutzlast herauszuholen war.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// Das Verdict.
    pub decision: Decision,
    /// Die Zusammenfassung — der Rest der Kommandozeile.
    pub summary: String,
    /// Wer kommentiert hat, **laut Payload** (E-Mail, sonst Benutzername).
    ///
    /// Das Feld ist eine Behauptung der Nutzlast, keine geprüfte Identität.
    /// Verlässlich wird es erst durch [`parse_verified`] — und auch dann sagt
    /// es nur „kam über den Hook mit dem richtigen Secret", nicht „diese
    /// Person war es".
    pub author: String,
    /// Die Change-Id, falls sie im Kommentar stand.
    pub change_id: Option<String>,
    /// Der Commit, auf den der MR zeigt — die Rückfallebene für die Change-Id.
    pub commit: Option<String>,
    /// Die Nummer des Merge Requests, für die Rückmeldung.
    pub merge_request: Option<u64>,
}

impl Incoming {
    /// Baut daraus ein Review, sobald das Subjekt feststeht.
    ///
    /// `change_id` gewinnt über alles, was der Aufrufer aufgelöst hat — was im
    /// Kommentar steht, ist ausdrücklich gemeint.
    pub fn into_review(
        self,
        resolved_change_id: Option<&str>,
        at: Option<String>,
    ) -> Option<Review> {
        let subject = self
            .change_id
            .as_deref()
            .or(resolved_change_id)
            .map(|id| Subject::Change(id.to_string()))?;
        Some(Review::new(
            subject,
            self.decision,
            self.author,
            self.summary,
            at,
        ))
    }
}

/// Deutet eine GitLab-Webhook-Nutzlast, nachdem der `X-Gitlab-Token`-Header
/// gegen das konfigurierte Secret geprüft wurde.
///
/// `provided_token` ist, was der vorgeschaltete Empfänger aus dem Header
/// übernommen hat; `None` heißt „der Header fehlte". `expected_token` ist das
/// Secret, das beim Anlegen des Hooks in GitLab hinterlegt wurde. Stimmen sie
/// nicht überein, wird die Nutzlast **gar nicht erst geparst** — es entsteht
/// kein [`Incoming`], egal was drinsteht.
///
/// Der Vergleich läuft in konstanter Zeit über die volle Länge, damit ein
/// Angreifer das Secret nicht byteweise über Antwortzeiten erraten kann.
pub fn parse_verified(
    payload: &[u8],
    provided_token: Option<&str>,
    expected_token: &str,
) -> Option<Incoming> {
    if !token_matches(provided_token, expected_token) {
        return None;
    }
    parse(payload)
}

/// Vergleicht den mitgelieferten Token mit dem erwarteten — in konstanter Zeit.
///
/// Kein `==` auf `&str`: Das vergliche byteweise mit Kurzschluss, und die
/// Antwortzeit verriete, wie viele führende Bytes schon stimmen. Hier wird
/// stattdessen über die **volle** Länge beider Werte ge-XOR-t und jede
/// Differenz nur eingesammelt; die Längendifferenz fließt genauso ein. Der
/// `fold` kennt keinen frühen Ausstieg — kurzschließen kann das nicht.
///
/// Öffentlich, damit ein Aufrufer, der die Prüfung selbst orchestriert (etwa
/// um bei falschem Token laut zu scheitern statt still nichts zu deuten),
/// dasselbe timing-sichere Werkzeug benutzt und nicht doch zu `==` greift.
pub fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    let provided = match provided {
        Some(value) => value.as_bytes(),
        None => return false,
    };
    let expected = expected.as_bytes();
    let difference =
        (0..provided.len().max(expected.len())).fold(provided.len() ^ expected.len(), |acc, i| {
            let a = provided.get(i).copied().unwrap_or(0);
            let b = expected.get(i).copied().unwrap_or(0);
            acc | usize::from(a ^ b)
        });
    difference == 0
}

/// Deutet eine GitLab-Webhook-Nutzlast — **ohne** Herkunftsprüfung.
///
/// `None` heißt „hier ist kein Verdict" und ist der **Normalfall** — die
/// allermeisten Hooks sind etwas anderes oder gewöhnliche Kommentare. Ein
/// Webhook-Empfänger, der bei jedem fremden Ereignis einen Fehler meldete, wäre
/// nach einer Stunde abgeschaltet.
///
/// **Vertrauensannahme:** Diese Funktion glaubt der Nutzlast jedes Feld,
/// insbesondere den Autor. Sie ist nur für Nutzlasten gedacht, deren Herkunft
/// schon feststeht — etwa lokal abgelegte, die jemand bewusst hineinkippt. Was
/// aus dem Netz kommt, gehört durch [`parse_verified`].
pub fn parse(payload: &[u8]) -> Option<Incoming> {
    let hook: NoteHook = serde_json::from_slice(payload).ok()?;
    if hook.object_kind.as_deref() != Some("note") {
        return None;
    }
    let attributes = hook.object_attributes?;
    if attributes.noteable_type.as_deref() != Some("MergeRequest") {
        return None;
    }

    let (decision, summary) = parse_command(attributes.note.as_deref()?)?;
    let user = hook.user.unwrap_or_default();
    let author = user
        .email
        .filter(|value| !value.trim().is_empty())
        .or(user.username)
        .unwrap_or_else(|| "unbekannt".to_string());

    let merge_request = hook.merge_request.as_ref().and_then(|mr| mr.iid);
    let commit = hook
        .merge_request
        .as_ref()
        .and_then(|mr| mr.last_commit.as_ref())
        .and_then(|commit| commit.id.clone());

    Some(Incoming {
        decision,
        change_id: find_change_id(attributes.note.as_deref().unwrap_or_default()),
        summary,
        author,
        commit,
        merge_request,
    })
}

/// Liest `/minds <verdict> <text>` aus einer Kommentarzeile.
fn parse_command(note: &str) -> Option<(Decision, String)> {
    for line in note.lines() {
        // Weiter suchen statt aufgeben: Das Kommando steht oft unter einem Satz
        // Fließtext („Sieht gut aus.\n/minds approve"). Ein Parser, der nur die
        // erste Zeile ansieht, verschluckte genau den häufigsten Fall.
        let Some(rest) = line.trim().strip_prefix(COMMAND) else {
            continue;
        };
        let rest = rest.trim_start();
        let (word, tail) = match rest.split_once(char::is_whitespace) {
            Some((word, tail)) => (word, tail.trim()),
            None => (rest, ""),
        };
        let decision = match word {
            "approve" => Decision::Approve,
            "reject" => Decision::Reject,
            "needs-work" => Decision::NeedsWork,
            // Ein `/minds` mit unbekanntem Wort ist kein Verdict — aber
            // vielleicht steht weiter unten eines.
            _ => continue,
        };
        // Eine mitgeschriebene Change-Id gehört nicht in die Zusammenfassung —
        // auch nicht, wenn sie wie üblich in Backticks steht.
        let summary = tail
            .split_whitespace()
            .filter(|word| !is_change_id(word.trim_matches('`')))
            .collect::<Vec<_>>()
            .join(" ");
        return Some((decision, summary));
    }
    None
}

/// Die erste Change-Id im Text, falls eine dasteht.
fn find_change_id(note: &str) -> Option<String> {
    note.split(|c: char| c.is_whitespace() || c == '`')
        .find(|word| is_change_id(word))
        .map(str::to_owned)
}

/// `I` gefolgt von 40 Hex-Zeichen — die Textform der Change-Id.
fn is_change_id(word: &str) -> bool {
    word.len() == 41 && word.starts_with('I') && word[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

// --- Die Nutzlast, so viel davon wie gebraucht wird --------------------------
//
// Bewusst tolerant: Unbekannte Felder werden ignoriert, alles ist optional.
// GitLab erweitert seine Hooks laufend, und ein Parser, der daran zerbricht,
// wäre eine Zeitbombe (Architektur-Prinzip 4: toleranter Reader).

#[derive(Debug, Deserialize)]
struct NoteHook {
    object_kind: Option<String>,
    user: Option<User>,
    object_attributes: Option<NoteAttributes>,
    merge_request: Option<MergeRequest>,
}

#[derive(Debug, Default, Deserialize)]
struct User {
    username: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NoteAttributes {
    note: Option<String>,
    noteable_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MergeRequest {
    iid: Option<u64>,
    last_commit: Option<LastCommit>,
}

#[derive(Debug, Deserialize)]
struct LastCommit {
    id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(note: &str) -> Vec<u8> {
        serde_json::json!({
            "object_kind": "note",
            "user": { "username": "anna", "email": "anna@example.org" },
            "object_attributes": { "note": note, "noteable_type": "MergeRequest" },
            "merge_request": { "iid": 7, "last_commit": { "id": "deadbeef" } }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_command_becomes_a_verdict() {
        let incoming = parse(&hook("/minds approve Backoff ist jetzt korrekt")).unwrap();
        assert_eq!(incoming.decision, Decision::Approve);
        assert_eq!(incoming.summary, "Backoff ist jetzt korrekt");
        assert_eq!(incoming.author, "anna@example.org");
        assert_eq!(incoming.merge_request, Some(7));
        assert_eq!(incoming.commit.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn all_three_verdicts_are_understood() {
        for (text, expected) in [
            ("/minds approve", Decision::Approve),
            ("/minds reject weil nicht", Decision::Reject),
            ("/minds needs-work bitte nachziehen", Decision::NeedsWork),
        ] {
            assert_eq!(parse(&hook(text)).unwrap().decision, expected, "{text}");
        }
    }

    #[test]
    fn the_command_is_found_below_prose() {
        // Der häufigste echte Fall: erst schreibt jemand etwas, dann kommt das
        // Kommando. Ein Parser, der nur Zeile eins ansieht, verschluckt ihn.
        let incoming = parse(&hook(
            "Habe mir den Backoff angesehen, sieht jetzt richtig aus.\n\n/minds approve danke",
        ))
        .unwrap();
        assert_eq!(incoming.decision, Decision::Approve);
        assert_eq!(incoming.summary, "danke");
    }

    #[test]
    fn an_ordinary_comment_is_not_a_verdict() {
        // Der Normalfall. Er darf nichts erzeugen und nichts melden.
        assert!(parse(&hook("Sieht gut aus, danke!")).is_none());
        assert!(parse(&hook("/minds vielleicht")).is_none());
    }

    #[test]
    fn other_hooks_are_ignored() {
        let push = serde_json::json!({ "object_kind": "push" }).to_string();
        assert!(parse(push.as_bytes()).is_none());

        let issue_note = serde_json::json!({
            "object_kind": "note",
            "object_attributes": { "note": "/minds approve", "noteable_type": "Issue" }
        })
        .to_string();
        assert!(
            parse(issue_note.as_bytes()).is_none(),
            "ein Issue-Kommentar ist kein Review eines Changes"
        );
    }

    #[test]
    fn garbage_is_no_verdict_and_no_panic() {
        assert!(parse(b"kein json").is_none());
        assert!(parse(b"{}").is_none());
        assert!(parse(b"").is_none());
    }

    #[test]
    fn a_change_id_in_the_comment_wins_and_leaves_the_summary() {
        let id = format!("I{}", "ab".repeat(20));
        let incoming = parse(&hook(&format!("/minds approve `{id}` sieht gut aus"))).unwrap();

        assert_eq!(incoming.change_id.as_deref(), Some(id.as_str()));
        // Die Id ist Adresse, nicht Text — sie gehört nicht in die Zusammenfassung.
        assert_eq!(incoming.summary, "sieht gut aus");

        let review = incoming.into_review(Some("Ietwasanderes"), None).unwrap();
        assert_eq!(review.subject.id(), id);
    }

    #[test]
    fn without_any_change_id_no_review_is_built() {
        // Lieber nichts als ein Verdict, das an nichts hängt.
        let incoming = parse(&hook("/minds approve")).unwrap();
        assert!(incoming.clone().into_review(None, None).is_none());
        assert!(incoming.into_review(Some("Iaufgeloest"), None).is_some());
    }

    #[test]
    fn a_wrong_or_missing_token_yields_no_incoming() {
        // Die Nutzlast selbst wäre ein gültiges Verdict — aber ohne bewiesene
        // Herkunft entsteht daraus kein Audit-Objekt (#8).
        let payload = hook("/minds approve Backoff ist jetzt korrekt");
        assert!(parse_verified(&payload, Some("falsch"), "geheim").is_none());
        assert!(parse_verified(&payload, None, "geheim").is_none());
        assert!(
            parse_verified(&payload, Some("geheimX"), "geheim").is_none(),
            "ein Präfix-Treffer ist kein Treffer"
        );
        assert!(
            parse_verified(&payload, Some(""), "geheim").is_none(),
            "ein leerer Header ist kein Token"
        );
        assert!(parse_verified(&payload, Some("geheim"), "geheim").is_some());
    }

    #[test]
    fn the_token_comparison_collects_every_difference() {
        // Kurzschließen hieße: Nach dem ersten Unterschied wird nicht mehr
        // verglichen. Die Funktion faltet stattdessen über die volle Länge —
        // eine Differenz an jeder einzelnen Position, auch der letzten, und
        // ebenso jede Längendifferenz muss sie deshalb gleichermaßen sehen.
        let expected = "streng-geheimes-secret";
        for position in 0..expected.len() {
            let mut provided = expected.as_bytes().to_vec();
            provided[position] ^= 0x01;
            let provided = String::from_utf8(provided).unwrap();
            assert!(
                !token_matches(Some(&provided), expected),
                "Differenz an Position {position} übersehen"
            );
        }
        assert!(!token_matches(
            Some(&expected[..expected.len() - 1]),
            expected
        ));
        assert!(!token_matches(Some(&format!("{expected}x")), expected));
        assert!(token_matches(Some(expected), expected));
    }

    #[test]
    fn the_username_stands_in_when_there_is_no_email() {
        let payload = serde_json::json!({
            "object_kind": "note",
            "user": { "username": "anna" },
            "object_attributes": { "note": "/minds approve", "noteable_type": "MergeRequest" }
        })
        .to_string();
        assert_eq!(parse(payload.as_bytes()).unwrap().author, "anna");
    }
}
