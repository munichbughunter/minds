//! Der [`Redactor`]-Trait und seine Bausteine.
//!
//! Ein `Redactor` *findet* sensible Stellen in einem Text — er *ersetzt* nichts.
//! Ersetzen, Zusammenführen überlappender Funde, Zählen und der Platzhalter
//! liegen allein in der [`RedactionPipeline`](crate::RedactionPipeline). So
//! bleibt jeder Detektor eine reine Funktion (String rein, Spans raus), und die
//! byte-genaue Ersetzung passiert an einem einzigen Ort.
//!
//! Die konkreten Detektoren kommen in den Folge-Commits von M2; hier steht nur
//! der Vertrag, den sie erfüllen.

/// Kategorie eines Fundes. Bildet direkt auf die Zähler in
/// [`minds_core::RedactionCounts`] ab (`secrets` / `pii`).
///
/// Bewusst geschlossen für Schema v1 — eine neue Kategorie wäre eine Änderung am
/// Envelope (`RedactionCounts`) und damit ein Versions-Bump, kein Detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Zugangsdaten: API-Keys, Tokens, private Schlüssel, Passwörter.
    Secret,
    /// Personenbezogene Daten: E-Mail-Adressen und Ähnliches.
    Pii,
}

impl Category {
    /// Der Platzhalter, der einen Fund dieser Kategorie im Text ersetzt.
    ///
    /// Feste, **längen-unabhängige** Zeichenkette pro Kategorie — nie eine Maske
    /// aus so vielen Zeichen wie der Originalwert. Eine längen-erhaltende
    /// Maskierung würde die Länge des Geheimnisses verraten (und damit einen Teil
    /// seiner Entropie); der Record soll über den entfernten Wert *nichts*
    /// aussagen außer „hier stand etwas dieser Kategorie".
    pub const fn placeholder(self) -> &'static str {
        match self {
            Category::Secret => "[redacted:secret]",
            Category::Pii => "[redacted:pii]",
        }
    }

    /// Die kritischere zweier Kategorien; `Secret` schlägt `Pii`.
    ///
    /// Führt die Pipeline überlappende Funde verschiedener Kategorien zu einem
    /// Bereich zusammen, entscheidet dieser Vergleich, als was der Bereich zählt
    /// — im Zweifel als das Strengere.
    pub(crate) const fn max_severity(self, other: Category) -> Category {
        match (self, other) {
            (Category::Secret, _) | (_, Category::Secret) => Category::Secret,
            (Category::Pii, Category::Pii) => Category::Pii,
        }
    }
}

/// Ob `text` **exakt** ein Redaktions-Platzhalter ist.
///
/// Ein Wert, der bereits das Ergebnis der Redaktion ist, darf nicht ein zweites
/// Mal getroffen werden — sonst schriebe ein Detektor **anderer Kategorie** ihn
/// um (`[redacted:secret]` hinter `DB_USER=` → `[redacted:pii]`), der Text
/// änderte sich, und der Verifikationslauf in
/// [`redact_session`](crate::RedactionPipeline::redact_session) verwürfe die
/// ganze Session als instabil. Das war ein stiller Erfassungsausfall, kein Leck
/// — der Platzhalter trägt nichts Schützenswertes.
///
/// Bewusst ein **Exakt**-Vergleich: `PASSWORD=x[redacted:secret]` (ein echtes
/// Geheimnis, das den Marker zufällig enthält) muss weiter redigiert werden.
/// Der Vergleich sitzt in [`RedactionPipeline::redact`](crate::RedactionPipeline::redact),
/// gilt also für **jeden** Detektor — heutige wie künftige.
///
/// Die Liste der Platzhalter ist hier hand-aufgezählt; `placeholder()` erzwingt
/// bei einer neuen [`Category`] den Variant, **nicht** diese Stelle. Der Test
/// `every_category_placeholder_is_recognized` schließt die Lücke mit einem
/// exhaustiven `match`, der dann bricht.
pub(crate) fn is_redaction_placeholder(text: &str) -> bool {
    text == Category::Secret.placeholder() || text == Category::Pii.placeholder()
}

/// Ein sensibler Bereich in einem Text: halboffener Byte-Span `[start, end)`
/// plus Kategorie.
///
/// # Vertrag
///
/// Ein [`Redactor`] gibt Funde zurück, deren `start`/`end`
/// - Byte-Offsets in den *gescannten* Text sind,
/// - auf UTF-8-Zeichengrenzen liegen,
/// - `start < end` erfüllen (kein leerer Fund) und
/// - in-bounds sind (`end <= text.len()`).
///
/// Reihenfolge und Überlappung sind egal — die Pipeline sortiert und führt
/// überlappende Bereiche zusammen. Verletzt ein Fund den Vertrag, verwirft ihn
/// die Pipeline (im Debug-Build mit `debug_assert!`, damit fehlerhafte Detektoren
/// im Test auffliegen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finding {
    /// Byte-Offset des Beginns (inklusive), auf einer UTF-8-Grenze.
    pub start: usize,
    /// Byte-Offset des Endes (exklusive), auf einer UTF-8-Grenze.
    pub end: usize,
    /// Kategorie des Fundes.
    pub category: Category,
}

impl Finding {
    /// Ein Fund der Kategorie `category` über `[start, end)`.
    pub const fn new(category: Category, start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            category,
        }
    }
}

/// Ein Detektor, der sensible Stellen einer oder mehrerer Kategorien in einem
/// Text findet.
///
/// `Send + Sync`, weil Detektoren zustandslos und rein sind — das hält die Tür
/// für parallele Bereinigung offen, ohne den Trait später brechen zu müssen.
pub trait Redactor: Send + Sync {
    /// Kurzer, stabiler Bezeichner für Audit und Debug (z. B. `"aws-access-key"`,
    /// `"email"`). Der Session-weite Redaction-Audit (späterer Commit) nutzt ihn,
    /// um Funde ihrem Detektor zuzuordnen.
    fn name(&self) -> &str;

    /// Findet alle sensiblen Stellen in `text` und gibt sie als [`Finding`]s
    /// zurück.
    ///
    /// Die Funde müssen den [`Finding`]-Vertrag erfüllen (gültige, nicht leere
    /// Byte-Spans auf UTF-8-Grenzen). Reihenfolge und Überlappung sind egal.
    fn scan(&self, text: &str) -> Vec<Finding>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_fixed_per_category() {
        assert_eq!(Category::Secret.placeholder(), "[redacted:secret]");
        assert_eq!(Category::Pii.placeholder(), "[redacted:pii]");
    }

    #[test]
    fn every_category_placeholder_is_recognized() {
        // Der Compile-Anker, den `is_redaction_placeholder` selbst nicht hat:
        // Eine neue `Category`-Variante macht dieses `match` non-exhaustiv und
        // bricht den Build hier — die Erinnerung, den Platzhalter mitzuziehen.
        for category in [Category::Secret, Category::Pii] {
            match category {
                Category::Secret | Category::Pii => {}
            }
            assert!(
                is_redaction_placeholder(category.placeholder()),
                "{category:?}-Platzhalter nicht erkannt"
            );
        }
        // Und die Gegenprobe: gewöhnlicher Text ist keiner.
        assert!(!is_redaction_placeholder("[redacted:secret]x"));
        assert!(!is_redaction_placeholder("hunter2"));
    }

    #[test]
    fn secret_outranks_pii_in_severity() {
        assert_eq!(
            Category::Secret.max_severity(Category::Pii),
            Category::Secret
        );
        assert_eq!(
            Category::Pii.max_severity(Category::Secret),
            Category::Secret
        );
        assert_eq!(Category::Pii.max_severity(Category::Pii), Category::Pii);
        assert_eq!(
            Category::Secret.max_severity(Category::Secret),
            Category::Secret
        );
    }
}
