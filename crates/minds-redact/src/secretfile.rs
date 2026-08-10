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
//!
//! # Was das kostet, und warum es trotzdem bleibt
//!
//! `credentials.json` ist ein verbreiteter Name — auch für Test-Fixtures und
//! Ressourcenverzeichnisse. Eine `tests/fixtures/credentials.json` verschwindet
//! deshalb vollständig aus dem Record, obwohl nichts Echtes darin steht.
//!
//! Das ist der bewusste Preis. Die Alternative wäre eine Ausnahme über
//! Pfadteile wie `test/` — und die wäre selbst ein Loch, denn ein Verzeichnis
//! so zu benennen kostet nichts. Wer die Vorlage im Record braucht, hat den
//! vorgesehenen Weg: eine der Endungen aus [`TEMPLATE_SUFFIXES`]
//! (`credentials.json.example`).
//!
//! Zweite Kostenstelle, weniger offensichtlich: Für eine *produzierte* Datei
//! unterdrückt `minds-capture` den BLAKE3-Artefakt-Hash, sobald die Mauer
//! greift — absichtlich, weil ein Hash über eine kurze, ratbare Datei ein
//! Orakel wäre. Ein zu breiter Eintrag hier macht also auch die Audit-Kette
//! an einer Stelle dünner, an der man es nicht vermutet.

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
    // Verschlüsselt, aber nicht harmlos: Der Name sagt, was drinsteht, und der
    // Chiffretext ist im Record ohnehin wertlos.
    (".netrc.gpg", "netrc"),
    (".pgpass", "pgpass"),
    (".my.cnf", "mysql-config"),
    (".npmrc", "npmrc"),
    (".pypirc", "pypirc"),
    (".git-credentials", "git-credentials"),
    (".htpasswd", "htpasswd"),
    // Ohne führenden Punkt ist der Name genauso verbreitet — Apache legt die
    // Datei so neben die Konfiguration.
    ("htpasswd", "htpasswd"),
    ("credentials", "credentials-file"),
    // Bewusst der **generische** Grund: Unter diesem Namen legen die
    // Google-API-Quickstarts ein OAuth-*Client-Secret* ab, nicht den
    // Service-Account-Schlüssel (der heißt beim Download
    // `<projekt>-<hash>.json`). Beides gehört hinter die Mauer — aber der
    // Regelname landet als Begründung im Record, und dort soll nichts stehen,
    // was für die Hälfte der Fälle falsch ist.
    ("credentials.json", "credentials-file"),
    // Diese beiden sind eindeutig: ein **GCP-Service-Account-Schlüssel**, mit
    // vollständigem PEM-Private-Key im JSON-Feld `private_key` — mit literalen
    // `\n`, also genau in der Form, an der die PEM-Regel lange vorbeilief.
    ("service-account.json", "gcp-service-account"),
    ("service_account.json", "gcp-service-account"),
    // `gcloud auth application-default login` legt hier ein Refresh-Token ab.
    (
        "application_default_credentials.json",
        "gcp-application-default",
    ),
    ("kubeconfig", "kubeconfig"),
    ("id_rsa", "ssh-private-key"),
    ("id_dsa", "ssh-private-key"),
    ("id_ecdsa", "ssh-private-key"),
    ("id_ed25519", "ssh-private-key"),
    // FIDO-gebundene Schlüssel. Der private Teil ist zwar ohne Token nutzlos,
    // aber das ist eine Aussage über den *Angreifer*, nicht über den Inhalt —
    // und außerhalb von `~/.ssh/` greift die Verzeichnisregel nicht.
    ("id_ecdsa_sk", "ssh-private-key"),
    ("id_ed25519_sk", "ssh-private-key"),
    ("secrets.yaml", "secrets-file"),
    ("secrets.yml", "secrets-file"),
    ("secrets.json", "secrets-file"),
    ("terraform.tfvars", "terraform-vars"),
    // Docker im Legacy-Format. Trägt denselben base64-`auth` wie
    // `config.json` — und für den greift **kein** Detektor: `auth` steht
    // bewusst nicht in den Schlüsselregeln (es träfe `author`), und der Wert
    // liegt typisch unter der Entropie-Schwelle. Hier ist die Mauer die
    // einzige Schicht.
    (".dockercfg", "docker-config"),
    // Ansible-Vault-Passwortdateien: **eine Zeile, nur das Passwort**, ohne
    // Schlüsselnamen und oft ohne Entropie (`Sommer2024!`). Weder die
    // Zuweisungs- noch die Token- noch die Entropie-Regel hat hier etwas zu
    // greifen — auch das ist ein Fall, in dem nur die Mauer schützt.
    (".vault_pass", "ansible-vault-password"),
    (".vault-password", "ansible-vault-password"),
    ("vault_pass.txt", "ansible-vault-password"),
    ("vault-password.txt", "ansible-vault-password"),
    // rclone legt hier `pass = …` ab; `pass` steht nicht in den
    // Schlüsselregeln (dort nur `password|passwd|pwd|passphrase`).
    ("rclone.conf", "rclone-config"),
    (".rclone.conf", "rclone-config"),
    (".s3cfg", "s3cmd-config"),
    (".boto", "boto-config"),
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
///
/// WireGuard steht deshalb hier und nicht bei den Endungen: Der private
/// Schlüssel liegt in einer `.conf`, und `.conf` als Suffix aufzunehmen hieße,
/// jede Konfigurationsdatei im Repo auszulassen. Das Verzeichnis ist der
/// einzige Ort, an dem die Aussage trägt.
const SECRET_DIRECTORIES: &[(&str, &str)] = &[
    ("/.gnupg/", "gnupg"),
    ("/.ssh/", "ssh-directory"),
    ("/.config/gcloud/", "gcloud-config"),
    // Derselbe Ort unter Windows — `%APPDATA%\gcloud\` normalisiert sich zu
    // `/appdata/roaming/gcloud/` und enthält `/.config/gcloud/` gerade nicht.
    ("/appdata/roaming/gcloud/", "gcloud-config"),
    ("/etc/wireguard/", "wireguard"),
];

/// Warum dieser Pfad eine Zugangsdaten-Datei ist — oder `None`.
///
/// Der zurückgegebene Regelname ist für den Audit gedacht: Im Record soll
/// stehen, *weshalb* ein Inhalt ausgelassen wurde, nicht nur *dass*.
///
/// Groß-/Kleinschreibung wird im ASCII-Bereich ignoriert, `\` gilt wie `/`
/// (Windows-Pfade aus Tool-Calls).
pub fn secret_file_reason(path: &str) -> Option<&'static str> {
    let normalized = normalize(path);
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
        if ends_path_segment(&normalized, path) {
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
        // Auch ohne Schluss-Slash: `Grep`/`Glob` liefern das Verzeichnis selbst
        // als `path` (`/etc/wireguard`), und eine Grep-Antwort darüber trägt die
        // `PrivateKey`-Zeilen im Klartext.
        if normalized.contains(directory) || normalized.ends_with(directory.trim_end_matches('/')) {
            return Some(reason);
        }
    }

    None
}

/// Bringt einen Pfad in die Form, in der die Tabellen ihn erwarten.
///
/// `\` gilt wie `/` (Windows-Pfade aus Tool-Calls), Groß-/Kleinschreibung wird
/// im ASCII-Bereich ignoriert. Zusätzlich fallen `//` und `/./` weg: Beide sind
/// für das Dateisystem bedeutungslos, für ein `contains` aber ein Unterschied —
/// `~/.docker//config.json` hätte die Mauer sonst umgangen.
///
/// `..` wird **nicht** aufgelöst. Das ginge nur mit Kenntnis des
/// Arbeitsverzeichnisses, und dieses Modul hat bewusst kein I/O.
fn normalize(path: &str) -> String {
    let mut out = path.trim().replace('\\', "/").to_ascii_lowercase();
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    while out.contains("/./") {
        out = out.replace("/./", "/");
    }
    out
}

/// Kurzform von [`secret_file_reason`].
pub fn is_secret_file(path: &str) -> bool {
    secret_file_reason(path).is_some()
}

/// Endungen, die aus einem Credential-Pfad ein **anderes Artefakt** machen.
///
/// Der Unterschied, um den es geht: `~/.aws/credentials.bak` trägt byte-genau
/// dieselben Schlüssel wie das Original — `~/.aws/credentials-helper.sh` ist
/// ein Skript, das sie allenfalls *ausgibt*. Nur die zweite Sorte darf durch.
///
/// Die Liste ist bewusst eine **Denylist der Ausnahmen** und keine Allowlist
/// der erlaubten Dekorationen: Wer sie erweitert, macht die Mauer löchriger und
/// muss das begründen. Eine Allowlist hätte den umgekehrten Fehler — sie
/// verliert stillschweigend jede Form, an die niemand gedacht hat
/// (`config-prod`, `credentials 2`, `config.json.orig`).
const NON_CREDENTIAL_SUFFIXES: &[&str] = &[
    // Programme, die Zugangsdaten *benutzen*
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".ps1",
    ".bat",
    ".cmd",
    ".py",
    ".rb",
    ".pl",
    ".js",
    ".ts",
    // Dokumentation über Zugangsdaten
    ".md",
    ".txt",
    ".rst",
    ".adoc",
    ".html",
    // Vorlagen, aus denen erst noch eine Datei wird
    ".tmpl",
    ".tpl",
    ".j2",
    ".jinja",
    ".mustache",
    ".gotmpl",
    ".schema",
    ".schema.json",
];

/// Ob `needle` im Pfad steht und der Rest dahinter **noch dieselbe Datei
/// meint**.
///
/// Ein schlichtes `contains` reicht nicht: `/.aws/credentials` steht auch in
/// `/.aws/credentials-helper.sh`, und ein Hilfsskript ist keine Zugangsdatei.
/// Die Mauer ersetzt den **gesamten** Inhalt — ein Fehlalarm kostet hier eine
/// ganze Datei im Record.
///
/// # Warum nicht einfach „Rest muss leer oder `/` sein"
///
/// Weil das die Mauer an der falschen Stelle schließt. Gemessen fielen damit
/// durch: `credentials.bak`, `credentials.old`, `credentials~`,
/// `credentials 2` (macOS-Kopie), `config.json.orig` — und `config-prod`,
/// `config.yaml`, `config_eks`, also die übliche `KUBECONFIG`-Konvention.
/// Diese Dateien tragen dieselben Live-Zugangsdaten wie das Original; ein
/// `.bak` entsteht gerade dann, wenn jemand eine Credential-Datei
/// „vorsichtshalber sichert", bevor er sie ändert.
///
/// Der Fehlalarm, den die enge Regel beseitigt, kostet Kontext. Der Fehlpass,
/// den sie erzeugt, kostet Zugangsdaten — und beim Docker-Fall greift danach
/// **kein einziger** Detektor mehr: `auth` steht bewusst nicht in den
/// Schlüsselregeln (es träfe `author`), und der base64-Wert liegt mit 16
/// Zeichen unter der Entropie-Schwelle.
///
/// Deshalb umgekehrt: Der Rest disqualifiziert nur, wenn er die Datei zu etwas
/// anderem macht ([`NON_CREDENTIAL_SUFFIXES`]).
fn ends_path_segment(haystack: &str, needle: &str) -> bool {
    let matches_here = |rest: &str| {
        rest.is_empty()
            || rest.starts_with('/')
            || !NON_CREDENTIAL_SUFFIXES.iter().any(|s| rest.ends_with(s))
    };

    // Der führende `/` der Muster verankert sie an einer Segmentgrenze. Ein
    // repo-relativer Pfad (`\.kube/config`, wie ihn Tool-Calls oft liefern) hat
    // ihn nicht — deshalb zusätzlich der Anfang.
    if let Some(without_slash) = needle.strip_prefix('/') {
        if let Some(rest) = haystack.strip_prefix(without_slash) {
            if matches_here(rest) {
                return true;
            }
        }
    }

    haystack.match_indices(needle).any(|(start, _)| {
        // `start + needle.len()` liegt am Ende eines Treffers, also auf einer
        // Zeichengrenze — `haystack[end..]` kann nicht panicken.
        matches_here(&haystack[start + needle.len()..])
    })
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
            // Dieselbe Ausnahme muss für die neu aufgenommenen Namen gelten —
            // sonst verschwindet die Vorlage, an der der Reviewer sieht,
            // *welche* Felder eine Datei hat.
            "credentials.json.example",
            "service-account.json.sample",
            "id_ed25519_sk.pub",
            "htpasswd.example",
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
    fn cloud_service_account_keys_are_walled_off() {
        // Ein GCP-Service-Account-Schlüssel trägt einen vollständigen
        // PEM-Private-Key im JSON — mit literalen `\n`. Genau die Form, die
        // ohne die Mauer am Entropie-Netz hinge.
        for (path, reason) in [
            // Generischer Grund: Unter diesem Namen liegt meist ein
            // OAuth-Client-Secret, nicht der Service-Account-Schlüssel. Beides
            // gehört hinter die Mauer — aber der Grund wandert in den Record
            // und soll dort stimmen.
            ("/home/p/projekt/credentials.json", "credentials-file"),
            ("/home/p/service-account.json", "gcp-service-account"),
            ("/home/p/service_account.json", "gcp-service-account"),
            (
                "/home/p/.config/gcloud/application_default_credentials.json",
                "gcp-application-default",
            ),
            // Ein *unbekannter* Name im gcloud-Verzeichnis fällt auf den
            // Verzeichnis-Grund zurück.
            (
                "/home/p/.config/gcloud/legacy_credentials/x/adc.json",
                "gcloud-config",
            ),
        ] {
            assert_eq!(secret_file_reason(path), Some(reason), "verpasst: {path}");
        }
    }

    #[test]
    fn fido_ssh_keys_outside_the_ssh_directory() {
        // Außerhalb von `~/.ssh/` greift die Verzeichnisregel nicht — der Name
        // muss selbst tragen.
        for path in [
            "/home/p/keys/id_ed25519_sk",
            "/home/p/keys/id_ecdsa_sk",
            "C:\\Users\\P\\keys\\id_ed25519_sk",
        ] {
            assert_eq!(
                secret_file_reason(path),
                Some("ssh-private-key"),
                "verpasst: {path}"
            );
        }
    }

    #[test]
    fn htpasswd_and_encrypted_netrc() {
        assert_eq!(
            secret_file_reason("/etc/apache2/htpasswd"),
            Some("htpasswd")
        );
        assert_eq!(secret_file_reason("/home/p/.netrc.gpg"), Some("netrc"));
    }

    #[test]
    fn a_wireguard_config_is_walled_off_by_its_directory() {
        // `.conf` als Endung aufzunehmen hieße, jede Konfigurationsdatei
        // auszulassen — deshalb das Verzeichnis.
        assert_eq!(
            secret_file_reason("/etc/wireguard/wg0.conf"),
            Some("wireguard")
        );
        assert_eq!(secret_file_reason("/etc/nginx/nginx.conf"), None);
        assert_eq!(secret_file_reason("src/config/app.conf"), None);
    }

    #[test]
    fn a_path_rule_ignores_programs_and_templates() {
        // `contains` allein träfe auch das Hilfsskript daneben. Die Mauer
        // ersetzt den **ganzen** Inhalt — ein Fehlalarm kostet hier eine
        // komplette Datei im Record, nicht nur einen Wert.
        for path in [
            "/home/p/.aws/credentials-helper.sh",
            "/home/p/.aws/credentials.py",
            "/home/p/.kube/config.tmpl",
            "/home/p/.kube/config-generator.py",
            "/home/p/.docker/config.json.j2",
            "/home/p/.kube/config.md",
        ] {
            assert_eq!(
                secret_file_reason(path),
                None,
                "fälschlich geschwärzt: {path}"
            );
        }
    }

    #[test]
    fn a_copy_of_a_credential_file_is_still_one() {
        // Der teure Fehler wäre andersherum: Ein `.bak` trägt byte-genau
        // dieselben Schlüssel wie das Original und entsteht gerade dann, wenn
        // jemand eine Credential-Datei vor dem Ändern sichert.
        //
        // Beim Docker-Fall greift danach **kein** Detektor mehr: `auth` steht
        // bewusst nicht in den Schlüsselregeln, und der base64-Wert liegt unter
        // der Entropie-Schwelle. Die Mauer ist dort die einzige Schicht.
        for (path, reason) in [
            ("/home/p/.aws/credentials", "aws-credentials"),
            ("/home/p/.aws/credentials.bak", "aws-credentials"),
            ("/home/p/.aws/credentials.old", "aws-credentials"),
            ("/home/p/.aws/credentials~", "aws-credentials"),
            ("/home/p/.aws/credentials 2", "aws-credentials"),
            ("/home/p/.kube/config.bak", "kubeconfig"),
            // Die übliche KUBECONFIG-Konvention, keine Randerscheinung.
            ("/home/p/.kube/config-prod", "kubeconfig"),
            ("/home/p/.kube/config.yaml", "kubeconfig"),
            ("/home/p/.kube/config_eks", "kubeconfig"),
            ("/home/p/.docker/config.json.bak", "docker-config"),
            ("/home/p/.docker/config.json.orig", "docker-config"),
        ] {
            assert_eq!(secret_file_reason(path), Some(reason), "verpasst: {path}");
        }
    }

    #[test]
    fn relative_and_untidy_paths_do_not_slip_through() {
        // Tool-Calls liefern regelmäßig repo-relative Pfade, und `//` bzw.
        // `/./` sind für das Dateisystem bedeutungslos — für ein `contains`
        // aber ein Unterschied.
        for (path, reason) in [
            (".kube/config", "kubeconfig"),
            (".docker/config.json", "docker-config"),
            ("/home/p/.docker//config.json", "docker-config"),
            ("/home/p/.kube/./config", "kubeconfig"),
            ("/etc/./wireguard/wg0.conf", "wireguard"),
        ] {
            assert_eq!(secret_file_reason(path), Some(reason), "verpasst: {path}");
        }
    }

    #[test]
    fn a_directory_without_its_trailing_slash_still_counts() {
        // `Grep`/`Glob` liefern das Verzeichnis selbst als `path`, und eine
        // Grep-Antwort darüber trägt die Schlüssel im Klartext.
        assert_eq!(secret_file_reason("/etc/wireguard"), Some("wireguard"));
        assert_eq!(secret_file_reason("/home/p/.ssh"), Some("ssh-directory"));
    }

    #[test]
    fn files_that_only_the_wall_can_protect() {
        // Für diese Klasse hat das Netz nichts zu greifen: nacktes Passwort
        // ohne Schlüsselnamen, `auth`/`pass` als bewusst ausgenommene
        // Schlüssel, Werte unter der Entropie-Schwelle.
        for (path, reason) in [
            ("/home/p/.vault_pass", "ansible-vault-password"),
            ("/home/p/ansible/vault_pass.txt", "ansible-vault-password"),
            ("/home/p/.dockercfg", "docker-config"),
            ("/home/p/.config/rclone/rclone.conf", "rclone-config"),
            ("/home/p/.s3cfg", "s3cmd-config"),
        ] {
            assert_eq!(secret_file_reason(path), Some(reason), "verpasst: {path}");
        }
    }

    #[test]
    fn ordinary_json_files_are_not_credential_files() {
        // Der Negativ-Zwilling zu den neuen JSON-Namen: Die Mauer lässt ganze
        // Dateien verschwinden, deshalb muss belegt sein, dass die Nachbarn
        // bleiben. Schützt auch gegen ein künftiges Aufweichen zu
        // `*credentials*.json`.
        for path in [
            "package.json",
            "tsconfig.json",
            "composer.json",
            "android/app/google-services.json",
            "src/fixtures/credentials.schema.json",
        ] {
            assert_eq!(
                secret_file_reason(path),
                None,
                "fälschlich geschwärzt: {path}"
            );
        }
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
