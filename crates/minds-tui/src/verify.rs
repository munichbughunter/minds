//! Das Urteil von `minds verify` über einen Commit, wie die Übersicht es in
//! EVIDENCE führt (C3.4, C12.2) — zusammengefasst, nie nachgebaut: Was
//! Signaturen braucht (Stufe, Intent-Signatur, Scope), liefert die
//! [`crate::Source`] mit denselben Bausteinen wie `minds verify`. Fehlt das,
//! sagt die Zeile es, statt zu raten.

/// Das Urteil, zusammengefasst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overall {
    /// Intakt und vollständig.
    Verified,
    /// Intakt, aber lückenhaft.
    Incomplete,
    /// Eine Session ohne Seal — nichts zu verifizieren (wie `verify`).
    NotVerifiable,
    /// Die Signer werden noch geprüft — noch kein Urteil.
    Checking,
    /// Die Signer ließen sich nicht prüfen — kein VERIFIED ohne sie.
    NotChecked,
    /// Material verändert.
    Tampered,
}
