//! Regeln über die Signer-Datei (`allowed_signers`), die jeder Prüfer gleich
//! anwenden muss: welche Principals und Schlüssel auf genau einen Namespace
//! beschränkt sind. Bis Spec S2 lagen sie im Binary-Crate `minds-cli`; sie
//! stehen hier, damit es eine sicherheitskritische Regel nur einmal gibt
//! (Spec S2 §6). Die Module prüfen nur Text, sie starten keinen Prozess.

/// Die Schlüssel (SSH-Wire-Format) aller Zeilen, die auf genau
/// `namespaces="<namespace>"` beschränkt sind — eine **Obermenge** dessen,
/// was [`restricted_to`] gelten lässt. Sie sortiert ohne Prozess aus, was
/// sicher von keinem solchen Schlüssel stammt; ob eine Signatur gilt,
/// entscheidet weiter die volle Prüfung. `cert-authority`-Zeilen gelten
/// für `minds-anchor` nicht ([`restricted_to`] lehnt sie ab) — ein
/// CA-signierter Anker-Schlüssel wird nicht unterstützt.
pub fn restricted_keys(allowed_signers: &str, namespace: &str) -> Vec<Vec<u8>> {
    let only = format!("namespaces=\"{namespace}\"");
    let mut keys = Vec::new();
    for line in allowed_signers.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokens(line);
        let (Some(options), rest) = (match tokens.get(1) {
            Some(token) if !is_key_type(token) => (Some(token.as_str()), &tokens[2..]),
            _ => (None, &tokens[1.min(tokens.len())..]),
        }) else {
            continue;
        };
        let restricted = options
            .split(',')
            .filter(|o| o.to_ascii_lowercase().starts_with("namespaces="))
            .all(|o| o.eq_ignore_ascii_case(&only))
            && options
                .split(',')
                .any(|o| o.to_ascii_lowercase().starts_with("namespaces="));
        if let ([kind, blob, ..], true) = (rest, restricted)
            && let Some(key) = crate::public_key_blob(&format!("{kind} {blob}"))
        {
            keys.push(key);
        }
    }
    keys
}

/// Ob **jede** Zeile, die `principal` nennt oder einen seiner Schlüssel
/// trägt, auf genau `namespaces="<namespace>"` beschränkt ist.
pub fn restricted_to(allowed_signers: &str, principal: &str, namespace: &str) -> bool {
    let only = format!("\"{namespace}\"");
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
                    == [only.as_str()]
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

    /// Dieselben Fälle wie die Witness-Regel in `minds-cli`: Nur ein
    /// Principal, dessen jede Zeile (und jede Zeile seines Schlüssels) auf
    /// genau den Namespace beschränkt ist, zählt.
    #[test]
    fn only_principals_restricted_to_the_namespace_count() {
        let ns = crate::NS_WITNESS;
        let restricted = format!("w@host namespaces=\"minds-witness\" {KEY}");
        assert!(restricted_to(&restricted, "w@host", ns));
        for file in [
            format!("w@host {KEY}"),
            format!("w@host namespaces=\"minds,minds-witness\" {KEY}"),
            format!("w@host namespaces=\"minds\" {KEY}"),
            format!("{restricted}\nw@* {OTHER}"),
            format!("{restricted}\n\"w@host\" {OTHER}"),
            format!("{restricted}\n!x@y,* {OTHER}"),
            format!("{restricted}\nw@host {OTHER}"),
            format!("human@x {KEY}\nhuman@x,w@host namespaces=\"minds-witness\" {KEY}"),
            format!("w@host cert-authority,namespaces=\"minds-witness\" {KEY}"),
            "w@host namespaces=\"minds-witness\"".to_owned(),
        ] {
            assert!(!restricted_to(&file, "w@host", ns), "{file}");
        }
        assert!(!restricted_to(&restricted, "other@host", ns));
        assert!(!restricted_to(&restricted, "w@host", crate::NS_ANCHOR));
        assert!(restricted_to(
            &format!("*@firma.de namespaces=\"git\" {OTHER}\n{restricted}"),
            "w@host",
            ns
        ));
    }

    /// Der Vorfilter liefert nur Schlüssel aus Zeilen, die auf genau den
    /// Namespace beschränkt sind.
    #[test]
    fn only_keys_restricted_to_the_namespace_pass_the_prefilter() {
        let line = |n: u8| {
            let mut blob = Vec::new();
            blob.extend_from_slice(&11u32.to_be_bytes());
            blob.extend_from_slice(b"ssh-ed25519");
            blob.extend_from_slice(&32u32.to_be_bytes());
            blob.extend_from_slice(&[n; 32]);
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in blob.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                for i in 0..4 {
                    if i <= chunk.len() {
                        out.push(ALPHABET[(v >> (18 - 6 * i)) as usize & 63] as char);
                    } else {
                        out.push('=');
                    }
                }
            }
            (blob, format!("ssh-ed25519 {out}"))
        };
        let signers = format!(
            "ci@x namespaces=\"minds-anchor\" {}\ndev@x {}\nw@x namespaces=\"minds-witness\" {}\n",
            line(1).1,
            line(2).1,
            line(3).1
        );
        assert_eq!(
            restricted_keys(&signers, crate::NS_WITNESS),
            vec![line(3).0]
        );
        assert_eq!(restricted_keys(&signers, crate::NS_ANCHOR), vec![line(1).0]);
    }
}
