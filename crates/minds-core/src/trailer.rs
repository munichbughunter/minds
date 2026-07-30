//! Minds-Trailer: die Verlinkung zwischen einem Production-Commit und den
//! Sessions, die ihn erzeugt haben.
//!
//! Ein Trailer ist eine `Key: Value`-Zeile am Ende einer Commit-Message (wie
//! `Signed-off-by:`). Minds hängt an jeden agent-erzeugten Commit einen
//!
//! ```text
//! Minds-Session-Id: b3-<64 Hex>
//! ```
//!
//! Warum die Verlinkung in der Commit-Message steht und nicht am Commit-Hash:
//! Die [`SessionId`] ist content-adressiert (siehe [`crate::id`]); der Trailer
//! trägt sie in den *Text* der Message. Damit überlebt die Verlinkung `rebase`,
//! `squash` und `cherry-pick` — Operationen, die den Commit-Hash ändern, die
//! Message aber mitnehmen (Architektur-Prinzip 1 im Plan).
//!
//! **Mehrere Sessions pro Commit** ⇒ mehrere Trailer-Zeilen. Beim `squash`
//! mehrerer Commits konkateniert Git die Messages; die Trailer der
//! Einzel-Commits sammeln sich dann über *mehrere* Absätze der neuen Message.
//! [`Trailer::extract_all`] scannt deshalb bewusst die **ganze** Message und
//! nicht nur Gits letzten Absatz — sonst gingen beim Squash alle bis auf den
//! letzten Trailer verloren. Die strikte Wertgrammatik (`b3-` + genau 64 Hex)
//! macht das ganzflächige Scannen praktisch fehlalarmfrei: Prosa trifft dieses
//! Muster nicht zufällig.
//!
//! **Lesen tolerant, Schreiben kanonisch** — wie bei der [`SessionId`]:
//! [`Trailer::from_str`] akzeptiert Groß-/Kleinschreibung im Schlüssel,
//! umgebende Leerzeichen und Großbuchstaben im Hex-Teil; [`Display`](fmt::Display)
//! gibt ausschließlich die kanonische Form aus (Schlüssel exakt, Hex klein). So
//! bleibt ein von Hand oder von einer Fremdimplementierung geschriebener Trailer
//! auflösbar, während unsere eigene Ausgabe bit-stabil ist.
//!
//! # Die Schreibseite: [`Trailer::append_all`]
//!
//! Die Gegenrichtung zu [`Trailer::extract_all`] — aus einer Message plus
//! Trailern wird eine neue Message. Drei Zusagen:
//!
//! - **Absatzregel.** Trailer gehören in den letzten Absatz. Besteht der
//!   bereits nur aus Trailer-Zeilen (etwa `Signed-off-by:`), wird angehängt;
//!   sonst beginnt ein neuer Absatz. Die **erste Zeile ist immer der Betreff**,
//!   auch wenn sie wie ein Trailer aussieht (`fix: etwas` erfüllt die
//!   Grammatik) — ohne diese Ausnahme klebte der Trailer am Betreff.
//! - **Idempotenz.** Was schon dasteht, kommt nicht noch einmal hinein. Ein
//!   zweiter Lauf desselben Hooks ändert nichts; das trägt später den
//!   `Unchanged`-Fall des Amend-Helfers in `minds-git`.
//! - **Nichts anzuhängen ⇒ nichts anzufassen.** Ist alles schon da, kommt die
//!   Message unverändert zurück, inklusive ihrer Leerzeilen am Ende.
//!
//! Erwartet wird eine **bereinigte** Message, so wie sie im Commit-Objekt steht
//! — keine `#`-Kommentarzeilen, keine Scissors-Zeile aus `commit --verbose`.
//! Ein `prepare-commit-msg`-Hook (M6) muss die Datei also vorher bereinigen
//! oder den Amend-Weg nehmen; sonst landete der Trailer hinter der
//! Scissors-Zeile und Git würfe ihn weg.
//!
//! Dieses Modul hat **kein I/O**: es wandelt nur zwischen Text und Typ. Die
//! Message aus einem echten Commit zu lesen und einen Trailer nachzurüsten,
//! übernimmt `minds-git` (M3) — beides auf dieser Grammatik.
//!
//! Der zweite Trailer-Typ, `Minds-Attribution:`, folgt im nächsten Commit
//! (`feat(core): Attribution-Modell`) — deshalb ist [`Trailer`] ein Enum.

use std::fmt;
use std::str::FromStr;

use crate::change::{ChangeId, ChangeIdParseError};
use crate::id::{SessionId, SessionIdParseError};

/// Schlüssel des Session-Trailers, exakt so geschrieben, wie er in die
/// Commit-Message geht.
pub const SESSION_ID_TRAILER_KEY: &str = "Minds-Session-Id";

/// Schlüssel des Change-Id-Trailers.
pub const CHANGE_ID_TRAILER_KEY: &str = "Minds-Change-Id";

/// Ein einzelner Minds-Trailer aus einer Commit-Message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trailer {
    /// Verweis auf eine erfasste Session: `Minds-Session-Id: b3-<hex>`.
    SessionId(SessionId),
    /// Stabile Änderungs-Identität: `Minds-Change-Id: I<40 hex>`.
    ChangeId(ChangeId),
}

impl Trailer {
    /// Der Trailer-Schlüssel (links vom Doppelpunkt), kanonisch geschrieben.
    pub fn key(&self) -> &'static str {
        match self {
            Trailer::SessionId(_) => SESSION_ID_TRAILER_KEY,
            Trailer::ChangeId(_) => CHANGE_ID_TRAILER_KEY,
        }
    }

    /// Der Trailer-Wert (rechts vom Doppelpunkt), kanonisch geschrieben.
    pub fn value(&self) -> String {
        match self {
            Trailer::SessionId(id) => id.to_string(),
            Trailer::ChangeId(id) => id.to_string(),
        }
    }

    /// Extrahiert alle Minds-Trailer aus einer Commit-Message, in
    /// Auftretens-Reihenfolge und ohne Deduplizierung.
    ///
    /// Scannt jede Zeile — nicht nur Gits letzten Absatz —, damit beim `squash`
    /// konkatenierter Messages *alle* Trailer erhalten bleiben. Zeilen, die kein
    /// wohlgeformter Minds-Trailer sind, werden übersprungen.
    ///
    /// Eingerückte Zeilen zählen mit; warum, steht bei [`Trailer::from_str`].
    pub fn extract_all(message: &str) -> Vec<Trailer> {
        message
            .lines()
            .filter_map(|line| line.parse::<Trailer>().ok())
            .collect()
    }

    /// Alle über Trailer verlinkten [`SessionId`]s einer Commit-Message, in
    /// Auftretens-Reihenfolge und ohne Deduplizierung (das entscheidet der
    /// Aufrufer — content-adressiert ist eine doppelte ID dieselbe Session).
    pub fn session_ids(message: &str) -> Vec<SessionId> {
        Self::extract_all(message)
            .into_iter()
            .filter_map(|trailer| match trailer {
                Trailer::SessionId(id) => Some(id),
                Trailer::ChangeId(_) => None,
            })
            .collect()
    }

    /// Alle Change-Ids einer Message, in Auftretens-Reihenfolge.
    pub fn change_ids(message: &str) -> Vec<ChangeId> {
        Self::extract_all(message)
            .into_iter()
            .filter_map(|trailer| match trailer {
                Trailer::ChangeId(id) => Some(id),
                Trailer::SessionId(_) => None,
            })
            .collect()
    }

    /// Die (erste) Change-Id einer Message — der Regelfall, weil eine Änderung
    /// genau eine trägt.
    pub fn change_id(message: &str) -> Option<ChangeId> {
        Self::change_ids(message).into_iter().next()
    }

    /// Hängt einen einzelnen Trailer an — die Kurzform von
    /// [`Trailer::append_all`].
    pub fn append(message: &str, trailer: &Trailer) -> String {
        Self::append_all(message, std::slice::from_ref(trailer))
    }

    /// Hängt `trailers` an `message` an und gibt die vollständige neue Message
    /// zurück.
    ///
    /// Absatzregel, Idempotenz und die Erwartung an eine bereinigte Message
    /// stehen in der Modul-Doku. Trailer, die schon in der Message stehen — und
    /// Wiederholungen innerhalb von `trailers` — werden übersprungen; die
    /// übrigen kommen in der übergebenen Reihenfolge ans Ende. Die
    /// Zeilenenden der Eingabe (LF oder CRLF) werden beibehalten, die Ausgabe
    /// endet immer mit einem Zeilenumbruch.
    ///
    /// ```
    /// # use minds_core::Trailer;
    /// # let id = format!("b3-{}", "a".repeat(64)).parse().unwrap();
    /// let message = Trailer::append("fix: Retry-Test entflackert", &Trailer::SessionId(id));
    /// assert!(message.starts_with("fix: Retry-Test entflackert\n\nMinds-Session-Id: b3-"));
    /// ```
    pub fn append_all(message: &str, trailers: &[Trailer]) -> String {
        let present = Self::extract_all(message);

        let mut queued: Vec<&Trailer> = Vec::new();
        for trailer in trailers {
            if !present.contains(trailer) && !queued.contains(&trailer) {
                queued.push(trailer);
            }
        }

        // Nichts anzuhängen heißt: nichts anfassen. Kein normalisiertes
        // Zeilenende, keine gekappte Leerzeile — der Aufrufer soll ein
        // wiederholtes `append` nicht daran erkennen, dass sich der Text
        // bewegt hat.
        if queued.is_empty() {
            return message.to_owned();
        }

        let newline = if message.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let body = message.trim_end();

        let mut out = String::with_capacity(message.len() + queued.len() * 80);
        if !body.is_empty() {
            out.push_str(body);
            out.push_str(newline);
            if !last_paragraph_is_trailer_block(body) {
                out.push_str(newline);
            }
        }
        for trailer in queued {
            out.push_str(&trailer.to_string());
            out.push_str(newline);
        }
        out
    }
}

/// Ob der letzte Absatz von `message` ausschließlich aus Trailer-Zeilen besteht
/// — dann darf direkt angehängt werden, ohne Leerzeile davor.
///
/// `message` muss am Ende bereits getrimmt sein. Ohne Leerzeile gibt es nur
/// einen Absatz, und der ist der Betreff: `fix: etwas` erfüllt die
/// Trailer-Grammatik, ist aber keiner.
fn last_paragraph_is_trailer_block(message: &str) -> bool {
    let lines: Vec<&str> = message.lines().collect();
    let Some(blank) = lines.iter().rposition(|line| line.trim().is_empty()) else {
        return false;
    };

    let paragraph = &lines[blank + 1..];
    !paragraph.is_empty() && paragraph.iter().copied().all(looks_like_trailer_line)
}

/// Ob eine Zeile *aussieht* wie ein Trailer — beliebiger Schlüssel, nicht nur
/// unserer.
///
/// Bewusst strenger als [`Trailer::from_str`]: Hier geht es nicht ums Parsen,
/// sondern um die Frage, ob der letzte Absatz ein Trailer-Block ist. Ein
/// verlangter Leerraum hinter dem Doppelpunkt hält `https://example.test`
/// draußen, das sonst als `https:` + Wert durchginge.
fn looks_like_trailer_line(line: &str) -> bool {
    let line = line.trim_end();

    // Eingerückte Zeilen sind in Git Fortsetzungen des Trailers darüber.
    if line.starts_with(' ') || line.starts_with('\t') {
        return true;
    }

    match line.split_once(':') {
        Some((key, value)) => {
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && (value.is_empty() || value.starts_with(' '))
        }
        None => false,
    }
}

/// Kanonische Ausgabe: `<Schlüssel>: <Wert>`. Genau diese Zeile wird an die
/// Commit-Message angehängt.
impl fmt::Display for Trailer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key(), self.value())
    }
}

/// Fehler beim Parsen einer einzelnen Trailer-Zeile.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrailerParseError {
    /// Die Zeile enthält keinen Doppelpunkt, ist also keine `Key: Value`-Zeile.
    #[error("Zeile ist kein Trailer (kein ':' gefunden)")]
    NotATrailer,

    /// Ein `Key: Value` mit einem Schlüssel, der kein Minds-Trailer ist.
    #[error("unbekannter Trailer-Schlüssel: {0:?}")]
    UnknownKey(String),

    /// Der Schlüssel war `Minds-Session-Id`, der Wert aber keine gültige
    /// [`SessionId`].
    #[error("ungültige SessionId im Trailer: {0}")]
    SessionId(#[from] SessionIdParseError),

    /// Der Schlüssel war `Minds-Change-Id`, der Wert aber keine gültige
    /// [`ChangeId`].
    #[error("ungültige Change-Id im Trailer: {0}")]
    ChangeId(#[from] ChangeIdParseError),
}

impl FromStr for Trailer {
    type Err = TrailerParseError;

    /// Parst **eine** Zeile. Tolerant: Schlüssel case-insensitiv, Leerzeichen um
    /// Schlüssel und Wert werden entfernt, Hex im Wert darf groß sein (via
    /// [`SessionId`]).
    ///
    /// # Warum Einrückung erlaubt ist
    ///
    /// Gits eigene Trailer-Logik behandelt eingerückte Zeilen als
    /// Fortsetzungen. Diese hier tut es nicht — und das ist kein Versehen,
    /// sondern die Voraussetzung dafür, dass `git merge --squash` funktioniert:
    /// Git schreibt die Einzel-Messages im `log`-Format nach `SQUASH_MSG`, also
    /// mit **vier Leerzeichen eingerückten Rümpfen**. Bestünde diese Funktion
    /// auf Spalte 0, verlöre jeder Squash-Merge sämtliche Verweise — genau die
    /// Eigenschaft, für die der Trailer in der Message steht und nicht in einer
    /// `git note`.
    ///
    /// Falsch-Positive kostet das praktisch nichts: Ein eingerückter Fließtext
    /// müsste `Minds-Session-Id:` gefolgt von 64 gültigen Hex-Zeichen enthalten,
    /// um durchzukommen — und wäre dann mit an Sicherheit grenzender
    /// Wahrscheinlichkeit ein echter Verweis.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let line = line.trim_end();
        let (raw_key, raw_value) = line.split_once(':').ok_or(TrailerParseError::NotATrailer)?;
        let key = raw_key.trim();
        let value = raw_value.trim();

        if key.eq_ignore_ascii_case(SESSION_ID_TRAILER_KEY) {
            Ok(Trailer::SessionId(value.parse()?))
        } else if key.eq_ignore_ascii_case(CHANGE_ID_TRAILER_KEY) {
            Ok(Trailer::ChangeId(value.parse()?))
        } else {
            Err(TrailerParseError::UnknownKey(key.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut eine gültige [`SessionId`] aus einem wiederholten Hex-Zeichen.
    fn id(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    /// Die kanonische Trailer-Zeile zu einem Hex-Zeichen.
    fn line(hex: char) -> String {
        Trailer::SessionId(id(hex)).to_string()
    }

    /// Der Trailer zu einem Hex-Zeichen.
    fn trailer(hex: char) -> Trailer {
        Trailer::SessionId(id(hex))
    }

    #[test]
    fn display_is_canonical_line() {
        let line = Trailer::SessionId(id('a')).to_string();
        assert_eq!(line, format!("Minds-Session-Id: b3-{}", "a".repeat(64)));
    }

    #[test]
    fn key_and_value() {
        let t = Trailer::SessionId(id('a'));
        assert_eq!(t.key(), "Minds-Session-Id");
        assert_eq!(t.value(), format!("b3-{}", "a".repeat(64)));
    }

    #[test]
    fn parse_roundtrips_display() {
        let t = Trailer::SessionId(id('c'));
        let parsed: Trailer = t.to_string().parse().unwrap();
        assert_eq!(t, parsed);
    }

    #[test]
    fn parse_tolerates_whitespace() {
        // Zusätzliche Leerzeichen nach dem Doppelpunkt und am Zeilenende.
        let line = format!("Minds-Session-Id:    b3-{}   ", "a".repeat(64));
        assert_eq!(
            line.parse::<Trailer>().unwrap(),
            Trailer::SessionId(id('a'))
        );
    }

    #[test]
    fn parse_key_is_case_insensitive() {
        let line = format!("minds-session-id: b3-{}", "a".repeat(64));
        assert_eq!(
            line.parse::<Trailer>().unwrap(),
            Trailer::SessionId(id('a'))
        );
    }

    #[test]
    fn parse_accepts_uppercase_hex_value() {
        // Erbt die Toleranz von SessionId::from_str; hier nur bestätigt, dass
        // der Trailer den Wert durchreicht. Hex `A` == `a` ⇒ gleiche ID.
        let line = format!("Minds-Session-Id: b3-{}", "A".repeat(64));
        assert_eq!(
            line.parse::<Trailer>().unwrap(),
            Trailer::SessionId(id('a'))
        );
    }

    #[test]
    fn parse_rejects_line_without_colon() {
        assert_eq!(
            "kein trailer".parse::<Trailer>(),
            Err(TrailerParseError::NotATrailer)
        );
    }

    #[test]
    fn parse_rejects_unknown_key() {
        let err = "Signed-off-by: Jemand".parse::<Trailer>().unwrap_err();
        assert_eq!(err, TrailerParseError::UnknownKey("Signed-off-by".into()));
    }

    #[test]
    fn parse_tolerates_indented_key() {
        // `git merge --squash` rückt die übernommenen Rümpfe um vier Leerzeichen
        // ein — auf Spalte 0 zu bestehen verlöre dort jeden Verweis. Die
        // ausführliche Begründung steht an `from_str`.
        let line = format!("    Minds-Session-Id: b3-{}", "a".repeat(64));
        assert_eq!(
            line.parse::<Trailer>().unwrap(),
            Trailer::SessionId(id('a'))
        );
    }

    #[test]
    fn parse_rejects_malformed_session_id() {
        let err = "Minds-Session-Id: kein-hash"
            .parse::<Trailer>()
            .unwrap_err();
        assert!(matches!(err, TrailerParseError::SessionId(_)));
    }

    #[test]
    fn extract_all_finds_trailer_after_body() {
        let msg = format!(
            "fix: Retry-Test entflackert\n\
             \n\
             Backoff war zu kurz.\n\
             \n\
             Minds-Session-Id: b3-{}\n",
            "a".repeat(64)
        );
        assert_eq!(
            Trailer::extract_all(&msg),
            vec![Trailer::SessionId(id('a'))]
        );
    }

    #[test]
    fn extract_all_finds_multiple_sessions() {
        // Mehrere Sessions pro Commit ⇒ mehrere Trailer-Zeilen.
        let msg = format!(
            "feat: großer Wurf\n\nMinds-Session-Id: b3-{}\nMinds-Session-Id: b3-{}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            Trailer::extract_all(&msg),
            vec![Trailer::SessionId(id('a')), Trailer::SessionId(id('b'))]
        );
    }

    #[test]
    fn extract_all_collects_across_paragraphs_after_squash() {
        // Squash konkateniert zwei Messages — die Trailer landen in
        // verschiedenen Absätzen. Beide müssen gefunden werden.
        let msg = format!(
            "erster Commit\n\nMinds-Session-Id: b3-{}\n\n\
             zweiter Commit\n\nMinds-Session-Id: b3-{}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(Trailer::session_ids(&msg), vec![id('a'), id('b')]);
    }

    #[test]
    fn extract_all_reads_a_real_squash_merge_message() {
        // Genau das Format, das `git merge --squash` nach SQUASH_MSG schreibt:
        // Log-Ausgabe im Medium-Format, Rümpfe vier Leerzeichen eingerückt.
        // Vorher gingen hier alle Verweise verloren.
        let msg = format!(
            "Squashed commit of the following:\n\n\
             commit 1111111111111111111111111111111111111111\n\
             Author: Jemand <j@example.invalid>\n\
             Date:   Mon Jan 1 00:00:00 2024 +0000\n\n    \
             feat: c\n\n    Minds-Session-Id: b3-{b}\n\n\
             commit 2222222222222222222222222222222222222222\n\
             Author: Jemand <j@example.invalid>\n\
             Date:   Mon Jan 1 00:00:00 2024 +0000\n\n    \
             feat: b\n\n    Minds-Session-Id: b3-{a}\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
        );
        assert_eq!(Trailer::session_ids(&msg), vec![id('b'), id('a')]);
    }

    #[test]
    fn extract_all_ignores_prose_and_foreign_trailers() {
        let msg = format!(
            "docs: Kram\n\nSiehe auch: https://example.test\n\
             Signed-off-by: X\nMinds-Session-Id: b3-{}\n",
            "a".repeat(64)
        );
        assert_eq!(
            Trailer::extract_all(&msg),
            vec![Trailer::SessionId(id('a'))]
        );
    }

    #[test]
    fn extract_all_handles_crlf_line_endings() {
        let msg = format!("subject\r\n\r\nMinds-Session-Id: b3-{}\r\n", "a".repeat(64));
        assert_eq!(
            Trailer::extract_all(&msg),
            vec![Trailer::SessionId(id('a'))]
        );
    }

    #[test]
    fn session_ids_preserves_order_and_duplicates() {
        let msg = format!(
            "x\n\nMinds-Session-Id: b3-{a}\nMinds-Session-Id: b3-{b}\nMinds-Session-Id: b3-{a}\n",
            a = "a".repeat(64),
            b = "b".repeat(64),
        );
        assert_eq!(Trailer::session_ids(&msg), vec![id('a'), id('b'), id('a')]);
    }

    #[test]
    fn extract_all_empty_message_is_empty() {
        assert!(Trailer::extract_all("").is_empty());
    }

    // --- Schreiben -----------------------------------------------------------

    #[test]
    fn append_puts_the_trailer_in_its_own_paragraph() {
        let out = Trailer::append("feat: etwas", &trailer('a'));
        assert_eq!(out, format!("feat: etwas\n\n{}\n", line('a')));
    }

    #[test]
    fn append_does_not_glue_onto_a_subject_that_looks_like_a_trailer() {
        // `fix: etwas` erfüllt die Trailer-Grammatik — als *erste* Zeile ist es
        // trotzdem der Betreff. Ohne Leerzeile davor läse Git den Trailer als
        // Teil des Betreffs.
        let out = Trailer::append("fix: etwas", &trailer('a'));
        assert_eq!(out, format!("fix: etwas\n\n{}\n", line('a')));
    }

    #[test]
    fn append_extends_an_existing_trailer_block() {
        // Fremde Trailer im letzten Absatz ⇒ direkt anhängen, keine Leerzeile.
        let msg = "fix: etwas\n\nSigned-off-by: A <a@x.invalid>\n";
        let out = Trailer::append(msg, &trailer('a'));
        assert_eq!(
            out,
            format!(
                "fix: etwas\n\nSigned-off-by: A <a@x.invalid>\n{}\n",
                line('a')
            )
        );
    }

    #[test]
    fn append_starts_a_new_paragraph_after_prose() {
        let msg = "fix: etwas\n\nDer Backoff war zu kurz.\n";
        let out = Trailer::append(msg, &trailer('a'));
        assert_eq!(
            out,
            format!("fix: etwas\n\nDer Backoff war zu kurz.\n\n{}\n", line('a'))
        );
    }

    #[test]
    fn append_to_an_empty_message_is_just_the_trailer() {
        assert_eq!(
            Trailer::append("", &trailer('a')),
            format!("{}\n", line('a'))
        );
    }

    #[test]
    fn append_all_to_an_empty_message_writes_one_block() {
        let out = Trailer::append_all("", &[trailer('a'), trailer('b')]);
        assert_eq!(out, format!("{}\n{}\n", line('a'), line('b')));
    }

    #[test]
    fn append_normalises_trailing_blank_lines() {
        let out = Trailer::append("feat: etwas\n\n\n", &trailer('a'));
        assert_eq!(out, format!("feat: etwas\n\n{}\n", line('a')));
    }

    #[test]
    fn append_is_idempotent() {
        // Der Hook läuft zweimal — die Message darf sich nur einmal ändern.
        let once = Trailer::append("feat: etwas", &trailer('a'));
        let twice = Trailer::append(&once, &trailer('a'));
        assert_eq!(once, twice);
    }

    #[test]
    fn append_adds_only_what_is_missing() {
        let msg = format!("feat: etwas\n\n{}\n", line('a'));
        let out = Trailer::append_all(&msg, &[trailer('a'), trailer('b')]);
        assert_eq!(
            out,
            format!("feat: etwas\n\n{}\n{}\n", line('a'), line('b'))
        );
    }

    #[test]
    fn append_deduplicates_its_input() {
        let out = Trailer::append_all("feat: etwas", &[trailer('a'), trailer('a')]);
        assert_eq!(out, format!("feat: etwas\n\n{}\n", line('a')));
    }

    #[test]
    fn append_without_trailers_leaves_the_message_untouched() {
        let msg = "feat: etwas\n\n\n";
        assert_eq!(Trailer::append_all(msg, &[]), msg);
    }

    #[test]
    fn append_keeps_crlf_line_endings() {
        let out = Trailer::append("feat: etwas\r\n", &trailer('a'));
        assert_eq!(out, format!("feat: etwas\r\n\r\n{}\r\n", line('a')));
    }

    #[test]
    fn append_treats_a_lone_trailer_line_as_the_subject() {
        // Eine Message, die nur aus einem Trailer besteht: Die erste Zeile ist
        // und bleibt der Betreff, der neue Trailer bekommt einen eigenen Absatz.
        let msg = format!("{}\n", line('a'));
        let out = Trailer::append(&msg, &trailer('b'));
        assert_eq!(out, format!("{}\n\n{}\n", line('a'), line('b')));
    }

    #[test]
    fn appended_trailers_are_found_again() {
        // Die Zusage, auf der M3 aufsetzt: Was hier hineingeht, liest
        // `session_ids` in derselben Reihenfolge wieder heraus.
        let msg = Trailer::append_all("feat: etwas", &[trailer('a'), trailer('b')]);
        assert_eq!(Trailer::session_ids(&msg), vec![id('a'), id('b')]);
    }

    // --- Change-Id -----------------------------------------------------------

    fn cid() -> ChangeId {
        format!("I{}", "ab".repeat(20)).parse().unwrap()
    }

    #[test]
    fn a_change_id_trailer_roundtrips() {
        let t = Trailer::ChangeId(cid());
        assert_eq!(t.key(), "Minds-Change-Id");
        assert_eq!(
            t.to_string(),
            format!("Minds-Change-Id: I{}", "ab".repeat(20))
        );
        assert_eq!(t.to_string().parse::<Trailer>().unwrap(), t);
    }

    #[test]
    fn change_and_session_trailers_coexist_in_a_message() {
        let msg = format!(
            "fix: x\n\nMinds-Change-Id: I{c}\nMinds-Session-Id: b3-{s}\n",
            c = "ab".repeat(20),
            s = "a".repeat(64),
        );
        // session_ids filtert die Change-Id heraus…
        assert_eq!(Trailer::session_ids(&msg), vec![id('a')]);
        // …und change_id findet sie.
        assert_eq!(Trailer::change_id(&msg), Some(cid()));
    }

    #[test]
    fn a_change_id_survives_being_appended_and_read_back() {
        let msg = Trailer::append("feat: etwas", &Trailer::ChangeId(cid()));
        assert_eq!(Trailer::change_id(&msg), Some(cid()));
    }

    #[test]
    fn appending_to_a_squashed_message_keeps_the_earlier_trailers() {
        // Nach einem Squash stehen Trailer in mehreren Absätzen. Der neue kommt
        // ans Ende, die alten bleiben, wo sie sind.
        let msg = format!(
            "erster Commit\n\n{}\n\nzweiter Commit\n\n{}\n",
            line('a'),
            line('b')
        );
        let out = Trailer::append(&msg, &trailer('c'));
        assert_eq!(Trailer::session_ids(&out), vec![id('a'), id('b'), id('c')]);
    }
}
