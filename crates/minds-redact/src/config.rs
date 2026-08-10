//! Konfigurierbare Redaction-Policy: [`RedactionConfig`], [`AllowList`],
//! [`DenyListRedactor`].
//!
//! Die eingebauten Detektoren erkennen, was *strukturell* erkennbar ist
//! (Token-Formen, hohe Entropie, E-Mail-Adressen). Zwei Dinge kann kein
//! Detektor wissen:
//!
//! - **Was hier sensibel ist, obwohl es harmlos aussieht.** Kundenname,
//!   interner Hostname, Projekt-Codename. Dafür die **Denylist**: ein
//!   [`Redactor`], der aufgezählte Begriffe findet — genau die Klasse von PII,
//!   für die ein Heuristik-Detektor nur raten könnte.
//! - **Was hier harmlos ist, obwohl es sensibel aussieht.** Der in AWS' Doku
//!   veröffentlichte Beispiel-Key `AKIAIOSFODNN7EXAMPLE`, die
//!   `noreply@example.com` aus dem Fixture. Dafür die **Allowlist**.
//!
//! # Die Allowlist ist ein Loch — deshalb ist sie eng
//!
//! Jeder Eintrag hebt Redaction auf, ist also ein potenzieller Leck-Kanal. Drei
//! Einschränkungen halten ihn klein:
//!
//! 1. **Nur exakte Volltreffer.** Ein Fund wird verworfen, wenn *sein gesamter
//!    Text* auf der Liste steht. `example.com` erlaubt also **nicht**
//!    `anna@example.com` — die Adresse wird weiter redigiert.
//! 2. **Keine Wildcards, keine Regex, keine Präfixe.** Ein Muster, das mehr
//!    trifft, als der Autor überblickt, ist genau das Risiko, das wir nicht
//!    wollen.
//! 3. **Sie kann keinen fremden Fund aufheben.** Deckt ein anderer Detektor
//!    einen überlappenden Bereich ab, wird der weiterhin ersetzt. Fail-closed.
//!
//! Und die Regel, die nirgends im Code steht, aber gilt: In die Allowlist
//! gehört **nie ein echtes Geheimnis** — die Datei liegt im Repo.
//!
//! # Beispiel-Konfiguration
//!
//! Dieses Crate ist format-agnostisch (es kennt nur `serde`); ob die Datei TOML
//! oder JSON ist und wo sie liegt, entscheidet die CLI.
//!
//! ```toml
//! known_tokens    = true
//! email           = true
//! keyed_values    = true     # DB_PASSWORD=… — der .env-Fall
//! url_credentials = true     # postgres://user:pw@host
//! short_flags     = true     # curl -u user:pass
//! allow           = ["AKIAIOSFODNN7EXAMPLE", "noreply@example.com"]
//! secret_keys     = ["VAULT_ROLE_ID"]        # Feldnamen
//! deny_secrets    = ["korrekt-pferd-batterie-klammer"]
//! deny_pii        = ["Max Mustermann", "kunde-nordlicht"]
//!
//! [high_entropy]
//! enabled          = true
//! min_len          = 32
//! min_entropy_bits = 4.5
//! ```
//!
//! # Warum die Config `deny_unknown_fields` hat, das Envelope aber nicht
//!
//! Das Session-Envelope ist **vorwärts-tolerant** (Architektur-Prinzip 4): Es
//! liest Daten aus der Vergangenheit, und ein unbekanntes Feld darf einen alten
//! Reader nicht brechen. Die Config ist der umgekehrte Fall — sie ist die
//! *jetzt* getippte Eingabe eines Menschen. Ein stillschweigend ignoriertes
//! `deny_pi = [...]` (Tippfehler) würde die Policy lautlos abschalten: fail-open.
//! Also wird die Config strikt gelesen und ein unbekannter Schlüssel ist ein
//! Fehler.

use std::collections::HashSet;

use aho_corasick::{AhoCorasick, MatchKind};
use serde::{Deserialize, Serialize};

use crate::assignment::{KeyValueRedactor, ShortFlagRedactor, UrlCredentialRedactor};
use crate::pii::EmailRedactor;
use crate::pipeline::RedactionPipeline;
use crate::redactor::{Category, Finding, Redactor};
use crate::secret::{
    DEFAULT_ENTROPY_BITS, DEFAULT_MIN_LEN, HighEntropyRedactor, KnownTokenRedactor,
};

// ---------------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------------

/// Menge von Zeichenketten, die **nicht** redigiert werden, obwohl ein Detektor
/// sie gefunden hat.
///
/// Der Vergleich ist ein **Volltreffer** gegen den Text des Fundes, nicht gegen
/// den umgebenden Text: `AllowList` sieht nur `text[finding.start..finding.end]`.
/// Groß-/Kleinschreibung wird im ASCII-Bereich ignoriert (`AKIA…` == `akia…`),
/// Nicht-ASCII wird byte-genau verglichen — bewusst, weil Unicode-Case-Folding
/// überraschende Gleichheiten erzeugt und Secrets/Adressen ohnehin ASCII sind.
///
/// Leere und nur aus Leerraum bestehende Einträge werden beim Aufbau verworfen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowList {
    /// Einträge, ASCII-kleingeschrieben und getrimmt.
    terms: HashSet<String>,
}

impl AllowList {
    /// Eine leere Allowlist. Sie erlaubt nichts — jeder Fund bleibt ein Fund.
    pub fn new() -> Self {
        Self::default()
    }

    /// Nimmt einen Eintrag auf. Leere/Leerraum-Einträge werden ignoriert.
    /// Gibt `true` zurück, wenn der Eintrag neu war.
    pub fn insert(&mut self, term: impl AsRef<str>) -> bool {
        let term = term.as_ref().trim();
        if term.is_empty() {
            return false;
        }
        self.terms.insert(term.to_ascii_lowercase())
    }

    /// `true`, wenn `matched` **exakt** auf der Liste steht (ASCII-case-insensitiv).
    pub fn allows(&self, matched: &str) -> bool {
        if self.terms.is_empty() {
            // Häufigster Fall — spart die Allokation für die Kleinschreibung.
            return false;
        }
        self.terms.contains(&matched.to_ascii_lowercase())
    }

    /// Anzahl der Einträge.
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    /// `true`, wenn kein Eintrag konfiguriert ist.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

impl<S: AsRef<str>> FromIterator<S> for AllowList {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut list = Self::new();
        for term in iter {
            list.insert(term);
        }
        list
    }
}

// ---------------------------------------------------------------------------
// Denylist
// ---------------------------------------------------------------------------

/// Detektor über eine konfigurierte Liste von Begriffen.
///
/// Das ist die Antwort auf kontextabhängige PII (Namen, interne Hostnamen,
/// Codenamen): Was ein Detektor nicht *erkennen* kann, kann ein Team
/// *aufzählen*.
///
/// # Semantik
///
/// - **Teilstring-Treffer, keine Wortgrenzen.** `acme` trifft auch in
///   `acme-internal.example`. Das ist die fail-closed-Wahl: Ein Begriff, den
///   jemand explizit auf die Denylist gesetzt hat, soll nicht deshalb
///   durchrutschen, weil ein Bindestrich dranhängt. Der Preis sind mögliche
///   Treffer mitten in Wörtern — wer das nicht will, wählt einen längeren,
///   spezifischeren Begriff.
/// - **ASCII-case-insensitiv.** `Acme` == `ACME` == `acme`.
/// - **Längster Treffer gewinnt** (`MatchKind::LeftmostLongest`): Stehen `acme`
///   und `acme corp` auf der Liste, wird der längere Bereich ersetzt. Bei
///   gleicher Länge gewinnt der zuerst aufgeführte Begriff — deshalb reiht
///   [`RedactionConfig::pipeline`] `deny_secrets` **vor** `deny_pii` ein: im
///   Zweifel zählt der Treffer als das Strengere.
/// - **Leere Begriffe werden verworfen.** Ein leeres Muster würde an jeder
///   Position einen Null-Längen-Fund erzeugen — den der Pipeline-Vertrag
///   ohnehin ablehnt (`start < end`).
pub struct DenyListRedactor {
    /// `None`, wenn kein (nicht-leerer) Begriff konfiguriert ist.
    matcher: Option<AhoCorasick>,
    /// Kategorie je Muster-ID des Automaten, gleiche Reihenfolge wie beim Bau.
    categories: Vec<Category>,
}

impl DenyListRedactor {
    /// Ein Detektor ohne Begriffe. Gültig und findet nichts.
    pub fn empty() -> Self {
        Self {
            matcher: None,
            categories: Vec::new(),
        }
    }

    /// Baut den Detektor aus `(Begriff, Kategorie)`-Paaren.
    ///
    /// Leere und Leerraum-Begriffe werden übersprungen. Der Fehlerfall
    /// (`aho_corasick::BuildError`) ist ein pathologischer — z. B. absurd viele
    /// oder absurd lange Begriffe. Er wird trotzdem als `Result` gemeldet und
    /// nicht gepanict: Die Begriffe sind Benutzereingabe, und wenn die Policy
    /// nicht gebaut werden kann, muss Capture abbrechen (fail-closed), nicht
    /// ungefiltert weiterlaufen.
    pub fn from_terms<I, S>(terms: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (S, Category)>,
        S: AsRef<str>,
    {
        let mut patterns: Vec<String> = Vec::new();
        let mut categories: Vec<Category> = Vec::new();

        for (term, category) in terms {
            let term = term.as_ref().trim();
            if term.is_empty() {
                continue;
            }
            patterns.push(term.to_string());
            categories.push(category);
        }

        if patterns.is_empty() {
            return Ok(Self::empty());
        }

        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .ascii_case_insensitive(true)
            .build(&patterns)?;

        Ok(Self {
            matcher: Some(matcher),
            categories,
        })
    }

    /// Anzahl der aktiven Begriffe.
    pub fn len(&self) -> usize {
        self.categories.len()
    }

    /// `true`, wenn kein Begriff konfiguriert ist.
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }
}

impl Default for DenyListRedactor {
    fn default() -> Self {
        Self::empty()
    }
}

impl Redactor for DenyListRedactor {
    fn name(&self) -> &str {
        "denylist"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        let Some(matcher) = &self.matcher else {
            return Vec::new();
        };
        // UTF-8 ist selbst-synchronisierend: ein gültiges UTF-8-Muster kann in
        // einem gültigen UTF-8-Text nur auf Zeichengrenzen treffen. Die
        // ASCII-Case-Insensitivität ändert daran nichts — sie variiert nur
        // Bytes < 0x80, die in Mehrbyte-Sequenzen nie vorkommen.
        matcher
            .find_iter(text)
            .map(|m| Finding::new(self.categories[m.pattern().as_usize()], m.start(), m.end()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Fehler beim Übersetzen einer [`RedactionConfig`] in eine Pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Die Denylist ließ sich nicht in einen Automaten übersetzen.
    #[error("Denylist konnte nicht übersetzt werden: {0}")]
    DenyList(#[from] aho_corasick::BuildError),

    /// Aus `secret_keys` ließ sich kein Muster bauen.
    #[error("Schlüsselwörter konnten nicht übersetzt werden: {0}")]
    Pattern(#[from] regex::Error),
}

/// Parameter des [`HighEntropyRedactor`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HighEntropyConfig {
    /// Detektor aktiv?
    pub enabled: bool,
    /// Mindestlänge eines Kandidaten-Laufs.
    pub min_len: usize,
    /// Entropieschwelle in bit/Zeichen. Der einzige Fließkommawert der Config —
    /// er geht nie ins Envelope (dessen Kanonisierung lehnt Floats ab), sondern
    /// nur in einen Schwellenvergleich.
    pub min_entropy_bits: f64,
}

impl Default for HighEntropyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_len: DEFAULT_MIN_LEN,
            min_entropy_bits: DEFAULT_ENTROPY_BITS,
        }
    }
}

/// Die Redaction-Policy eines Repos.
///
/// Der Default ist die sichere Wahl: **alle eingebauten Detektoren an**, Allow-
/// und Denylist leer. Wer nichts konfiguriert, bekommt volle Redaction; jede
/// Abschwächung ist eine bewusste, sichtbare Zeile in der Config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedactionConfig {
    /// Bekannte Token-Formen erkennen (AWS, GitHub, GitLab, PEM …).
    pub known_tokens: bool,
    /// E-Mail-Adressen erkennen.
    pub email: bool,
    /// Werte hinter sensiblen Schlüsselnamen erkennen (`DB_PASSWORD=…`) — der
    /// `.env`-Fall. **Ausschalten heißt: Passwörter aus Config-Dateien landen
    /// im Record.**
    pub keyed_values: bool,
    /// Zugangsdaten im Autoritätsteil einer URL erkennen
    /// (`postgres://user:pw@host`).
    pub url_credentials: bool,
    /// Short-CLI-Flags für Authentifizierung erkennen (`curl -u user:pass`).
    pub short_flags: bool,
    /// Generisches High-Entropy-Auffangnetz.
    pub high_entropy: HighEntropyConfig,
    /// Zusätzliche **Schlüsselnamen**, deren Wert als Geheimnis gilt
    /// (`VAULT_ROLE_ID`, `LDAP_BIND_DN`). Sie werden strikt behandelt: jeder
    /// Wert dahinter verschwindet, egal wie kurz.
    ///
    /// Unterschied zu [`deny_secrets`](Self::deny_secrets): Dort steht der
    /// **Wert**, hier der **Name des Feldes**. Wer den Wert kennt, muss ihn
    /// nicht in eine Datei im Repo schreiben — für den Namen gilt das nicht.
    pub secret_keys: Vec<String>,
    /// Funde, deren Text exakt hier steht, werden **nicht** ersetzt.
    pub allow: Vec<String>,
    /// Zusätzliche Begriffe, die als [`Category::Secret`] gelten.
    pub deny_secrets: Vec<String>,
    /// Zusätzliche Begriffe, die als [`Category::Pii`] gelten.
    pub deny_pii: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            known_tokens: true,
            email: true,
            keyed_values: true,
            url_credentials: true,
            short_flags: true,
            high_entropy: HighEntropyConfig::default(),
            secret_keys: Vec::new(),
            allow: Vec::new(),
            deny_secrets: Vec::new(),
            deny_pii: Vec::new(),
        }
    }
}

impl RedactionConfig {
    /// Die konfigurierte [`AllowList`].
    pub fn allowlist(&self) -> AllowList {
        self.allow.iter().collect()
    }

    /// Der konfigurierte [`DenyListRedactor`].
    ///
    /// `deny_secrets` kommt vor `deny_pii`: Steht derselbe Begriff in beiden
    /// Listen, gewinnt bei gleicher Trefferlänge der zuerst aufgeführte — also
    /// die strengere Kategorie.
    pub fn denylist(&self) -> Result<DenyListRedactor, ConfigError> {
        DenyListRedactor::from_terms(
            self.deny_secrets
                .iter()
                .map(|t| (t.as_str(), Category::Secret))
                .chain(self.deny_pii.iter().map(|t| (t.as_str(), Category::Pii))),
        )
    }

    /// Baut die vollständige [`RedactionPipeline`] dieser Policy.
    ///
    /// Die Reihenfolge der Detektoren ist für das Ergebnis egal (die Pipeline
    /// sortiert und führt zusammen) — sie folgt hier der Lesbarkeit.
    pub fn pipeline(&self) -> Result<RedactionPipeline, ConfigError> {
        let mut pipeline = RedactionPipeline::new();

        if self.known_tokens {
            pipeline.push(KnownTokenRedactor::new());
        }
        if self.high_entropy.enabled {
            pipeline.push(HighEntropyRedactor::with_params(
                self.high_entropy.min_len,
                self.high_entropy.min_entropy_bits,
            ));
        }
        if self.email {
            pipeline.push(EmailRedactor::new());
        }
        if self.keyed_values {
            pipeline.push(KeyValueRedactor::with_extra_keys(&self.secret_keys)?);
        }
        if self.url_credentials {
            pipeline.push(UrlCredentialRedactor::new());
        }
        if self.short_flags {
            pipeline.push(ShortFlagRedactor::new());
        }

        let denylist = self.denylist()?;
        if !denylist.is_empty() {
            pipeline.push(denylist);
        }

        pipeline.set_allowlist(self.allowlist());
        Ok(pipeline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AllowList ------------------------------------------------------------

    #[test]
    fn allowlist_suppresses_exact_match() {
        // Der in AWS' Doku veröffentlichte Beispiel-Key — genau der Fall, für
        // den es die Allowlist gibt.
        let cfg = RedactionConfig {
            allow: vec!["AKIAIOSFODNN7EXAMPLE".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("key = AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out.text, "key = AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out.counts.secrets, 0);
    }

    #[test]
    fn allowlist_is_ascii_case_insensitive() {
        let mut allow = AllowList::new();
        allow.insert("NoReply@Example.COM");
        assert!(allow.allows("noreply@example.com"));
        assert!(allow.allows("NOREPLY@EXAMPLE.COM"));
    }

    #[test]
    fn allowlist_matches_only_the_whole_finding() {
        // `example.com` erlaubt *nicht* `anna@example.com` — sonst wäre die
        // Allowlist ein Präfix-/Teilstring-Loch.
        let cfg = RedactionConfig {
            allow: vec!["example.com".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("mail anna@example.com");
        assert_eq!(out.text, "mail [redacted:pii]");
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn allowlist_cannot_unblock_an_overlapping_finding() {
        // Die Adresse steht auf der Allowlist, `nordlicht` aber auf der
        // Denylist. Der Denylist-Fund ist ein *anderer* Span — er bleibt.
        // Fail-closed: die Allowlist hebt nur ihren eigenen Fund auf.
        let cfg = RedactionConfig {
            allow: vec!["anna@nordlicht.example".into()],
            deny_pii: vec!["nordlicht".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("von anna@nordlicht.example");
        assert!(!out.text.contains("nordlicht"));
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn allowlist_ignores_empty_and_blank_entries() {
        let allow: AllowList = ["", "   ", "ok"].into_iter().collect();
        assert_eq!(allow.len(), 1);
        assert!(allow.allows("ok"));
        assert!(!allow.allows(""));
    }

    #[test]
    fn allowlist_trims_entries() {
        let allow: AllowList = ["  anna@example.org  "].into_iter().collect();
        assert!(allow.allows("anna@example.org"));
    }

    #[test]
    fn empty_allowlist_allows_nothing() {
        let allow = AllowList::new();
        assert!(allow.is_empty());
        assert!(!allow.allows("irgendwas"));
    }

    // --- DenyList -------------------------------------------------------------

    #[test]
    fn denylist_redacts_configured_term() {
        let cfg = RedactionConfig {
            deny_pii: vec!["Max Mustermann".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("Ticket von Max Mustermann");
        assert_eq!(out.text, "Ticket von [redacted:pii]");
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn denylist_is_case_insensitive() {
        let d = DenyListRedactor::from_terms([("acme", Category::Pii)]).unwrap();
        let out = RedactionPipeline::new().with(d).redact("ACME und Acme");
        assert_eq!(out.text, "[redacted:pii] und [redacted:pii]");
        assert_eq!(out.counts.pii, 2);
    }

    #[test]
    fn denylist_matches_substrings() {
        // Dokumentierte fail-closed-Semantik: kein Wortgrenzen-Zwang.
        let d = DenyListRedactor::from_terms([("acme", Category::Pii)]).unwrap();
        let out = RedactionPipeline::new()
            .with(d)
            .redact("acme-internal.test");
        assert_eq!(out.text, "[redacted:pii]-internal.test");
    }

    #[test]
    fn denylist_longest_term_wins() {
        let d =
            DenyListRedactor::from_terms([("acme", Category::Pii), ("acme corp", Category::Pii)])
                .unwrap();
        let out = RedactionPipeline::new()
            .with(d)
            .redact("bei acme corp gehört");
        assert_eq!(out.text, "bei [redacted:pii] gehört");
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn denylist_keeps_categories_apart() {
        let cfg = RedactionConfig {
            deny_secrets: vec!["korrekt-pferd".into()],
            deny_pii: vec!["nordlicht".into()],
            ..RedactionConfig::default()
        };
        let out = cfg
            .pipeline()
            .unwrap()
            .redact("korrekt-pferd bei nordlicht");
        assert_eq!(out.text, "[redacted:secret] bei [redacted:pii]");
        assert_eq!(out.counts.secrets, 1);
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn denylist_secret_wins_on_equal_length_tie() {
        let cfg = RedactionConfig {
            deny_secrets: vec!["zwitter".into()],
            deny_pii: vec!["zwitter".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("hier: zwitter");
        assert_eq!(out.counts.secrets, 1);
        assert_eq!(out.counts.pii, 0);
    }

    #[test]
    fn empty_denylist_finds_nothing() {
        let d = DenyListRedactor::empty();
        assert!(d.is_empty());
        assert!(d.scan("beliebiger Text").is_empty());
    }

    #[test]
    fn blank_deny_terms_are_dropped() {
        // Kritisch: ein leeres Muster würde an jeder Position einen
        // Null-Längen-Fund erzeugen und den Finding-Vertrag verletzen.
        let d =
            DenyListRedactor::from_terms([("", Category::Pii), ("  ", Category::Secret)]).unwrap();
        assert!(d.is_empty());
        let out = RedactionPipeline::new().with(d).redact("unberührt");
        assert_eq!(out.text, "unberührt");
        assert_eq!(out.counts, minds_core::RedactionCounts::default());
    }

    #[test]
    fn denylist_handles_multibyte_terms_and_context() {
        let d = DenyListRedactor::from_terms([("Müller", Category::Pii)]).unwrap();
        let out = RedactionPipeline::new()
            .with(d)
            .redact("🦀 Frau Müller schrieb");
        assert_eq!(out.text, "🦀 Frau [redacted:pii] schrieb");
        assert_eq!(out.counts.pii, 1);
    }

    #[test]
    fn denylist_name_is_stable() {
        assert_eq!(DenyListRedactor::empty().name(), "denylist");
    }

    // --- Config ---------------------------------------------------------------

    #[test]
    fn default_enables_all_builtin_detectors() {
        let cfg = RedactionConfig::default();
        assert!(cfg.known_tokens);
        assert!(cfg.email);
        assert!(cfg.high_entropy.enabled);
        assert!(cfg.allow.is_empty());

        assert!(cfg.keyed_values);
        assert!(cfg.url_credentials);
        assert!(cfg.short_flags);

        // known-token + high-entropy + email + key-value + url-credential + short-flag,
        // keine Denylist.
        assert_eq!(cfg.pipeline().unwrap().len(), 6);
    }

    #[test]
    fn default_config_leaks_nothing_from_a_dotenv_file() {
        // Die Regressionsprobe für den ganzen Zweck dieser Policy: Was hier
        // durchrutscht, steht später in GitLab.
        let env = "DB_USER=admin\n\
                   DB_PASSWORD=hunter2\n\
                   SMTP_PASSWORD=Sommer2024!\n\
                   JWT_SECRET=abc123\n\
                   DATABASE_URL=postgres://admin:s3cr3t@db.internal:5432/prod\n\
                   GITLAB_TOKEN=glpat-ABCDEFGHIJ1234567890\n";
        let out = RedactionConfig::default().pipeline().unwrap().redact(env);

        for leak in [
            "admin",
            "hunter2",
            "Sommer2024",
            "abc123",
            "s3cr3t",
            "glpat-ABCDEFGHIJ1234567890",
        ] {
            assert!(!out.text.contains(leak), "{leak:?} überlebt:\n{}", out.text);
        }
        // Die Struktur bleibt lesbar.
        assert!(out.text.contains("DB_PASSWORD="));
        assert!(out.text.contains("postgres://"));
    }

    #[test]
    fn secret_keys_extend_the_strict_tier() {
        let cfg = RedactionConfig {
            secret_keys: vec!["VAULT_ROLE_ID".into()],
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("VAULT_ROLE_ID=r1");
        assert_eq!(out.text, "VAULT_ROLE_ID=[redacted:secret]");
    }

    #[test]
    fn default_pipeline_catches_secret_and_pii() {
        let out = RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact("anna@example.org meldet AKIAIOSFODNN7EXAMPLE");
        assert_eq!(out.text, "[redacted:pii] meldet [redacted:secret]");
        assert_eq!(out.counts.pii, 1);
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn disabled_detector_is_not_wired() {
        let cfg = RedactionConfig {
            email: false,
            ..RedactionConfig::default()
        };
        let out = cfg.pipeline().unwrap().redact("anna@example.org");
        assert_eq!(out.text, "anna@example.org");
        assert_eq!(out.counts.pii, 0);
    }

    #[test]
    fn high_entropy_params_reach_the_detector() {
        let cfg = RedactionConfig {
            known_tokens: false,
            email: false,
            high_entropy: HighEntropyConfig {
                enabled: true,
                min_len: 20,
                min_entropy_bits: 1.0,
            },
            ..RedactionConfig::default()
        };
        let out = cfg
            .pipeline()
            .unwrap()
            .redact("constantatetetetetetetetetetuvwxyz");
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn denylist_is_only_wired_when_non_empty() {
        let cfg = RedactionConfig {
            known_tokens: false,
            email: false,
            keyed_values: false,
            url_credentials: false,
            short_flags: false,
            high_entropy: HighEntropyConfig {
                enabled: false,
                ..HighEntropyConfig::default()
            },
            deny_pii: vec!["  ".into()],
            ..RedactionConfig::default()
        };
        assert!(cfg.pipeline().unwrap().is_empty());
    }

    // --- serde ----------------------------------------------------------------

    #[test]
    fn serde_roundtrips_through_json() {
        let cfg = RedactionConfig {
            email: false,
            allow: vec!["noreply@example.com".into()],
            deny_pii: vec!["nordlicht".into()],
            ..RedactionConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: RedactionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: RedactionConfig = serde_json::from_str(r#"{"email": false}"#).unwrap();
        assert!(!cfg.email);
        assert!(cfg.known_tokens);
        assert_eq!(cfg.high_entropy, HighEntropyConfig::default());
    }

    #[test]
    fn empty_object_is_the_default_config() {
        let cfg: RedactionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, RedactionConfig::default());
    }

    #[test]
    fn unknown_field_is_rejected() {
        // Fail-closed gegen Tippfehler: `deny_pi` würde die Policy sonst
        // lautlos abschalten.
        let err = serde_json::from_str::<RedactionConfig>(r#"{"deny_pi": ["x"]}"#).unwrap_err();
        assert!(err.to_string().contains("deny_pi"));
    }

    #[test]
    fn nested_unknown_field_is_rejected() {
        let err = serde_json::from_str::<RedactionConfig>(r#"{"high_entropy": {"min_ln": 8}}"#)
            .unwrap_err();
        assert!(err.to_string().contains("min_ln"));
    }
}
