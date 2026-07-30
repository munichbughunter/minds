//! Konkrete PII-Detektoren für M2.
//!
//! Personenbezogene Daten unterscheiden sich strukturell von Secrets: Ein
//! API-Key trägt sein Format mit sich (Präfix, Länge, Alphabet) und ist damit
//! *erkennbar*. Ein Name, eine Adresse, eine Telefonnummer sind es nicht — sie
//! sehen aus wie gewöhnlicher Text.
//!
//! Daraus folgt die Arbeitsteilung dieses Moduls:
//!
//! - **Strukturell erkennbare PII** bekommt einen eingebauten Detektor. In v0.1
//!   ist das genau eine Form: die E-Mail-Adresse ([`EmailRedactor`]). Sie hat
//!   eine eindeutige Grammatik (`local@label.tld`), ist praktisch
//!   fehlalarmfrei erkennbar und taucht in Agent-Transkripten real auf
//!   (Git-Autor, Issue-Zitate, Log-Ausschnitte).
//! - **Alles Kontextabhängige** (Kundennamen, interne Hostnamen, Projekt-
//!   Codenamen) gehört in die **Denylist**
//!   ([`DenyListRedactor`](crate::DenyListRedactor)). Ein Team kann diese
//!   Begriffe aufzählen; ein Heuristik-Detektor kann sie nur raten.
//!
//! # Bewusst *nicht* eingebaut
//!
//! - **Telefonnummern.** Das Muster „Ziffernfolge mit Trennzeichen" trifft in
//!   einem Entwickler-Transkript auf Versionsnummern, Ports, Zeitstempel,
//!   Byte-Offsets, Issue-IDs. Der Fehlalarm-Preis ist hoch, der Ertrag gering.
//! - **IPv4-Adressen.** `1.2.3.4` ist von einer Versionsnummer nicht
//!   unterscheidbar; zudem ist eine Server-IP im Log meist keine PII.
//! - **Namen.** Ohne Wörterbuch und Kontext nicht entscheidbar — und mit
//!   Wörterbuch eine Fehlalarm-Maschine. Das ist der Denylist-Fall.
//! - **IBAN / Kreditkarte.** Beide sind prüfsummen-validierbar (mod-97,
//!   Luhn) und damit *saubere* Kandidaten für einen eingebauten Detektor —
//!   aber in Agent-Transkripten über Code kommen sie kaum vor. Nachrüstbar,
//!   sobald jemand sie tatsächlich braucht.
//!
//! Wie im Secret-Modul gilt: Detektoren *finden* nur, das Ersetzen und Zählen
//! bleibt allein Sache der [`RedactionPipeline`](crate::RedactionPipeline).

use regex::Regex;

use crate::redactor::{Category, Finding, Redactor};

/// Muster einer E-Mail-Adresse — pragmatisch, nicht RFC-5322-vollständig.
///
/// Die vollständige RFC-Grammatik (Kommentare, quoted strings, Klammer-IPs)
/// ist als Regex weder lesbar noch nützlich: solche Adressen kommen in
/// Transkripten nicht vor, blähen aber die Fehlalarm-Fläche auf. Erkannt wird
/// die Form, die real auftritt:
///
/// - `\b` am Anfang verhindert Treffer mitten in einem Wort und sorgt dafür,
///   dass der Fund beim ersten Wortzeichen beginnt (führende `.`/`-`/`+` des
///   Kontexts bleiben also außen vor).
/// - **Local part:** 1–64 Zeichen aus `A–Z a–z 0–9 . _ % + -`.
/// - **Domain-Labels:** 1–8 Labels, jedes beginnt und endet alphanumerisch und
///   darf innen Bindestriche haben (`-bar.de` ist damit kein Treffer).
/// - **TLD:** 2–24 Buchstaben. Das schließt `1.2@3.4` aus (Zahl statt TLD) und
///   erzwingt mindestens einen Punkt — `kein@ding` ist kein Treffer.
///
/// Alle Wiederholungen sind gedeckelt; die Regex-Engine von Rust ist ohnehin
/// linear (RE2-Linie, kein Backtracking), die Deckelung hält aber zusätzlich
/// die Match-Länge kalkulierbar.
///
/// Nicht abgedeckt: internationalisierte Domains/Local-Parts (Umlaute).
/// Bewusst — sie kommen praktisch nicht vor, und ein Unicode-Alphabet würde den
/// Detektor auf normale Prosa loslassen.
const EMAIL_PATTERN: &str = concat!(
    r"\b[A-Za-z0-9._%+-]{1,64}",
    r"@",
    r"(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.){1,8}",
    r"[A-Za-z]{2,24}\b",
);

/// Detektor für E-Mail-Adressen. Liefert ausschließlich
/// [`Category::Pii`]-Funde.
///
/// Kompiliert die Regex **einmal** bei der Konstruktion. Ein eigener
/// Aho-Corasick-Vorfilter (wie bei den Token-Formen) wäre hier doppelt gemoppelt:
/// das Muster enthält das Pflicht-Literal `@`, und die `regex`-Crate baut sich
/// daraus selbst einen Literal-Vorfilter — Text ohne `@` wird also ohnehin
/// nicht Zeichen für Zeichen geprüft.
pub struct EmailRedactor {
    re: Regex,
}

impl EmailRedactor {
    /// Baut den Detektor. Das Muster ist eine Konstante und in den Tests
    /// abgedeckt — ein Kompilierfehler wäre ein Programmierfehler, deshalb
    /// `expect` statt eines Laufzeit-`Result`.
    pub fn new() -> Self {
        Self {
            re: Regex::new(EMAIL_PATTERN).expect("konstantes E-Mail-Muster muss kompilieren"),
        }
    }
}

impl Default for EmailRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for EmailRedactor {
    fn name(&self) -> &str {
        "email"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        // Regex-Matches liegen immer auf UTF-8-Zeichengrenzen; das Muster ist
        // reines ASCII, der Fund enthält also nie ein Mehrbyte-Zeichen.
        self.re
            .find_iter(text)
            .map(|m| Finding::new(Category::Pii, m.start(), m.end()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::RedactionPipeline;
    use crate::secret::KnownTokenRedactor;

    /// Bequemer Durchlauf durch eine Pipeline mit genau einem Detektor.
    fn redact_with<R: Redactor + 'static>(r: R, text: &str) -> (String, u32) {
        let out = RedactionPipeline::new().with(r).redact(text);
        (out.text, out.counts.pii)
    }

    // --- Positiv-Fälle --------------------------------------------------------

    #[test]
    fn detects_simple_email() {
        let (text, n) = redact_with(
            EmailRedactor::new(),
            "Melde dich bei anna@example.org bitte",
        );
        assert_eq!(text, "Melde dich bei [redacted:pii] bitte");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_dotted_and_plus_local_part() {
        let (text, n) = redact_with(
            EmailRedactor::new(),
            "max.mustermann+ci@example.com gemeldet",
        );
        assert_eq!(text, "[redacted:pii] gemeldet");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_subdomain_and_country_tld() {
        let (text, n) = redact_with(EmailRedactor::new(), "user@mail.sub.example.co.uk");
        assert_eq!(text, "[redacted:pii]");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_uppercase_email() {
        let (_, n) = redact_with(EmailRedactor::new(), "MAX@EXAMPLE.COM");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_email_inside_url() {
        // `git remote`-Zeilen und Log-Ausschnitte sehen so aus.
        let (text, n) = redact_with(EmailRedactor::new(), "https://user@example.com/repo.git");
        assert_eq!(text, "https://[redacted:pii]/repo.git");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_multiple_emails() {
        let (text, n) = redact_with(EmailRedactor::new(), "a@b.de,c@d.io");
        assert_eq!(text, "[redacted:pii],[redacted:pii]");
        assert_eq!(n, 2);
    }

    // --- Negativ-Fälle (kein Fehlalarm) --------------------------------------

    #[test]
    fn at_without_tld_is_ignored() {
        let (text, n) = redact_with(EmailRedactor::new(), "ping @kollege und kein@ding");
        assert_eq!(text, "ping @kollege und kein@ding");
        assert_eq!(n, 0);
    }

    #[test]
    fn missing_local_part_is_ignored() {
        let (text, n) = redact_with(EmailRedactor::new(), "Handle @example.com");
        assert_eq!(text, "Handle @example.com");
        assert_eq!(n, 0);
    }

    #[test]
    fn version_like_at_expression_is_ignored() {
        // Ziffern-TLD ⇒ kein Treffer. Genau die Sorte False-Positive, die ein
        // naives `\S+@\S+` produzieren würde.
        let (text, n) = redact_with(EmailRedactor::new(), "gebaut mit 1.2@3.4 und pkg@1.0.0");
        assert_eq!(text, "gebaut mit 1.2@3.4 und pkg@1.0.0");
        assert_eq!(n, 0);
    }

    #[test]
    fn label_starting_with_hyphen_is_ignored() {
        let (text, n) = redact_with(EmailRedactor::new(), "foo@-bar.de");
        assert_eq!(text, "foo@-bar.de");
        assert_eq!(n, 0);
    }

    #[test]
    fn prose_and_hashes_are_untouched() {
        let prose = "Commit 356a192b7913b04c54574d18c28d46e6395428ab: Reviewer liest die Absicht.";
        let (text, n) = redact_with(EmailRedactor::new(), prose);
        assert_eq!(text, prose);
        assert_eq!(n, 0);
    }

    // --- Ränder & Struktur ----------------------------------------------------

    #[test]
    fn trailing_punctuation_stays_outside_the_finding() {
        let (text, n) = redact_with(EmailRedactor::new(), "Schreib an anna@example.org.");
        assert_eq!(text, "Schreib an [redacted:pii].");
        assert_eq!(n, 1);
    }

    #[test]
    fn leading_punctuation_stays_outside_the_finding() {
        // `\b` setzt den Fund auf das erste Wortzeichen — der Bindestrich davor
        // gehört zum Kontext, nicht zur Adresse.
        let (text, n) = redact_with(EmailRedactor::new(), "-anna@example.org");
        assert_eq!(text, "-[redacted:pii]");
        assert_eq!(n, 1);
    }

    #[test]
    fn multibyte_context_is_preserved() {
        // é (2 Byte) und 🦀 (4 Byte) rund um den Fund: die Byte-Offsets müssen
        // stimmen, sonst würde die Ersetzung paniken.
        let (text, n) = redact_with(EmailRedactor::new(), "café 🦀 anna@example.org 🦀 café");
        assert_eq!(text, "café 🦀 [redacted:pii] 🦀 café");
        assert_eq!(n, 1);
    }

    #[test]
    fn findings_are_pii_category() {
        let f = EmailRedactor::new().scan("anna@example.org");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].category, Category::Pii);
        assert_eq!(f[0].start, 0);
        assert_eq!(f[0].end, 16);
    }

    #[test]
    fn email_redacted_output_never_contains_the_address() {
        let (text, _) = redact_with(EmailRedactor::new(), "kontakt: anna@example.org");
        assert!(!text.contains("anna@example.org"));
        assert!(!text.contains("example.org"));
    }

    #[test]
    fn email_name_is_stable() {
        assert_eq!(EmailRedactor::new().name(), "email");
    }

    // --- Zusammenspiel mit den Secret-Detektoren ------------------------------

    #[test]
    fn email_and_secret_are_counted_in_separate_buckets() {
        let out = RedactionPipeline::new()
            .with(KnownTokenRedactor::new())
            .with(EmailRedactor::new())
            .redact("von anna@example.org, key AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out.text, "von [redacted:pii], key [redacted:secret]");
        assert_eq!(out.counts.pii, 1);
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn known_token_is_not_mistaken_for_an_email() {
        let (text, n) = redact_with(
            EmailRedactor::new(),
            "ghp_w2wMqZcUDIh7yfJs1ON43xKmTecQoXsf2o3g",
        );
        assert_eq!(text, "ghp_w2wMqZcUDIh7yfJs1ON43xKmTecQoXsf2o3g");
        assert_eq!(n, 0);
    }
}
