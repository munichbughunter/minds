//! Erkennung von **Zugangsdaten-Dateien** an ihrem Pfad — die Mauer vor dem
//! Netz.
//!
//! Die Detektoren in [`crate::secret`], [`crate::pii`] und
//! [`crate::assignment`] sind ein *Netz*: Sie fangen, was sie erkennen. Für
//! eine Datei, deren einziger Zweck es ist, Zugangsdaten zu enthalten, ist ein
//! Netz die falsche Bauform. Wenn ein Agent `cat .env` ausführt und der Inhalt
//! im Transkript landet, wollen wir nicht *möglichst viel* davon schwärzen —
//! wir wollen **nichts davon aufheben**.
//!
//! Deshalb dieser zweite, gröbere Mechanismus: Ist der Pfad eine bekannte
//! Zugangsdaten-Datei, wird ihr Inhalt **gar nicht erst gescannt**, sondern
//! vollständig durch [`SECRET_FILE_PLACEHOLDER`] ersetzt. Kein Teilerhalt, kein
//! Rest, keine Diskussion über Erkennungsraten.
//!
//! # Warum das die eigentliche Antwort ist
//!
//! Ein Detektor kann `DB_PASSWORD=hunter2` fangen. Er kann nicht fangen, was er
//! nicht als Zuweisung erkennt — einen fortgesetzten PEM-Block, ein
//! selbstgebautes Format, einen Kommentar `# altes PW war hunter2`. Bei einer
//! `.env` ist die einzige belastbare Aussage: *alles hier drin ist verdächtig.*
//!
//! # Wo das durchgesetzt wird
//!
//! **Nicht hier.** Dieses Modul liefert nur das Prädikat; es hat kein I/O und
//! liest keine Dateien. Angewandt wird es in `minds-capture` (M5): Beim
//! Übersetzen eines Tool-Calls in einen [`Turn`](minds_core::Turn) prüft der
//! Adapter den Pfad-Parameter — und ersetzt das Ergebnis der Datei-Lesung
//! komplett, statt es der Pipeline zu übergeben.
//!
//! # Was bewusst *nicht* als Geheimnis gilt
//!
//! `.env.example`, `.env.sample`, `.env.template`, `.env.dist` und jede
//! `*.pub`-Datei. Das sind die eingecheckten Platzhalter-Dateien — sie zu
//! schwärzen kostet echten Kontext (welche Variablen existieren überhaupt) und
//! schützt nichts. Ihr Inhalt läuft trotzdem durch die normale Pipeline: Wer
//! aus Versehen ein echtes Passwort in die `.env.example` schreibt, ist über
//! [`KeyValueRedactor`](crate::KeyValueRedactor) weiterhin abgedeckt. Mauer für
//! die echte Datei, Netz für die Kopie.

/// Ersatz für den Inhalt einer Zugangsdaten-Datei.
///
/// Bewusst anders als die Kategorie-Platzhalter aus
/// [`Category::placeholder`](crate::Category::placeholder): Der Reader soll
/// „ganze Datei ausgelassen" von „einzelner Wert geschwärzt" unterscheiden
/// können.
pub const SECRET_FILE_PLACEHOLDER: &str = "[omitted:secret-file]";

/// Endungen, die eine Datei trotz passendem Namen als **Vorlage** ausweisen.
const TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template", ".dist", ".pub"];

/// Dateinamen, die für sich genommen öffentlich sind.
const PUBLIC_BASENAMES: &[&str] = &["known_hosts", "authorized_keys"];

/// Exakte Dateinamen ⇒ Regelname.
const SECRET_BASENAMES: &[(&str, &str)] = &[
    (".env", "dotenv"),
    (".envrc", "direnv"),
    (".netrc", "netrc"),
    ("_netrc", "netrc"),
    (".pgpass", "pgpass"),
    (".my.cnf", "mysql-config"),
    (".npmrc", "npmrc"),
    (".pypirc", "pypirc"),
    (".git-credentials", "git-credentials"),
    (".htpasswd", "htpasswd"),
    ("credentials", "credentials-file"),
    ("kubeconfig", "kubeconfig"),
    ("id_rsa", "ssh-private-key"),
    ("id_dsa", "ssh-private-key"),
    ("id_ecdsa", "ssh-private-key"),
    ("id_ed25519", "ssh-private-key"),
    ("secrets.yaml", "secrets-file"),
    ("secrets.yml", "secrets-file"),
    ("secrets.json", "secrets-file"),
    ("terraform.tfvars", "terraform-vars"),
];

/// Dateiendungen ⇒ Regelname.
const SECRET_SUFFIXES: &[(&str, &str)] = &[
    (".env", "dotenv"),
    (".pem", "private-key"),
    (".key", "private-key"),
    (".p12", "keystore"),
    (".pfx", "keystore"),
    (".jks", "keystore"),
    (".keystore", "keystore"),
    (".kdbx", "password-database"),
    (".tfvars", "terraform-vars"),
    (".ovpn", "vpn-profile"),
];

/// Vollständige Pfad-Enden ⇒ Regelname.
///
/// Werden **vor** den Basenamen geprüft, weil sie spezifischer sind:
/// `/.aws/credentials` soll als `aws-credentials` im Audit stehen, nicht als
/// generisches `credentials-file`.
const SECRET_PATHS: &[(&str, &str)] = &[
    ("/.aws/credentials", "aws-credentials"),
    ("/.kube/config", "kubeconfig"),
    ("/.docker/config.json", "docker-config"),
];

/// Verzeichnisse, deren Inhalt pauschal als Zugangsdaten gilt.
///
/// Werden **zuletzt** geprüft — das Gegenstück zur Regel oben: Ein bekannter
/// Dateiname behält seinen genaueren Grund (`.ssh/id_ed25519` ⇒
/// `ssh-private-key`), und erst ein *unbekannter* Name im selben Verzeichnis
/// fällt auf den Verzeichnis-Grund zurück.
const SECRET_DIRECTORIES: &[(&str, &str)] = &[("/.gnupg/", "gnupg"), ("/.ssh/", "ssh-directory")];

/// Warum dieser Pfad eine Zugangsdaten-Datei ist — oder `None`.
///
/// Der zurückgegebene Regelname ist für den Audit gedacht: Im Record soll
/// stehen, *weshalb* ein Inhalt ausgelassen wurde, nicht nur *dass*.
///
/// Groß-/Kleinschreibung wird im ASCII-Bereich ignoriert, `\` gilt wie `/`
/// (Windows-Pfade aus Tool-Calls).
pub fn secret_file_reason(path: &str) -> Option<&'static str> {
    let normalized = path.trim().replace('\\', "/").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
    if basename.is_empty() {
        return None; // Verzeichnis-Pfad, keine Datei.
    }

    // Ausnahmen zuerst — sonst würde `.env.example` über die `.env.`-Regel
    // greifen und `id_rsa.pub` über `id_rsa`.
    if TEMPLATE_SUFFIXES.iter().any(|s| basename.ends_with(s))
        || PUBLIC_BASENAMES.contains(&basename)
    {
        return None;
    }

    // Reihenfolge = Spezifität. Erst der volle Pfad, dann der Dateiname, zuletzt
    // das Verzeichnis — so trägt jeder Fund den genauesten verfügbaren Grund.
    for &(path, reason) in SECRET_PATHS {
        if normalized.contains(path) {
            return Some(reason);
        }
    }
    for &(name, reason) in SECRET_BASENAMES {
        if name == basename {
            return Some(reason);
        }
    }
    // `.env.local`, `.env.production`, `.env.ci`
    if basename.starts_with(".env.") {
        return Some("dotenv");
    }
    for &(suffix, reason) in SECRET_SUFFIXES {
        if basename.ends_with(suffix) {
            return Some(reason);
        }
    }
    for &(directory, reason) in SECRET_DIRECTORIES {
        if normalized.contains(directory) {
            return Some(reason);
        }
    }

    None
}

/// Kurzform von [`secret_file_reason`].
pub fn is_secret_file(path: &str) -> bool {
    secret_file_reason(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_in_all_its_forms() {
        for path in [
            ".env",
            "./.env",
            "/home/claude/projekt/.env",
            ".env.local",
            ".env.production",
            "config/prod.env",
            "C:\\Users\\Patrick\\projekt\\.env",
        ] {
            assert_eq!(secret_file_reason(path), Some("dotenv"), "verpasst: {path}");
        }
    }

    #[test]
    fn template_files_are_not_secret() {
        // Sie tragen keine echten Werte und sind für den Reviewer nützlich —
        // ihr Inhalt läuft trotzdem durch die Detektoren.
        for path in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "id_rsa.pub",
            "deploy_key.pub",
        ] {
            assert!(!is_secret_file(path), "fälschlich geschwärzt: {path}");
        }
    }

    #[test]
    fn private_keys_and_keystores() {
        assert_eq!(secret_file_reason("certs/server.pem"), Some("private-key"));
        assert_eq!(secret_file_reason("tls/server.key"), Some("private-key"));
        assert_eq!(secret_file_reason("app.p12"), Some("keystore"));
        assert_eq!(
            secret_file_reason("/home/p/.ssh/id_ed25519"),
            Some("ssh-private-key")
        );
    }

    #[test]
    fn tool_and_cloud_credential_files() {
        assert_eq!(secret_file_reason("~/.npmrc"), Some("npmrc"));
        assert_eq!(secret_file_reason("/root/.netrc"), Some("netrc"));
        assert_eq!(
            secret_file_reason("/home/p/.aws/credentials"),
            Some("aws-credentials")
        );
        assert_eq!(
            secret_file_reason("/home/p/.kube/config"),
            Some("kubeconfig")
        );
        assert_eq!(
            secret_file_reason("/home/p/.docker/config.json"),
            Some("docker-config")
        );
        assert_eq!(
            secret_file_reason(".git-credentials"),
            Some("git-credentials")
        );
    }

    #[test]
    fn ssh_directory_catches_unknown_key_names() {
        // Ein selbstbenannter Schlüssel im .ssh-Verzeichnis ist trotzdem einer.
        assert_eq!(
            secret_file_reason("/home/p/.ssh/gitlab_deploy"),
            Some("ssh-directory")
        );
        // …der öffentliche Teil aber nicht.
        assert!(!is_secret_file("/home/p/.ssh/gitlab_deploy.pub"));
        assert!(!is_secret_file("/home/p/.ssh/known_hosts"));
    }

    #[test]
    fn ordinary_source_files_are_not_secret() {
        for path in [
            "src/retry.rs",
            "Cargo.toml",
            "README.md",
            "crates/minds-redact/src/config.rs",
            ".gitlab-ci.yml",
            "docs/environment.md",
            "config/settings.yaml",
        ] {
            assert!(!is_secret_file(path), "Fehlalarm auf {path}");
        }
    }

    #[test]
    fn case_and_separators_do_not_matter() {
        assert!(is_secret_file(".ENV"));
        assert!(is_secret_file("C:/Users/P/.aws/credentials"));
        assert!(is_secret_file("C:\\Users\\P\\.aws\\credentials"));
    }

    #[test]
    fn empty_and_directory_paths() {
        assert!(!is_secret_file(""));
        assert!(!is_secret_file("   "));
        assert!(!is_secret_file("/home/claude/"));
    }

    #[test]
    fn placeholder_is_distinguishable_from_value_redaction() {
        assert_eq!(SECRET_FILE_PLACEHOLDER, "[omitted:secret-file]");
        assert_ne!(
            SECRET_FILE_PLACEHOLDER,
            crate::Category::Secret.placeholder()
        );
    }
}
