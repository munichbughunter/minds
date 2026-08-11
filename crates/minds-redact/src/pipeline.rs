//! Die [`RedactionPipeline`]: führt mehrere [`Redactor`]s über einen Text und
//! liefert den bereinigten Text plus Zähler.
//!
//! Die Detektoren *finden* nur (siehe [`crate::redactor`]); das eigentliche
//! Bereinigen passiert hier in einem Durchlauf:
//!
//! 1. **Einsammeln** — alle Funde aller Redactors.
//! 2. **Prüfen** — vertragswidrige Spans (leer, out-of-bounds, nicht auf einer
//!    UTF-8-Grenze) werden verworfen **und in
//!    [`RedactedText::invalid_findings`] gezählt**. Verwerfen allein wäre
//!    fail-open: Der zugehörige Text bliebe ungeschwärzt stehen, und niemand
//!    erführe davon. Der Zähler macht den Detektor-Bug sichtbar; zum harten
//!    Abbruch erhebt ihn
//!    [`redact_session`](RedactionPipeline::redact_session).
//! 3. **Allowlist** — Funde, deren Text *exakt* auf der
//!    [`AllowList`] steht, fallen raus (Beispiel-Keys aus Doku, Bot-Adressen
//!    aus Fixtures). Der Filter greift **vor** dem Zusammenführen und prüft nur
//!    den Text des jeweiligen Fundes: Ein überlappender Fund eines anderen
//!    Detektors bleibt bestehen — die Allowlist kann fremde Funde nicht
//!    aufheben (fail-closed).
//! 4. **Sortieren** nach Start.
//! 5. **Zusammenführen** — *echt* überlappende Bereiche werden zu einem vereint.
//!    Das ist die fail-closed-korrekte Wahl: würde man bei Überlappung nur den
//!    „ersten" Fund ersetzen, bliebe der überstehende Rest des zweiten stehen —
//!    ein Teilleck. Aneinandergrenzende (sich nur berührende) Funde bleiben
//!    getrennt.
//! 6. **Ersetzen & Zählen** — jeder vereinte Bereich wird durch den Platzhalter
//!    seiner Kategorie (`Category::max_severity` bei gemischten Funden) ersetzt
//!    und einmal gezählt.
//!
//! Ein Nebeneffekt des Zusammenführens: melden zwei Detektoren denselben Span
//! (z. B. dieselbe Schlüssel-Zeichenkette), wird er einmal ersetzt und einmal
//! gezählt — Dedup gratis.
//!
//! [`RedactedText`] trägt **nur den bereinigten Text und Zähler**, niemals die
//! entfernten Werte — dieselbe Zusage wie [`minds_core::Redaction`]. Verankert
//! ist sie eine Ebene höher: [`crate::session`] führt den Lauf über die ganze
//! Session, setzt `applied`, schreibt die Summe ins Envelope und bricht ab,
//! wenn hier etwas schiefging.

use minds_core::RedactionCounts;

use crate::config::AllowList;
use crate::redactor::{Category, Finding, Redactor, is_redaction_placeholder};

/// Ergebnis einer Bereinigung: der Text ohne sensible Stellen und die Zähler der
/// ersetzten Funde je Kategorie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedText {
    /// Der bereinigte Text — an jeder gefundenen Stelle steht der Platzhalter der
    /// jeweiligen Kategorie statt des Originalwerts.
    pub text: String,
    /// Wie viele Funde je Kategorie ersetzt wurden. **Nur Zähler, nie Werte.**
    pub counts: RedactionCounts,
    /// Wie viele Funde den [`Finding`]-Vertrag verletzt haben und deshalb
    /// verworfen werden mussten.
    ///
    /// Jeder davon ist ein Detektor-Bug **und ein mögliches Leck**: Der Span
    /// ließ sich nicht ersetzen, der Text steht also noch da. `redact` kann
    /// daran nichts ändern — aber es verschweigt es nicht. Wer die Zusage
    /// „redigiert" geben will, muss hier auf einer Null bestehen; genau das tut
    /// [`redact_session`](RedactionPipeline::redact_session).
    pub invalid_findings: u32,
}

/// Eine geordnete Kette von [`Redactor`]s, die gemeinsam einen Text bereinigen,
/// plus die [`AllowList`], die einzelne Funde wieder freigibt.
///
/// Die Reihenfolge, in der Redactors hinzugefügt werden, beeinflusst das Ergebnis
/// **nicht** — die Pipeline sortiert und führt Funde zusammen. Sie ist nur die
/// Sammlung der aktiven Detektoren.
///
/// Zusammengebaut wird sie normalerweise aus der Policy:
/// [`RedactionConfig::pipeline`](crate::RedactionConfig::pipeline).
#[derive(Default)]
pub struct RedactionPipeline {
    redactors: Vec<Box<dyn Redactor>>,
    allow: AllowList,
}

impl RedactionPipeline {
    /// Eine leere Pipeline ohne Detektoren und ohne Allowlist. Sie ist gültig und
    /// bereinigt nichts: jeder Text geht unverändert durch, die Zähler bleiben
    /// null. Detektoren kommen über [`with`](Self::with) / [`push`](Self::push)
    /// hinzu.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fügt einen Redactor hinzu und gibt die Pipeline zurück (Builder-Stil).
    #[must_use]
    pub fn with<R: Redactor + 'static>(mut self, redactor: R) -> Self {
        self.push(redactor);
        self
    }

    /// Fügt einen Redactor hinzu.
    pub fn push<R: Redactor + 'static>(&mut self, redactor: R) {
        self.redactors.push(Box::new(redactor));
    }

    /// Setzt die Allowlist und gibt die Pipeline zurück (Builder-Stil).
    #[must_use]
    pub fn with_allowlist(mut self, allow: AllowList) -> Self {
        self.set_allowlist(allow);
        self
    }

    /// Setzt die Allowlist (ersetzt eine bereits gesetzte).
    pub fn set_allowlist(&mut self, allow: AllowList) {
        self.allow = allow;
    }

    /// Die aktive Allowlist.
    pub fn allowlist(&self) -> &AllowList {
        &self.allow
    }

    /// Anzahl der Detektoren in der Pipeline.
    pub fn len(&self) -> usize {
        self.redactors.len()
    }

    /// `true`, wenn kein Detektor konfiguriert ist.
    pub fn is_empty(&self) -> bool {
        self.redactors.is_empty()
    }

    /// Bereinigt `text`: ersetzt jede von einem Detektor gefundene Stelle durch
    /// den Platzhalter ihrer Kategorie und zählt die Ersetzungen.
    ///
    /// Infallibel — ein Text lässt sich immer bereinigen. Vertragswidrige Funde
    /// eines Detektors werden verworfen und in
    /// [`RedactedText::invalid_findings`] gemeldet (siehe Modul-Doku). Die
    /// fail-closed-*Garantie* — abbrechen, statt ein solches Ergebnis zu
    /// verwenden — sitzt eine Ebene höher, in
    /// [`redact_session`](Self::redact_session).
    pub fn redact(&self, text: &str) -> RedactedText {
        let mut invalid_findings = 0u32;
        let mut findings: Vec<Finding> = self
            .redactors
            .iter()
            .flat_map(|redactor| redactor.scan(text))
            .filter(|finding| {
                let ok = is_valid(finding, text);
                if !ok {
                    invalid_findings = invalid_findings.saturating_add(1);
                }
                ok
            })
            // Erst nach `is_valid` slicen — vorher ist der Span nicht
            // garantiert in-bounds und auf einer Zeichengrenze.
            .filter(|finding| !self.allow.allows(&text[finding.start..finding.end]))
            // Ein bereits redigierter Platzhalter wird nicht ein zweites Mal
            // getroffen: Sonst schriebe ein Detektor anderer Kategorie ihn um,
            // der Text änderte sich, und der Verifikationslauf verwürfe die
            // Session als instabil. Zentral hier, damit es für **jeden**
            // Detektor gilt — nicht nur den, der zufällig einen Guard hat.
            .filter(|finding| !is_redaction_placeholder(&text[finding.start..finding.end]))
            .collect();

        // Nach Start aufsteigend; bei gleichem Start der längere Fund zuerst. Das
        // Zusammenführen unten braucht nur monoton steigende Starts.
        findings.sort_unstable_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));

        let mut out = String::with_capacity(text.len());
        let mut counts = RedactionCounts::default();
        let mut cursor = 0usize; // Bis hierher ist das Original schon verarbeitet.

        let mut i = 0;
        while i < findings.len() {
            let start = findings[i].start;
            let mut end = findings[i].end;
            let mut category = findings[i].category;
            i += 1;

            // Alle *echt* überlappenden Folgefunde in diesen Bereich ziehen.
            while i < findings.len() && findings[i].start < end {
                end = end.max(findings[i].end);
                category = category.max_severity(findings[i].category);
                i += 1;
            }

            // `start` >= `cursor`: Starts steigen monoton, `cursor` ist das Ende
            // des vorigen Bereichs — der Slice ist also immer vorwärts und liegt
            // (wie `end`) auf einer UTF-8-Grenze.
            out.push_str(&text[cursor..start]);
            out.push_str(category.placeholder());
            tally(&mut counts, category);
            cursor = end;
        }
        out.push_str(&text[cursor..]);

        RedactedText {
            text: out,
            counts,
            invalid_findings,
        }
    }
}

/// Prüft den [`Finding`]-Vertrag: nicht leer, in-bounds, beide Enden auf einer
/// UTF-8-Zeichengrenze.
///
/// Verletzungen sind Detektor-Bugs. Hier stand ein `debug_assert!` — gut
/// gemeint, aber in zweifacher Hinsicht die falsche Bauform: Im Release
/// verschwand die Prüfung, und *dort* ist der verworfene Fund ein stilles Leck;
/// und ein Panic ließ sich nicht testen, weil er genau den Fehlerpfad abschoss,
/// den man prüfen wollte. Stattdessen zählt [`RedactionPipeline::redact`] die
/// Verstöße in [`RedactedText::invalid_findings`] — in jedem Build gleich,
/// beobachtbar, und auf Session-Ebene ein harter Abbruch.
fn is_valid(finding: &Finding, text: &str) -> bool {
    finding.start < finding.end
        && finding.end <= text.len()
        && text.is_char_boundary(finding.start)
        && text.is_char_boundary(finding.end)
}

/// Zählt einen ersetzten Fund in den passenden Kategorie-Zähler.
fn tally(counts: &mut RedactionCounts, category: Category) {
    match category {
        Category::Secret => counts.secrets = counts.secrets.saturating_add(1),
        Category::Pii => counts.pii = counts.pii.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Test-Redactors -------------------------------------------------------
    //
    // Nur für Tests; die echten Detektoren stehen in `secret` / `pii`.

    /// Markiert jedes Vorkommen eines festen Substrings als Fund. Rechnet mit den
    /// echten Byte-Offsets im Text und ist damit zugleich der UTF-8-Prüfstein.
    struct Needle {
        needle: &'static str,
        category: Category,
    }

    impl Needle {
        fn new(category: Category, needle: &'static str) -> Self {
            Self { needle, category }
        }
    }

    impl Redactor for Needle {
        fn name(&self) -> &str {
            "needle"
        }

        fn scan(&self, text: &str) -> Vec<Finding> {
            let mut out = Vec::new();
            let mut from = 0;
            while let Some(rel) = text[from..].find(self.needle) {
                let start = from + rel;
                let end = start + self.needle.len();
                out.push(Finding::new(self.category, start, end));
                from = end;
            }
            out
        }
    }

    /// Liefert eine feste Fund-Liste unabhängig vom Text — für punktgenaue
    /// Kontrolle über Reihenfolge, Überlappung und Duplikate.
    struct Fixed(Vec<Finding>);

    impl Redactor for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }

        fn scan(&self, _text: &str) -> Vec<Finding> {
            self.0.clone()
        }
    }

    // --- Tests ----------------------------------------------------------------

    #[test]
    fn empty_pipeline_passes_text_through() {
        let p = RedactionPipeline::new();
        let r = p.redact("nichts Sensibles hier");
        assert_eq!(r.text, "nichts Sensibles hier");
        assert_eq!(r.counts, RedactionCounts::default());
        assert!(p.is_empty());
        assert!(p.allowlist().is_empty());
    }

    #[test]
    fn no_match_leaves_text_and_counts_untouched() {
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "KEY"));
        let r = p.redact("hier steht kein Treffer");
        assert_eq!(r.text, "hier steht kein Treffer");
        assert_eq!(r.counts, RedactionCounts::default());
    }

    #[test]
    fn single_secret_is_replaced_and_counted() {
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "SECRET"));
        let r = p.redact("token=SECRET;");
        assert_eq!(r.text, "token=[redacted:secret];");
        assert_eq!(r.counts.secrets, 1);
        assert_eq!(r.counts.pii, 0);
    }

    #[test]
    fn multiple_disjoint_findings_all_replaced() {
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "X"));
        let r = p.redact("aXbXc");
        assert_eq!(r.text, "a[redacted:secret]b[redacted:secret]c");
        assert_eq!(r.counts.secrets, 2);
    }

    #[test]
    fn categories_are_counted_separately() {
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "SEC"))
            .with(Needle::new(Category::Pii, "MAIL"));
        let r = p.redact("SEC und MAIL");
        assert_eq!(r.text, "[redacted:secret] und [redacted:pii]");
        assert_eq!(r.counts.secrets, 1);
        assert_eq!(r.counts.pii, 1);
    }

    #[test]
    fn redacted_output_never_contains_the_original_value() {
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "hunter2"));
        let r = p.redact("passwort ist hunter2 ok");
        assert!(!r.text.contains("hunter2"));
        assert_eq!(r.counts.secrets, 1);
    }

    #[test]
    fn overlapping_findings_merge_into_one_no_tail_leaks() {
        // Zwei Detektoren mit überschneidenden Treffern auf "abcdef":
        // "abcd" = [0,4) secret, "cdef" = [2,6) pii. Der vereinte Bereich [0,6)
        // wird *einmal* ersetzt (kein überstehender Rest bleibt) und zählt als
        // Secret (die strengere Kategorie).
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "abcd"))
            .with(Needle::new(Category::Pii, "cdef"));
        let r = p.redact("abcdef");
        assert_eq!(r.text, "[redacted:secret]");
        assert_eq!(r.counts.secrets, 1);
        assert_eq!(r.counts.pii, 0);
    }

    #[test]
    fn duplicate_span_from_two_redactors_is_deduped() {
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "AKIA"))
            .with(Needle::new(Category::Secret, "AKIA"));
        let r = p.redact("id=AKIA!");
        assert_eq!(r.text, "id=[redacted:secret]!");
        assert_eq!(r.counts.secrets, 1);
    }

    #[test]
    fn adjacent_findings_stay_separate() {
        // "AABB": "AA" = [0,2) secret grenzt an "BB" = [2,4) pii — sie berühren
        // sich, überlappen aber nicht. Also zwei Platzhalter, zwei Zähler.
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "AA"))
            .with(Needle::new(Category::Pii, "BB"));
        let r = p.redact("AABB");
        assert_eq!(r.text, "[redacted:secret][redacted:pii]");
        assert_eq!(r.counts.secrets, 1);
        assert_eq!(r.counts.pii, 1);
    }

    #[test]
    fn findings_are_sorted_before_replacing() {
        // Detektor liefert die Funde in umgekehrter Reihenfolge; das Ergebnis
        // muss trotzdem stimmen, weil die Pipeline sortiert. Auf "aXbXc" werden
        // Byte 0 ('a') und Byte 4 ('c') ersetzt.
        let unsorted = Fixed(vec![
            Finding::new(Category::Secret, 4, 5),
            Finding::new(Category::Secret, 0, 1),
        ]);
        let p = RedactionPipeline::new().with(unsorted);
        let r = p.redact("aXbXc");
        assert_eq!(r.text, "[redacted:secret]XbX[redacted:secret]");
        assert_eq!(r.counts.secrets, 2);
    }

    #[test]
    fn redactor_order_does_not_change_result() {
        let text = "abcdef";
        let a = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "abcd"))
            .with(Needle::new(Category::Pii, "cdef"))
            .redact(text);
        let b = RedactionPipeline::new()
            .with(Needle::new(Category::Pii, "cdef"))
            .with(Needle::new(Category::Secret, "abcd"))
            .redact(text);
        assert_eq!(a, b);
    }

    #[test]
    fn multibyte_text_around_findings_is_preserved() {
        // Nicht-ASCII (é, 🦀) rund um den Treffer bleibt intakt; die Ausgabe ist
        // gültiges UTF-8.
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "SECRET"));
        let r = p.redact("café 🦀 SECRET 🦀 café");
        assert_eq!(r.text, "café 🦀 [redacted:secret] 🦀 café");
        assert_eq!(r.counts.secrets, 1);
    }

    #[test]
    fn multibyte_needle_is_found_on_char_boundaries() {
        // Der Treffer selbst ist mehrbytig ('é' = 2 Bytes). Lägen die Offsets
        // nicht auf Zeichengrenzen, würde die Ersetzung paniken.
        let p = RedactionPipeline::new().with(Needle::new(Category::Pii, "café"));
        let r = p.redact("x café y café z");
        assert_eq!(r.text, "x [redacted:pii] y [redacted:pii] z");
        assert_eq!(r.counts.pii, 2);
    }

    // --- Allowlist ------------------------------------------------------------

    #[test]
    fn allowlisted_finding_is_neither_replaced_nor_counted() {
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "SECRET"))
            .with_allowlist(["secret"].into_iter().collect());
        let r = p.redact("token=SECRET;");
        assert_eq!(r.text, "token=SECRET;");
        assert_eq!(r.counts, RedactionCounts::default());
    }

    #[test]
    fn allowlist_only_frees_the_matching_finding() {
        // "SECRET" ist erlaubt, "OTHER" nicht — der zweite Fund bleibt.
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "SECRET"))
            .with(Needle::new(Category::Secret, "OTHER"))
            .with_allowlist(["SECRET"].into_iter().collect());
        let r = p.redact("a SECRET b OTHER");
        assert_eq!(r.text, "a SECRET b [redacted:secret]");
        assert_eq!(r.counts.secrets, 1);
    }

    #[test]
    fn allowlist_does_not_free_an_overlapping_foreign_finding() {
        // "abcd" ist erlaubt, aber "cdef" überlappt und ist es nicht. Der
        // überlappende Fund wird ersetzt — die Allowlist hebt nur ihren eigenen
        // Fund auf, nie fremde. Fail-closed.
        let p = RedactionPipeline::new()
            .with(Needle::new(Category::Secret, "abcd"))
            .with(Needle::new(Category::Pii, "cdef"))
            .with_allowlist(["abcd"].into_iter().collect());
        let r = p.redact("abcdef");
        assert_eq!(r.text, "ab[redacted:pii]");
        assert_eq!(r.counts.pii, 1);
        assert_eq!(r.counts.secrets, 0);
    }

    // --- Vertragsverstöße -----------------------------------------------------

    #[test]
    fn contract_violations_are_counted_not_swallowed() {
        // Out-of-bounds-Span: Der Fund lässt sich nicht ersetzen, der Text bleibt
        // unverändert — und genau das meldet der Zähler, statt es zu schlucken.
        // `redact_session` macht daraus einen Abbruch.
        let p = RedactionPipeline::new().with(Fixed(vec![Finding::new(Category::Secret, 0, 99)]));
        let r = p.redact("kurz");
        assert_eq!(r.text, "kurz");
        assert_eq!(r.counts, RedactionCounts::default());
        assert_eq!(r.invalid_findings, 1);
    }

    #[test]
    fn empty_and_unaligned_spans_count_as_violations() {
        // 'é' ist zwei Bytes: [2,2) ist leer, [0,1) endet mitten im Zeichen.
        let p = RedactionPipeline::new().with(Fixed(vec![
            Finding::new(Category::Secret, 2, 2),
            Finding::new(Category::Pii, 0, 1),
        ]));
        let r = p.redact("é");
        assert_eq!(r.text, "é");
        assert_eq!(r.invalid_findings, 2);
    }

    #[test]
    fn valid_findings_leave_the_violation_counter_at_zero() {
        let p = RedactionPipeline::new().with(Needle::new(Category::Secret, "SECRET"));
        assert_eq!(p.redact("x SECRET y").invalid_findings, 0);
    }
}
