//! Wann `minds verify` eine Datei-Beobachtung des Witness (EA-08) gelten
//! lässt.
//!
//! Eine Beobachtung kann eine Zeile **erklären** und damit über
//! `--require-explained` entscheiden. Unter `refs/minds/` kann im geteilten
//! Repository aber auch der Agent schreiben — Objekt, Seal und Signatur. Die
//! Vertrauensfrage ist deshalb strenger als bei der bloßen Anzeige eines
//! Witness-Seals:
//!
//! - **Woher die Signer-Datei kommt:** nur `--signers` oder
//!   `~/.ssh/allowed_signers` — nie `gpg.ssh.allowedSignersFile` aus der
//!   Repo-Konfiguration, die der Agent schreiben kann.
//! - **Wer Witness ist:** ein Principal, dessen **sämtliche** Zeilen auf genau
//!   `namespaces="minds-witness"` beschränkt sind. Ein unbeschränkter
//!   Entwickler-Schlüssel verifiziert unter jedem Namespace — mit ihm könnte
//!   jeder, der ihn nutzen darf (auch ein Agent), einen „Witness"-Seal
//!   signieren.
//! - **Was geprüft wird:** die Signatur über genau den Seal-Text unter
//!   `minds-witness`.
//!
//! Fehlt eines davon, gilt keine Beobachtung — fail-closed.

use std::path::{Path, PathBuf};

use minds_core::ContentHash;
use minds_store::ContextStore;

/// Höchstgröße der Signer-Datei, die gelesen wird.
const MAX_SIGNERS_BYTES: u64 = 1024 * 1024;

/// Das Vertrauens-Prädikat für `witness-fs/v1`-Seals.
pub(super) fn observation_trust<'a>(
    store: &'a dyn ContextStore,
    signers: Option<&str>,
) -> impl Fn(&ContentHash, &str) -> bool + 'a {
    let file = trusted_signers_file(signers);
    let content = file.as_deref().and_then(|path| {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_SIGNERS_BYTES {
            return None;
        }
        std::fs::read_to_string(path).ok()
    });
    move |seal_id: &ContentHash, text: &str| {
        let (Some(path), Some(content)) = (file.as_deref(), content.as_deref()) else {
            return false;
        };
        if !minds_attest::ssh_keygen_available() {
            return false;
        }
        let Ok(Some(signature)) = store.seal_signature(seal_id) else {
            return false;
        };
        let Ok(principals) = minds_attest::ssh_find_principals(&signature, path) else {
            return false;
        };
        principals.iter().any(|principal| {
            witness_only(content, principal)
                && minds_attest::ssh_verify_ns(
                    text,
                    &signature,
                    path,
                    principal,
                    minds_attest::NS_WITNESS,
                )
                .unwrap_or(false)
        })
    }
}

/// `--signers`, sonst `~/.ssh/allowed_signers` — nie aus der Repo-Konfiguration.
fn trusted_signers_file(signers: Option<&str>) -> Option<PathBuf> {
    if let Some(signers) = signers {
        return Some(PathBuf::from(signers));
    }
    let home = std::env::var_os("HOME")?;
    let default = Path::new(&home).join(".ssh/allowed_signers");
    default.is_file().then_some(default)
}

/// Ob `principal` ein Witness ist: Er kommt in `allowed_signers` vor, und
/// **jede** Zeile, die ihn nennt **oder** einen seiner Schlüssel trägt (unter
/// welchem Principal auch immer), ist auf genau `namespaces="minds-witness"`
/// beschränkt. Sonst könnte ein Schlüssel, der anderswo unbeschränkt steht
/// (ein Entwickler-Schlüssel im ssh-agent), Witness-Seals signieren.
///
/// Was sich nicht sicher lesen lässt — Principals in Anführungszeichen,
/// Muster (`*`, `?`, `!`), Zeilen ohne Schlüssel —, macht das Ergebnis
/// `false`. `principal` darf das ganze Principal-Feld sein, wie
/// `ssh-keygen -Y find-principals` es ausgibt.
fn witness_only(allowed_signers: &str, principal: &str) -> bool {
    struct Line {
        principals: String,
        restricted: bool,
        key: Option<String>,
    }
    let mut lines = Vec::new();
    for line in allowed_signers.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokens(line);
        let Some(principals) = tokens.first() else {
            continue;
        };

        let (options, rest) = match tokens.get(1) {
            Some(token) if !is_key_type(token) => (Some(token.as_str()), &tokens[2..]),
            _ => (None, &tokens[1.min(tokens.len())..]),
        };
        // Eine CA-Zeile ließe jedes Zertifikat dieser CA als Witness gelten.
        let restricted = options.is_some_and(|options| {
            !options
                .split(',')
                .any(|o| o.eq_ignore_ascii_case("cert-authority"))
                && options
                    .split(',')
                    .filter(|o| o.to_ascii_lowercase().starts_with("namespaces="))
                    .map(|o| &o["namespaces=".len()..])
                    .collect::<Vec<_>>()
                    == ["\"minds-witness\""]
        });
        let key = match rest {
            [kind, blob, ..] if is_key_type(kind) => Some(format!("{kind} {blob}")),
            _ => None,
        };
        lines.push(Line {
            principals: principals.clone(),
            restricted,
            key,
        });
    }
    // Eine Zeile „nennt" den Principal, wenn eines ihrer Muster ihn treffen
    // **könnte** (OpenSSH: `*`, `?`, Anführungszeichen; eine Negation zählt
    // vorsichtshalber nicht als Ausschluss) — dann gelten für sie dieselben
    // Regeln. Muster, die ihn nicht treffen können (`*@firma.de` für
    // Commit-Signaturen), sind ohne Belang, außer sie tragen seinen
    // Schlüssel.
    let names = |line: &Line| {
        line.principals == principal
            || line
                .principals
                .trim_matches('"')
                .split(',')
                .map(|p| p.trim_start_matches('!'))
                .any(|pattern| glob(pattern.as_bytes(), principal.as_bytes()))
    };
    let mut keys = Vec::new();
    for line in lines.iter().filter(|line| names(line)) {
        match &line.key {
            Some(key) if line.restricted => keys.push(key.clone()),
            _ => return false,
        }
    }
    !keys.is_empty()
        && lines
            .iter()
            .filter(|line| line.key.as_ref().is_some_and(|key| keys.contains(key)))
            .all(|line| line.restricted)
}

/// `*` und `?` wie in OpenSSH-Principal-Mustern.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&pattern[1..], text) || (!text.is_empty() && glob(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

fn is_key_type(token: &str) -> bool {
    token.starts_with("ssh-") || token.starts_with("ecdsa-") || token.starts_with("sk-")
}

/// Leerzeichen-getrennte Felder; Anführungszeichen schützen Leerzeichen.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample";
    const OTHER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOther";

    #[test]
    fn only_principals_restricted_to_the_witness_namespace_count() {
        let restricted = format!("w@host namespaces=\"minds-witness\" {KEY}");
        assert!(witness_only(&restricted, "w@host"));
        for line in [
            // Unbeschränkt: verifiziert unter jedem Namespace.
            format!("w@host {KEY}"),
            format!("w@host namespaces=\"minds,minds-witness\" {KEY}"),
            format!("w@host namespaces=\"minds\" {KEY}"),
            format!("w@host valid-after=\"20260101\" {KEY}"),
            // Muster und Anführungszeichen, die ihn treffen, zählen wie ein
            // Name — unbeschränkt also: kein Witness.
            format!("{restricted}\nw@* {OTHER}"),
            format!("{restricted}\n\"w@host\" {OTHER}"),
            format!("{restricted}\n!x@y,* {OTHER}"),
            // Ohne Schlüssel.
            "w@host namespaces=\"minds-witness\"".to_owned(),
        ] {
            assert!(!witness_only(&line, "w@host"), "{line}");
        }
        // Eine weitere, unbeschränkte Zeile desselben Principals …
        assert!(!witness_only(
            &format!("{restricted}\nw@host {OTHER}"),
            "w@host"
        ));
        // … oder derselbe Schlüssel unbeschränkt unter einem anderen
        // Principal (der Entwickler-Schlüssel im ssh-agent).
        assert!(!witness_only(
            &format!("human@x {KEY}\nhuman@x,w@host namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
        // Nicht genannt: kein Witness.
        assert!(!witness_only(&restricted, "other@host"));
        // Kommentare, andere Principals mit eigenen Schlüsseln, und das ganze
        // Feld, wie `find-principals` es ausgibt.
        let file = format!(
            "# Witness\nhuman@x {OTHER}\nw@host,w2@host namespaces=\"minds-witness\" {KEY}"
        );
        assert!(witness_only(&file, "w@host"));
        assert!(witness_only(&file, "w@host,w2@host"));
        assert!(!witness_only(
            &format!("w@host cert-authority,namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
        // Ein Muster für andere Principals (Commit-Signaturen) sperrt nichts.
        assert!(witness_only(
            &format!("*@firma.de namespaces=\"git\" {OTHER}\n{restricted}"),
            "w@host"
        ));
        assert!(witness_only(
            &format!("w@* namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
    }

    #[test]
    fn the_signers_file_never_comes_from_the_repository() {
        assert_eq!(
            trusted_signers_file(Some("/explicit")),
            Some(PathBuf::from("/explicit"))
        );
    }
}
