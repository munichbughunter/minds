//! Der geschützte Witness (Durchlauf 2c): `minds enable` richtet ihn mit
//! **einer** Erhöhung ein — der Witness läuft danach unter eigenem Nutzer
//! (`_minds` auf macOS, `minds-witness` unter Linux) als Systemdienst, sein
//! Binary liegt root-eigen unter [`BINARY`], sein Home außerhalb jedes
//! Nutzer-Homes ([`base`]). Damit kann er A2 tragen (ADR-0012, Addendum
//! „Witness facts").
//!
//! # Ablauf
//!
//! 1. **Ohne Rechte, als Entwickler:** das Repo vorbereiten
//!    ([`prepare_repo`]): nur `core.sharedRepository=group` und die
//!    Verzeichnisse `.git/logs`, `.git/minds` — kein Recht wird erweitert.
//!    Gruppe und Modi setzt erst das Skript (Schritt 2): Was der Witness
//!    schreibt (`objects/`, `refs/`, `logs/`, `minds/` ohne das Journal),
//!    wird gruppenschreibbar, `.git` bekommt das Sticky-Bit. Die Gruppe ist
//!    eine eigene (`_minds` / `minds-witness`) mit genau zwei Mitgliedern:
//!    dem Witness und dem Entwickler — nie dessen primäre Gruppe.
//! 2. **Einmal `sudo`:** ein Skript, das minds selbst erzeugt
//!    (`templates/witness/protected-*.sh`, jeder Wert in einfachen
//!    Anführungszeichen, Systemprogramme mit absolutem Pfad): Binary
//!    kopieren, Nutzer und Gruppe anlegen; was im Repo zu tun ist (Gruppe
//!    wechseln, ACL für das Durchqueren), tut es **als Entwickler**
//!    (`sudo -u`), nie als root; Home einrichten (`witness init --profile
//!    user`, `keygen` als Witness), Systemdienst installieren und starten,
//!    schlanke Managed Settings (nur Deny-Regeln, nur wenn noch keine
//!    existieren — sonst ein Vorschlag daneben).
//! 3. **Danach:** auf den Witness warten, den ungeschützten Nutzerdienst
//!    dieses Repos anhalten (die Hooks finden ab jetzt den geschützten), die
//!    Signer-Zeile ausgeben.
//!
//! Die Hooks finden den geschützten Witness ohne Variable ([`discover`]):
//! am festen Ort unter [`base`], nur wenn dessen Verzeichniskette root
//! gehört und das Home einem anderen Nutzer als dem Agenten — und beim
//! Verbinden nur, wenn die Gegenstelle genau dieser Nutzer ist.

use std::path::{Path, PathBuf};

/// Das Binary des geschützten Witness — root-eigen, außerhalb jedes
/// Nutzer-Homes (macOS: `/Users/*` trägt eine ACL, die jeden Schutzbefund
/// verhindert).
pub(crate) const BINARY: &str = "/usr/local/libexec/minds/minds";

/// Der Nutzer des geschützten Witness.
#[cfg(target_os = "macos")]
pub(crate) const WITNESS_USER: &str = "_minds";
#[cfg(not(target_os = "macos"))]
pub(crate) const WITNESS_USER: &str = "minds-witness";

#[cfg(target_os = "macos")]
const BASE: &str = "/private/var/db/minds-witness";
#[cfg(not(target_os = "macos"))]
const BASE: &str = "/var/lib/minds-witness";

#[cfg(target_os = "macos")]
const MANAGED_DIR: &str = "/Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
const MANAGED_DIR: &str = "/etc/claude-code";

/// Das Programm für die eine Erhöhung.
const ELEVATE: &str = "/usr/bin/sudo";

/// Die Zeilen, an denen `enable` die Ausgabe des Skripts erkennt.
const SIGNERS_MARK: &str = "MINDS-SIGNERS ";
const MANAGED_MARK: &str = "MINDS-MANAGED ";
const NOTE_MARK: &str = "MINDS-NOTE ";
const INTENT_KEY_MARK: &str = "MINDS-INTENT-KEY ";

/// Wo die Homes geschützter Witnesses liegen (je Repo `<base>/<repo-id>`).
/// Nur in Debug-Builds (Tests) über `MINDS_PROTECTED_BASE` verlegbar.
pub(crate) fn base() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(base) = std::env::var_os("MINDS_PROTECTED_BASE").filter(|b| !b.is_empty()) {
        return PathBuf::from(base);
    }
    PathBuf::from(BASE)
}

/// Das Home des geschützten Witness für `root` (kanonisch).
pub(crate) fn home_of(root: &Path) -> PathBuf {
    base().join(crate::enable_witness::repo_id(root))
}

/// Was die Einrichtung ergab.
pub(crate) struct Installed {
    pub(crate) socket: PathBuf,
    /// Ob der Witness schon antwortet — ein langsamer erster Start ist kein
    /// Fehlschlag (der Dienst ist eingerichtet).
    pub(crate) answering: bool,
    /// Die `allowed_signers`-Zeile des Witness-Schlüssels.
    pub(crate) signers: Option<String>,
    /// `installed <path>`, `updated <path>`, `unchanged <path>` oder
    /// `proposed <path>`.
    pub(crate) managed: Option<String>,
    /// Hinweise des Skripts (etwa: einmal neu anmelden).
    pub(crate) notes: Vec<String>,
    /// Was der Witness über den Freigabe-Schlüssel sagt (festgehalten,
    /// oder ein anderer nicht übernommen).
    pub(crate) intent_key: Vec<String>,
}

/// Fragt genau einmal auf dem Terminal. Leere Antwort heißt ja; das Ende
/// der Eingabe (Ctrl-D) oder ein Lesefehler heißen nein.
pub(crate) fn ask() -> bool {
    use std::io::{BufRead, Write};
    let mut stderr = std::io::stderr();
    let _ = write!(
        stderr,
        "Set up the witness protected (administrator rights, once)? [Y/n] "
    );
    let _ = stderr.flush();
    let mut answer = String::new();
    match std::io::stdin().lock().read_line(&mut answer) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(
            answer.trim().to_lowercase().as_str(),
            "" | "y" | "yes" | "j" | "ja"
        ),
    }
}

/// Ob Ein- und Ausgabe ein Terminal sind — sonst wird nie gefragt.
pub(crate) fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Die Kennung des Witness-Nutzers, falls es ihn gibt.
fn witness_uid() -> Option<u32> {
    let name = std::ffi::CString::new(WITNESS_USER).ok()?;
    // SAFETY: `name` ist NUL-terminiert; getpwnam liefert NULL oder einen
    // Zeiger auf eine statische Struktur, aus der sofort kopiert wird.
    unsafe {
        let entry = libc::getpwnam(name.as_ptr());
        (!entry.is_null()).then(|| (*entry).pw_uid)
    }
}

/// Der geschützte Witness dieses Repos, wie die Hooks ihn finden: sein
/// Socket und die Kennung, die die Gegenstelle haben muss. `None`, wenn
/// keiner eingerichtet ist oder der Ort nicht vertrauenswürdig ist.
pub(crate) fn discover(root: &Path) -> Option<(PathBuf, u32)> {
    use std::os::unix::fs::FileTypeExt;
    let (home, uid) = trusted_home(root)?;
    let socket = home.join("run").join("witness.sock");
    std::fs::symlink_metadata(&socket)
        .ok()
        .filter(|meta| meta.file_type().is_socket())
        .map(|_| (socket, uid))
}

/// Das Home des geschützten Witness dieses Repos und die Kennung seines
/// Nutzers — wenn der Ort vertrauenswürdig ist ([`Place`]), gleich ob sein
/// Socket gerade da ist (ein angehaltener Dienst räumt ihn weg).
pub(crate) fn trusted_home(root: &Path) -> Option<(PathBuf, u32)> {
    use std::os::unix::fs::MetadataExt;
    let base = base();
    let home = base.join(crate::enable_witness::repo_id(root));
    let base_meta = std::fs::symlink_metadata(&base).ok()?;
    let home_meta = std::fs::symlink_metadata(&home).ok()?;
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    let place = Place {
        chain: base.parent().is_some_and(root_chain),
        base_dir: base_meta.is_dir(),
        base_uid: base_meta.uid(),
        base_mode: base_meta.mode(),
        home_dir: home_meta.is_dir(),
        home_uid: home_meta.uid(),
    };
    if !place.trusted(euid, witness_uid()?) {
        return None;
    }
    Some((home, home_meta.uid()))
}

/// Was [`discover`] über den Ort prüft — ohne Dateisystem, testbar.
#[derive(Debug, Clone, Copy)]
struct Place {
    /// Jedes Verzeichnis über der Basis gehört root und ist für niemanden
    /// sonst beschreibbar.
    chain: bool,
    base_dir: bool,
    base_uid: u32,
    base_mode: u32,
    home_dir: bool,
    home_uid: u32,
}

impl Place {
    /// Basis und Home gehören dem Witness-Nutzer (`witness`) — nicht dem
    /// Agenten, nicht root —, die Basis ist für niemanden sonst beschreibbar,
    /// und darüber gehört alles root. Einen solchen Ort kann der Agent nicht
    /// anlegen.
    fn trusted(self, euid: u32, witness: u32) -> bool {
        self.chain
            && self.base_dir
            && self.home_dir
            && self.base_uid == witness
            && self.home_uid == witness
            && witness != euid
            && witness != 0
            && self.base_mode & 0o022 == 0
    }
}

/// Ob `path` und jedes Verzeichnis darüber root gehören und für niemanden
/// sonst beschreibbar sind (ein root-eigener Symlink wie `/var` zählt mit
/// seinem Eigentümer).
fn root_chain(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    path.ancestors().all(|node| {
        std::fs::symlink_metadata(node).is_ok_and(|meta| {
            meta.uid() == 0
                && (meta.file_type().is_symlink()
                    || (meta.mode() & 0o022 == 0
                        && !crate::verify_cmd::conformance::carries_acl(node)))
        })
    })
}

/// Ob das Binary des geschützten Witness ein anderes ist als das laufende —
/// nach einem Update laufen Hooks und Witness sonst auseinander.
pub(crate) fn binary_differs() -> bool {
    let digest = |path: &Path| std::fs::read(path).ok().map(|bytes| blake3::hash(&bytes));
    let running = std::env::current_exe().ok().and_then(|exe| digest(&exe));
    match (running, digest(Path::new(BINARY))) {
        (Some(running), Some(installed)) => running != installed,
        _ => false,
    }
}

/// Orte, deren Inhalt macOS (TCC) einem Systemdienst ohne „Festplattenvoll-
/// zugriff" vorenthält — unabhängig von den Dateirechten.
const PRIVACY_PROTECTED: &[&str] = &[
    "Pictures",
    "Desktop",
    "Documents",
    "Downloads",
    "Library/Mobile Documents",
    "Library/CloudStorage",
];

/// Ob `root` an einem solchen Ort liegt (nur macOS) — oder auf einem
/// Wechsel- oder Netzwerklaufwerk (`/Volumes/…`), das TCC ebenso schützt.
fn privacy_protected(root: &Path, home: Option<&Path>) -> bool {
    cfg!(target_os = "macos")
        && (root.starts_with("/Volumes")
            || home.is_some_and(|home| {
                PRIVACY_PROTECTED
                    .iter()
                    .any(|place| root.starts_with(home.join(place)))
            }))
}

/// Richtet den geschützten Witness für `root` ein (siehe Modul-Doku).
pub(crate) fn install(
    root: &Path,
    store: &minds_store::StoreConfig,
    verbose: bool,
) -> std::io::Result<Installed> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(other(
            "the protected witness is set up on macOS and Linux only",
        ));
    }
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        return Err(other(
            "minds enable runs as root — run it as the developer; it asks for administrator \
             rights itself",
        ));
    }
    if !matches!(store.backend(), minds_store::Backend::InRepo) {
        return Err(other(
            "the protected witness supports the in-repository store only",
        ));
    }
    let root = root.canonicalize()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| home.canonicalize().ok());
    if privacy_protected(&root, home.as_deref()) {
        return Err(other(
            "the repository lies in a folder macOS keeps from system services (Desktop, \
             Documents, Downloads, Pictures, iCloud, /Volumes) — move it elsewhere for a \
             protected witness",
        ));
    }
    // Das Binary, das kopiert wird: das laufende, nie eines im Repo.
    let source = crate::enable_witness::service_binary(&root)?;
    let user = developer()?;
    prepare_repo(&root, verbose)?;
    let intent_key = intent_key(&root);
    let script = script(&Values {
        source: &source,
        root: &root,
        user: &user,
        uid: euid,
        managed: existing_managed(),
        intent_key: intent_key.line(),
    })?;
    if verbose {
        eprintln!("  setup script (run once as root):\n{script}");
    }
    eprintln!(
        "minds: setting up the protected witness — it runs as {WITNESS_USER} from {BINARY}; \
         you are asked for your password once"
    );
    // Der volle Fingerabdruck, mit einem `ssh-keygen` außerhalb von allem,
    // was der Agent ablegen kann. Vergleichen mit dem des eigenen
    // Sicherheitsschlüssels — nicht mit der `.pub` daneben: Die und
    // `user.signingkey` kann der Agent geschrieben haben.
    match &intent_key {
        IntentKey::Key(key) => {
            let shown = crate::verify_cmd::conformance::spec_program(&root)
                .map(|program| crate::witness_cmd::key_display_with(&program, key))
                .unwrap_or_else(|| "(fingerprint unavailable)".to_owned());
            eprintln!(
                "  intent key (from minds.intentKey): {} — once pinned, a touch of it activates \
                 an intent at the witness. Compare this fingerprint with your security key's \
                 before you enter your password; a key pinned earlier is kept",
                crate::text::sanitize(&shown)
            );
        }
        IntentKey::NotSet => {
            eprintln!("  no minds.intentKey set — intents are activated with your password")
        }
        IntentKey::Unusable(path) => eprintln!(
            "  minds.intentKey ({}) has no security key's .pub next to it — nothing pinned; \
             intents are activated with your password",
            crate::text::sanitize(path)
        ),
    }
    let output = elevate(&script)?;
    let mut signers = None;
    let mut managed = None;
    let mut notes = Vec::new();
    let mut intent_keys = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(line) = line.strip_prefix(SIGNERS_MARK) {
            signers = Some(crate::text::sanitize(line.trim()));
        } else if let Some(line) = line.strip_prefix(MANAGED_MARK) {
            managed = Some(crate::text::sanitize(line.trim()));
        } else if let Some(line) = line.strip_prefix(NOTE_MARK) {
            notes.push(crate::text::sanitize(line.trim()));
        } else if let Some(line) = line.strip_prefix(INTENT_KEY_MARK) {
            intent_keys.push(crate::text::sanitize(line.trim()));
        } else if !line.trim().is_empty() {
            eprintln!("  {}", crate::text::sanitize(line));
        }
    }
    if !output.status.success() {
        return Err(other(format!(
            "the protected setup stopped ({}) — see the lines above",
            output.status
        )));
    }
    // Bis zu zehn Sekunden auf einen Witness, der antwortet — wer genau
    // antwortet, prüft `discover` (Ort und Eigentümer) wie die Hooks.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let answering = loop {
        if discover(&root).is_some_and(|(socket, _)| crate::witness_cmd::ping(&socket)) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    if answering {
        stop_local_service(&root);
    }
    Ok(Installed {
        socket: home_of(&root).join("run").join("witness.sock"),
        answering,
        signers,
        managed,
        notes,
        intent_key: intent_keys,
    })
}

/// Die Werte, die in das Skript eingesetzt werden.
struct Values<'a> {
    source: &'a Path,
    root: &'a Path,
    /// Der Entwickler: Name und Kennung.
    user: &'a str,
    uid: u32,
    /// Die vorgefundene Managed-Settings-Datei ([`managed_plan`]).
    managed: Option<String>,
    /// Der Freigabe-Schlüssel für Intents ([`intent_key`]), falls einer.
    intent_key: Option<&'a str>,
}

/// Der Sicherheitsschlüssel aus `minds.intentKey` als Freigabe-Schlüssel
/// (`<typ> <base64>`) — ein **eigener** Schlüssel für Freigaben, nicht
/// `user.signingkey`: Mit dem signiert jeder Commit, und jede Berührung
/// dafür wäre eine Freigabe, die der Agent auslösen kann. Ohne ihn (oder
/// mit einem Software-Schlüssel) wird nichts festgehalten; dann aktiviert
/// `intent sign` mit dem Passwort.
///
/// Nur ein **Vorschlag**, kein Vertrauensanker: Die Git-Konfiguration und
/// die `.pub` kann der Agent geschrieben haben (auch einen
/// Software-„Sicherheitsschlüssel"). Darum zeigt `enable` den vollen
/// Fingerabdruck vor dem Passwort, und ein schon festgehaltener Schlüssel
/// wird hier nie ersetzt.
fn intent_key(root: &Path) -> IntentKey {
    intent_key_from(crate::sign_cmd::intent_key_config(root))
}

/// [`intent_key`] für einen schon gelesenen Wert von `minds.intentKey`.
fn intent_key_from(configured: Option<String>) -> IntentKey {
    let Some(key) = configured else {
        return IntentKey::NotSet;
    };
    crate::intent_cmd::read_public_key(&key)
        .and_then(|line| crate::witness_cmd::intent_key_line(&line).ok())
        .map_or(IntentKey::Unusable(key), IntentKey::Key)
}

/// Was `minds.intentKey` hergibt.
enum IntentKey {
    NotSet,
    /// Gesetzt, aber keine `.pub` eines Sicherheitsschlüssels daneben.
    Unusable(String),
    /// Die Zeile, die festgehalten wird.
    Key(String),
}

impl IntentKey {
    fn line(&self) -> Option<&str> {
        match self {
            Self::Key(line) => Some(line),
            Self::NotSet | Self::Unusable(_) => None,
        }
    }
}

/// Das Einrichtungsskript für diese Plattform — jeder Wert in einfachen
/// Anführungszeichen, eingesetzt in **einem** Durchgang (ein Wert, der wie
/// ein Platzhalter aussieht, bleibt Text).
fn script(values: &Values<'_>) -> std::io::Result<String> {
    let id = crate::enable_witness::repo_id(values.root);
    let home = home_of(values.root);
    let text = |path: &Path| -> std::io::Result<String> {
        let text = path
            .to_str()
            .ok_or_else(|| other(format!("{} is not valid UTF-8", path.display())))?;
        if text.chars().any(char::is_control) {
            return Err(other(format!(
                "{} contains control characters",
                crate::text::sanitize(text)
            )));
        }
        Ok(text.to_owned())
    };
    if !word(values.user) {
        return Err(other("the developer's user name is not a plain word"));
    }
    let home_text = text(&home)?;
    let (template, label, service_file, service) = if cfg!(target_os = "macos") {
        let label = format!("dev.minds.witness.{id}");
        let file = format!("/Library/LaunchDaemons/{label}.plist");
        let service = launch_daemon(&label, &home_text)?;
        (
            include_str!("../templates/witness/protected-macos.sh"),
            label,
            file,
            service,
        )
    } else {
        let unit = format!("minds-witness-{id}.service");
        let file = format!("/etc/systemd/system/{unit}");
        let service = system_unit(&id, &home_text, &text(values.root)?)?;
        (
            include_str!("../templates/witness/protected-linux.sh"),
            unit,
            file,
            service,
        )
    };
    let managed = managed_plan(
        managed_rules(values.root, &home)?,
        values.managed.as_deref(),
    )?;
    let quote = |value: &str| format!("'{}'", value.replace('\'', r"'\''"));
    let pairs: [(&str, String); 15] = [
        ("SOURCE", quote(&text(values.source)?)),
        ("BINARY", quote(BINARY)),
        ("WUSER", quote(WITNESS_USER)),
        ("DEVUSER", quote(values.user)),
        ("DEVUID", quote(&values.uid.to_string())),
        ("BASE", quote(&text(&base())?)),
        ("HOME", quote(&home_text)),
        ("ROOT", quote(&text(values.root)?)),
        ("LABEL", quote(&label)),
        ("SERVICE_FILE", quote(&service_file)),
        ("SERVICE_CONTENT", quote(&service)),
        ("MANAGED_DIR", quote(MANAGED_DIR)),
        ("MANAGED_CONTENT", quote(&managed.content)),
        ("MANAGED_PREVIOUS", quote(&managed.previous)),
        ("INTENT_KEY", quote(values.intent_key.unwrap_or_default())),
    ];
    // Ein Durchgang über die Vorlage: Eingesetzte Werte werden nie erneut
    // nach Platzhaltern durchsucht.
    let mut out = String::with_capacity(template.len() + 4096);
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| other("unterminated placeholder in the setup template"))?;
        let name = &after[..end];
        let value = pairs
            .iter()
            .find(|(key, _)| *key == name)
            .ok_or_else(|| other(format!("unknown placeholder {name} in the setup template")))?;
        out.push_str(&value.1);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Ein Name aus Buchstaben, Ziffern, `.`, `_`, `-` — für Nutzer und Gruppe.
fn word(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Der LaunchDaemon: unter dem Witness-Nutzer, Umask 007 — was er in
/// `.git` anlegt, bleibt für die Gruppe schreibbar; sein Home legt er mit
/// eigenen, engeren Modi an. Sein stderr (Start und Fehler des
/// Datei-Beobachters, Panics) landet in `<home>/log/witness.err` — neben
/// `witness.log`, das der Witness selbst schreibt; ohne die Datei wäre ein
/// Beobachter, der nie startete, nur über `minds doctor` zu sehen.
fn launch_daemon(label: &str, home: &str) -> std::io::Result<String> {
    let xml = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Minds witness, protected (minds enable). -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>UserName</key>
	<string>{user}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{binary}</string>
		<string>witness</string>
		<string>run</string>
		<string>--home</string>
		<string>{home}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/usr/bin:/bin:/usr/sbin:/sbin</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>StandardErrorPath</key>
	<string>{home}/log/witness.err</string>
	<key>Umask</key>
	<integer>7</integer>
</dict>
</plist>"#,
        label = xml(label),
        user = xml(WITNESS_USER),
        binary = xml(BINARY),
        home = xml(home),
    ))
}

/// Die systemd-Unit: unter dem Witness-Nutzer, Umask 007 (siehe
/// [`launch_daemon`]), gehärtet: keine neuen Rechte, das System nur lesend
/// bis auf sein Home und das `.git` des Repos.
fn system_unit(id: &str, home: &str, root: &str) -> std::io::Result<String> {
    for path in [home, root] {
        if path
            .chars()
            .any(|c| c.is_whitespace() || c == '%' || c == '"' || c == '\\' || c == '\'')
        {
            return Err(other("a path cannot be written into a systemd unit"));
        }
    }
    Ok(format!(
        "[Unit]\nDescription=Minds witness ({id}, protected)\nAfter=network.target\n\n\
         [Service]\nType=simple\nUser={WITNESS_USER}\nGroup={WITNESS_USER}\n\
         ExecStart={BINARY} witness run --home {home}\n\
         Environment=PATH=/usr/bin:/bin:/usr/sbin:/sbin\nRestart=on-failure\nRestartSec=5\n\
         UMask=0007\nNoNewPrivileges=yes\nPrivateTmp=yes\nProtectSystem=strict\n\
         ReadWritePaths={home} {root}/.git\n\n[Install]\nWantedBy=multi-user.target"
    ))
}

/// Der `$comment`, an dem minds seine eigene Managed-Settings-Datei erkennt.
const MANAGED_MARKER: &str =
    "Installed by `minds enable` (protected witness): deny rules only. Remove this file to undo.";

/// Die Deny-Regeln für dieses Repo und diesen Witness — nur Deny-Regeln,
/// keine Sandbox, keine Einschränkung eigener Hooks oder Regeln (die Datei
/// gilt für jede Claude-Code-Sitzung auf dem Rechner).
fn managed_rules(root: &Path, home: &Path) -> std::io::Result<Vec<String>> {
    let text = |path: &Path| -> std::io::Result<String> {
        let text = path
            .to_str()
            .ok_or_else(|| other("a path is not valid UTF-8"))?
            .to_owned();
        // Glob-Zeichen ergäben eine Regel, die den Pfad selbst nicht trifft.
        if text.chars().any(|c| {
            c.is_control() || matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '\\')
        }) {
            return Err(other(format!(
                "{} contains glob or rule metacharacters — the managed settings could not \
                 protect it",
                crate::text::sanitize(&path.display().to_string())
            )));
        }
        Ok(text)
    };
    let refs = format!("{}/.git/refs/minds", text(root)?);
    let home = text(home)?;
    let mut deny = vec![
        "Bash(minds witness:*)".to_owned(),
        format!("Bash({BINARY} witness:*)"),
    ];
    // Claude Code prüft Dateizugriffe nur gegen `Read(…)` und `Edit(…)` —
    // `Edit` deckt jedes schreibende Werkzeug ab; eine `Write(…)`-Regel
    // ignoriert es mit einer Warnung bei jedem Start.
    deny.push(format!("Edit(/{refs}/**)"));
    for verb in ["Read", "Edit"] {
        deny.push(format!("{verb}(/{home}/**)"));
    }
    Ok(deny)
}

fn render_managed(deny: &[String]) -> std::io::Result<String> {
    let settings = serde_json::json!({
        "$comment": MANAGED_MARKER,
        "permissions": { "deny": deny },
    });
    serde_json::to_string_pretty(&settings).map_err(other)
}

/// Was die Einrichtung mit den Managed Settings tut: `content` schreiben,
/// eine vorhandene Datei aber nur ersetzen, wenn sie noch genau `previous`
/// ist (sonst ein Vorschlag daneben).
#[derive(Debug, PartialEq, Eq)]
struct ManagedPlan {
    content: String,
    /// Leer: keine eigene Datei vorgefunden — eine vorhandene bleibt.
    previous: String,
}

/// Die Datei ist eine eigene minds-Datei, wenn sie als JSON **genau** unsere
/// Form hat (`$comment` mit unserer Kennung, `permissions.deny` als Liste von
/// Zeichenketten, sonst nichts) — dann wird sie zur Vereinigung ihrer
/// Regeln (ohne die überholten `Write(…)`) mit den neuen. Die Datei gilt
/// rechnerweit, die Regeln je Repo: So verliert ein zweites Repo die Regeln
/// des ersten nicht. Alles andere (eine Datei des Administrators, eine
/// mit weiteren Schlüsseln) bekommt nur einen Vorschlag.
fn managed_plan(new_deny: Vec<String>, existing: Option<&str>) -> std::io::Result<ManagedPlan> {
    // Eine eigene Datei über 32 KiB (sehr viele Repos) bekommt einen
    // Vorschlag: Alte und neue Fassung stehen im Skript, und das ist unter
    // Linux ein einziges Argument (höchstens 128 KiB).
    let existing = existing.filter(|text| text.len() <= 32 * 1024);
    let ours = existing.and_then(|text| {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let object = value.as_object()?;
        let permissions = object.get("permissions")?.as_object()?;
        if object.len() != 2
            || object.get("$comment")?.as_str()? != MANAGED_MARKER
            || permissions.len() != 1
        {
            return None;
        }
        permissions
            .get("deny")?
            .as_array()?
            .iter()
            .map(|rule| rule.as_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>()
    });
    match ours {
        Some(old) => {
            let mut deny: Vec<String> = old
                .into_iter()
                .filter(|rule| !rule.starts_with("Write("))
                .collect();
            for rule in new_deny {
                if !deny.contains(&rule) {
                    deny.push(rule);
                }
            }
            Ok(ManagedPlan {
                content: render_managed(&deny)?,
                previous: existing.unwrap_or_default().to_owned(),
            })
        }
        None => Ok(ManagedPlan {
            content: render_managed(&new_deny)?,
            previous: String::new(),
        }),
    }
}

/// Die vorhandene Managed-Settings-Datei, gelesen als Entwickler (sie ist
/// für alle lesbar) — begrenzt, nur eine reguläre Datei.
fn existing_managed() -> Option<String> {
    use std::io::Read;
    let path = Path::new(MANAGED_DIR).join("managed-settings.json");
    if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .ok()?
        .take(1024 * 1024)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Der Login-Name des Entwicklers.
fn developer() -> std::io::Result<String> {
    // SAFETY: getpwuid liefert NULL oder einen Zeiger auf eine statische
    // Struktur, deren Name sofort kopiert wird; geteuid hat keine
    // Vorbedingungen.
    let name = unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if entry.is_null() || (*entry).pw_name.is_null() {
            return Err(other("the current user has no name"));
        }
        std::ffi::CStr::from_ptr((*entry).pw_name)
            .to_string_lossy()
            .into_owned()
    };
    if !word(&name) {
        return Err(other("the developer's user name is not a plain word"));
    }
    Ok(name)
}

/// Bereitet das Repo vor — als Entwickler, ohne Rechte, ohne ein Recht zu
/// erweitern: `core.sharedRepository=group`, und `logs/` und `minds/` in
/// `.git` gibt es danach (der Witness schreibt dorthin). Gruppe und Modi
/// setzt erst das Skript, als Entwickler und **nach** dem Gruppenwechsel —
/// so wird nichts für die bisherige Gruppe (macOS: `staff`) schreibbar,
/// auch wenn die Einrichtung abbricht.
pub(crate) fn prepare_repo(root: &Path, verbose: bool) -> std::io::Result<()> {
    let git = root.join(".git");
    let meta = std::fs::symlink_metadata(&git)?;
    if !meta.is_dir() {
        return Err(other(
            "the protected witness needs a plain repository (.git is a directory)",
        ));
    }
    crate::config::set(root, "core.sharedRepository", "group")?;
    for dir in ["logs", "minds"] {
        match std::fs::symlink_metadata(git.join(dir)) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(git.join(dir))?;
            }
            Err(err) => return Err(err),
        }
    }
    if verbose {
        eprintln!("  repository prepared (core.sharedRepository=group)");
    }
    Ok(())
}

/// Ob `.git` über `start` für einen geschützten Witness geteilt ist — das
/// Skript setzt dort das Sticky-Bit und die Gruppe des Witness. Dann schreibt
/// minds mit Umask 002, damit der Witness in Verzeichnisse schreiben kann,
/// die minds anlegt (gix folgt `core.sharedRepository` nicht). Ein `stat` je
/// Verzeichnis nach oben; die Gruppe wird nur für ein Sticky-`.git`
/// nachgeschlagen.
pub(crate) fn shared_repository(start: &Path) -> bool {
    shared_with(start, witness_gid)
}

/// [`shared_repository`] mit der Gruppe des Witness als Parameter (Tests).
fn shared_with(start: &Path, group: impl FnOnce() -> Option<u32>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(meta) = start
        .ancestors()
        .take(64)
        .find_map(|dir| std::fs::symlink_metadata(dir.join(".git")).ok())
    else {
        return false;
    };
    // Ein Sticky-`.git` einer anderen Gruppe (etwa per tar mitgebracht) ist
    // keines, das minds geteilt hat — dort bliebe es bei 022.
    meta.is_dir() && meta.mode() & 0o1000 != 0 && group() == Some(meta.gid())
}

/// Ob dieser Nutzer in der Witness-Gruppe eingetragen ist, die laufende
/// Sitzung sie aber noch nicht hat — nur unter Linux, wo die Gruppe erst
/// nach dem nächsten Anmelden gilt (macOS löst Mitgliedschaften zur Laufzeit
/// auf; `getgroups` nennt dort höchstens 16 Gruppen und bewiese nichts).
pub(crate) fn membership_pending() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let Some(gid) = witness_gid() else {
        return false;
    };
    // SAFETY: getgroups mit 0 liefert die Anzahl; danach in einen Puffer
    // dieser Größe.
    let groups = unsafe {
        let count = libc::getgroups(0, std::ptr::null_mut());
        let Ok(len) = usize::try_from(count) else {
            return false;
        };
        let mut groups = vec![0 as libc::gid_t; len];
        if libc::getgroups(count, groups.as_mut_ptr()) < 0 {
            return false;
        }
        groups
    };
    // SAFETY: getegid hat keine Vorbedingungen.
    if groups.contains(&gid) || unsafe { libc::getegid() } == gid {
        return false;
    }
    let Ok(user) = developer() else {
        return false;
    };
    let Ok(name) = std::ffi::CString::new(WITNESS_USER) else {
        return false;
    };
    // SAFETY: getgrnam liefert NULL oder eine statische Struktur, deren
    // NULL-terminierte Mitgliederliste sofort gelesen wird. macOS legt die
    // Liste nicht immer ausgerichtet ab — jeder Eintrag wird deshalb mit
    // `read_unaligned` gelesen; höchstens 4096 Einträge.
    unsafe {
        let entry = libc::getgrnam(name.as_ptr());
        if entry.is_null() {
            return false;
        }
        let list = std::ptr::read_unaligned(&raw const (*entry).gr_mem);
        if list.is_null() {
            return false;
        }
        for index in 0..4096 {
            let member = std::ptr::read_unaligned(list.add(index));
            if member.is_null() {
                break;
            }
            if std::ffi::CStr::from_ptr(member).to_bytes() == user.as_bytes() {
                return true;
            }
        }
    }
    false
}

/// Die Gruppe des Witness, falls es sie gibt.
fn witness_gid() -> Option<u32> {
    let name = std::ffi::CString::new(WITNESS_USER).ok()?;
    // SAFETY: `name` ist NUL-terminiert; getgrnam liefert NULL oder einen
    // Zeiger auf eine statische Struktur, aus der sofort kopiert wird —
    // ungerichtet gelesen wie die Mitgliederliste in `membership_pending`.
    unsafe {
        let entry = libc::getgrnam(name.as_ptr());
        (!entry.is_null()).then(|| std::ptr::read_unaligned(&raw const (*entry).gr_gid))
    }
}

/// Führt das Skript mit der einen Erhöhung aus. Nur in Debug-Builds (Tests)
/// ersetzt `MINDS_SUDO_STUB` das Programm; der Ersatz bekommt dieselben
/// Argumente.
fn elevate(script: &str) -> std::io::Result<std::process::Output> {
    let program = elevation_program();
    // `-k`: Ein zwischengespeichertes Passwort zählt nicht, und es entsteht
    // keins — der Agent könnte es sonst in den nächsten Minuten nutzen.
    std::process::Command::new(program)
        .args(["-k", "/bin/sh", "-c", script, "minds-protected-setup"])
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .output()
}

/// Aktiviert einen Intent am geschützten Witness dieses Repos: als
/// Witness-Nutzer `minds witness activate` aus dem root-eigenen Binary
/// ([`BINARY`]), `request` (JSON mit Anker und Signatur) über stdin. Mit
/// derselben einen Erhöhung wie die Einrichtung, immer mit Passwort (`-k`):
/// Es ist der Beleg, dass der Mensch diese Aktivierung will — der Agent
/// teilt seine Kennung, aber nicht sein Passwort. Die Ausgabe des Witness
/// (stdout) wird eingesammelt, stderr und die Passwortabfrage gehen ans
/// Terminal.
pub(crate) fn activate_elevated(
    root: &Path,
    request: &[u8],
) -> std::io::Result<std::process::Output> {
    activate_elevated_with(&elevation_program(), &home_of(root), request)
}

/// [`activate_elevated`] mit genau dem Programm `program` (im Test ein
/// Ersatz).
fn activate_elevated_with(
    program: &Path,
    home: &Path,
    request: &[u8],
) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    let mut child = std::process::Command::new(program)
        .args(activation_args(home))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| other("no stdin for the elevated activation"))?;
        // Bricht die Erhöhung ab (falsches Passwort), liest niemand: Das
        // Urteil fällt am Exit-Status, nicht am Schreiben.
        if let Err(err) = stdin.write_all(request) {
            if err.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(err);
            }
        }
    }
    child.wait_with_output()
}

/// Die Argumente der Erhöhung für [`activate_elevated`] — ohne Shell, jeder
/// Wert ein eigenes Argument.
fn activation_args(home: &Path) -> Vec<std::ffi::OsString> {
    // `env -i` wie im Einrichtungsskript: Der Witness-Nutzer erbt weder
    // `PATH` noch `HOME` des Entwicklers (die der Agent setzen kann).
    let mut args: Vec<std::ffi::OsString> = [
        "-k",
        "-p",
        "minds: your password approves activating this intent at the protected witness: ",
        "-u",
        WITNESS_USER,
        "--",
        "/usr/bin/env",
        "-i",
        "HOME=/var/empty",
        "PATH=/usr/bin:/bin",
        BINARY,
        "witness",
        "activate",
        "--home",
    ]
    .iter()
    .map(Into::into)
    .collect();
    args.push(home.as_os_str().to_owned());
    args
}

/// Das Programm für die Erhöhung. Nur in Debug-Builds (Tests) ersetzt
/// `MINDS_SUDO_STUB` es; der Ersatz bekommt dieselben Argumente.
fn elevation_program() -> PathBuf {
    #[cfg(debug_assertions)]
    if let Some(stub) = std::env::var_os("MINDS_SUDO_STUB") {
        return PathBuf::from(stub);
    }
    PathBuf::from(ELEVATE)
}

/// Hält den ungeschützten Nutzerdienst dieses Repos an und schaltet ihn ab
/// (Durchlauf 2a) — die Hooks finden ab jetzt den geschützten. Die Dateien
/// bleiben; ein Fehler ändert nichts am Ergebnis. In Debug-Builds gilt der
/// Ersatz für den Dienstverwalter (`MINDS_SERVICE_MANAGER_STUB`).
pub(crate) fn stop_local_service(root: &Path) {
    let id = crate::enable_witness::repo_id(root);
    #[cfg(debug_assertions)]
    let stub = std::env::var_os("MINDS_SERVICE_MANAGER_STUB").map(PathBuf::from);
    #[cfg(not(debug_assertions))]
    let stub: Option<PathBuf> = None;
    let run = |program: &str, args: &[&str]| {
        let mut command = match &stub {
            Some(stub) => {
                let mut command = std::process::Command::new(stub);
                command.arg(Path::new(program).file_name().unwrap_or_default());
                command
            }
            None => std::process::Command::new(program),
        };
        let _ = command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    if cfg!(target_os = "macos") {
        // SAFETY: getuid hat keine Vorbedingungen.
        let uid = unsafe { libc::getuid() };
        let service = format!("gui/{uid}/dev.minds.witness.{id}");
        run("/bin/launchctl", &["bootout", &service]);
        // Sonst lüde launchd den Job beim nächsten Anmelden wieder.
        run("/bin/launchctl", &["disable", &service]);
    } else if let Some(systemctl) = ["/usr/bin/systemctl", "/bin/systemctl"]
        .into_iter()
        .find(|path| stub.is_some() || Path::new(path).is_file())
    {
        run(
            systemctl,
            &[
                "--user",
                "disable",
                "--now",
                &format!("minds-witness-{id}.service"),
            ],
        );
    }
}

fn other(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> Place {
        Place {
            chain: true,
            base_dir: true,
            base_uid: 498,
            base_mode: 0o40710,
            home_dir: true,
            home_uid: 498,
        }
    }

    /// Ein Ort gilt nur, wenn ihn der Agent nicht angelegt haben kann — und
    /// er dem Witness-Nutzer gehört.
    #[test]
    fn only_a_place_the_agent_cannot_have_made_is_trusted() {
        assert!(place().trusted(501, 498));
        assert!(!place().trusted(501, 497), "another user than the witness");
        for broken in [
            Place {
                chain: false,
                ..place()
            },
            Place {
                base_uid: 501,
                home_uid: 501,
                ..place()
            },
            Place {
                home_uid: 499,
                ..place()
            },
            Place {
                base_mode: 0o40730,
                ..place()
            },
            Place {
                home_dir: false,
                ..place()
            },
        ] {
            assert!(!broken.trusted(501, 498), "{broken:?}");
        }
        // Der Agent selbst oder root als „Witness" zählen nie.
        assert!(!place().trusted(498, 498));
        let rooted = Place {
            base_uid: 0,
            home_uid: 0,
            ..place()
        };
        assert!(!rooted.trusted(501, 0));
    }

    fn values<'a>(root: &'a Path, source: &'a Path) -> Values<'a> {
        Values {
            source,
            root,
            user: "dev",
            uid: 501,
            managed: None,
            intent_key: None,
        }
    }

    /// Jeder Wert steht in einfachen Anführungszeichen — auch einer mit
    /// Anführungszeichen, `$HOME`, Backticks und Leerzeichen; das Skript
    /// ist gültiges sh.
    #[test]
    fn the_script_quotes_every_value_and_parses() {
        let dir = tempfile::tempdir().unwrap();
        // macOS: ein Pfad mit Anführungszeichen und Leerzeichen; Linux: einer,
        // den die systemd-Unit erlaubt (Leerzeichen und Anführungszeichen
        // lehnt sie ab — siehe unten).
        let (name, quoted) = if cfg!(target_os = "macos") {
            ("it's $HOME `id`; repo", r"'\''s $HOME `id`; repo'")
        } else {
            ("it$HOME`id`;repo", "it$HOME`id`;repo'")
        };
        let root = dir.path().join(name);
        std::fs::create_dir(&root).unwrap();
        let script = script(&values(&root, Path::new("/opt/minds/bin/minds"))).unwrap();
        assert!(script.contains(quoted), "{script}");
        assert!(!script.contains("{{"), "{script}");
        assert!(script.contains("DEVUID='501'"), "{script}");
        assert!(script.contains("--profile user --socket-group \"$WUSER\""));
        let out = std::process::Command::new("/bin/sh")
            .args(["-n", "-c", &script])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Ein Nutzername ist ein schlichtes Wort.
        let mut bad = values(&root, Path::new("/opt/minds"));
        bad.user = "dev; rm -rf /";
        assert!(super::script(&bad).is_err());
        // Unter Linux lehnt die Unit Pfade ab, die systemd zerlegen würde.
        if !cfg!(target_os = "macos") {
            let spaced = dir.path().join("it's a repo");
            std::fs::create_dir(&spaced).unwrap();
            assert!(super::script(&values(&spaced, Path::new("/opt/minds"))).is_err());
        }
    }

    /// Der LaunchDaemon schreibt sein stderr in das Log-Verzeichnis des
    /// Witness-Homes — XML-sicher, auch bei einem Home mit Sonderzeichen.
    #[test]
    fn the_launch_daemon_writes_stderr_into_the_witness_home() {
        let plist =
            launch_daemon("dev.minds.witness.x", "/private/var/db/minds-witness/x").unwrap();
        assert!(
            plist.contains(
                "\t<key>StandardErrorPath</key>\n\t<string>/private/var/db/minds-witness/x/log/witness.err</string>\n"
            ),
            "{plist}"
        );
        let odd = launch_daemon("l", "/tmp/a&b<c>").unwrap();
        assert!(
            odd.contains("<string>/tmp/a&amp;b&lt;c&gt;/log/witness.err</string>"),
            "{odd}"
        );
    }

    /// Beide Vorlagen nennen nur Platzhalter, die `script` kennt — auch die
    /// der anderen Plattform, die hier nie gerendert wird.
    #[test]
    fn both_templates_use_only_known_placeholders() {
        const KNOWN: [&str; 15] = [
            "SOURCE",
            "BINARY",
            "WUSER",
            "DEVUSER",
            "DEVUID",
            "BASE",
            "HOME",
            "ROOT",
            "LABEL",
            "SERVICE_FILE",
            "SERVICE_CONTENT",
            "MANAGED_DIR",
            "MANAGED_CONTENT",
            "MANAGED_PREVIOUS",
            "INTENT_KEY",
        ];
        for template in [
            include_str!("../templates/witness/protected-macos.sh"),
            include_str!("../templates/witness/protected-linux.sh"),
        ] {
            let mut rest = template;
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let end = after.find("}}").expect("terminated placeholder");
                assert!(KNOWN.contains(&&after[..end]), "{}", &after[..end]);
                rest = &after[end + 2..];
            }
        }
    }

    /// Ein Wert, der wie ein Platzhalter aussieht, bleibt Text: Eingesetzt
    /// wird in einem Durchgang.
    #[test]
    fn a_value_that_looks_like_a_placeholder_stays_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo`touch`$HOME");
        std::fs::create_dir(&root).unwrap();
        let source = Path::new("/opt/{{ROOT}}/minds");
        let script = script(&values(&root, source)).unwrap();
        assert!(script.contains("SOURCE='/opt/{{ROOT}}/minds'"), "{script}");
        assert!(!script.contains("'/opt/'"), "{script}");
    }

    /// Repos in Ordnern, die macOS Systemdiensten vorenthält, werden nicht
    /// geschützt eingerichtet.
    #[test]
    fn privacy_protected_folders_are_refused_on_macos() {
        let home = Path::new("/Users/dev");
        let refused = cfg!(target_os = "macos");
        for root in [
            "/Users/dev/Documents/repo",
            "/Users/dev/Desktop/x",
            "/Users/dev/Library/Mobile Documents/com~apple~CloudDocs/r",
        ] {
            assert_eq!(
                privacy_protected(Path::new(root), Some(home)),
                refused,
                "{root}"
            );
        }
        assert!(!privacy_protected(
            Path::new("/Users/dev/dev/repo"),
            Some(home)
        ));
        assert!(!privacy_protected(
            Path::new("/Users/dev/Documents/r"),
            None
        ));
    }

    /// Das Sticky-Bit auf `.git` markiert ein geteiltes Repo — gefunden auch
    /// aus einem Unterverzeichnis.
    #[test]
    fn a_shared_repository_is_recognised_by_its_sticky_git() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::create_dir(&git).unwrap();
        use std::os::unix::fs::MetadataExt;
        let group = std::fs::metadata(&git).unwrap().gid();
        let deep = dir.path().join("src/deep");
        assert!(!shared_with(&deep, || Some(group)));
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o1770)).unwrap();
        assert!(shared_with(&deep, || Some(group)));
        // Sticky, aber nicht die Gruppe des Witness (oder es gibt keinen).
        assert!(!shared_with(&deep, || Some(group.wrapping_add(1))));
        assert!(!shared_with(&deep, || None));
    }

    /// Die Managed Settings tragen nur Deny-Regeln für dieses Repo und
    /// diesen Witness — keine Sandbox, keine Einschränkung eigener Hooks.
    #[test]
    fn the_managed_settings_only_deny() {
        let text = render_managed(
            &managed_rules(
                Path::new("/Users/dev/repo"),
                Path::new("/private/var/db/minds-witness/0123456789abcdef"),
            )
            .unwrap(),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["$comment", "permissions"]);
        let deny = value["permissions"]["deny"].as_array().unwrap();
        assert!(deny.iter().any(|r| r == "Bash(minds witness:*)"));
        assert!(
            deny.iter()
                .any(|r| r == "Edit(//Users/dev/repo/.git/refs/minds/**)")
        );
        assert!(
            deny.iter()
                .any(|r| r == "Read(//private/var/db/minds-witness/0123456789abcdef/**)")
        );
        assert!(value["permissions"].get("allow").is_none());
        // Nur Regeln, die Claude Code für Dateien auswertet.
        assert!(
            deny.iter()
                .all(|r| !r.as_str().unwrap().starts_with("Write(")),
            "{deny:?}"
        );
        assert!(managed_rules(Path::new("/Users/dev/re[p]o"), Path::new("/x")).is_err());
    }

    /// Eine eigene Datei (genau unsere Form) wird zur Vereinigung — die Regeln
    /// eines anderen Repos bleiben, überholte `Write(…)` fallen weg; alles
    /// andere bleibt unberührt (Vorschlag).
    #[test]
    fn the_managed_settings_merge_only_into_our_own_file() {
        let rules = |repo: &str| {
            managed_rules(
                Path::new(repo),
                Path::new("/private/var/db/minds-witness/0123456789abcdef"),
            )
            .unwrap()
        };
        // Keine Datei: neu.
        let fresh = managed_plan(rules("/Users/dev/a"), None).unwrap();
        assert!(fresh.previous.is_empty());
        // Eigene Datei von Repo A (noch mit `Write`) — Repo B kommt dazu.
        let mut old = rules("/Users/dev/a");
        old.push("Write(//Users/dev/a/.git/refs/minds/**)".into());
        let existing = render_managed(&old).unwrap() + "\n";
        let plan = managed_plan(rules("/Users/dev/b"), Some(&existing)).unwrap();
        assert_eq!(plan.previous, existing);
        assert!(
            plan.content
                .contains("Edit(//Users/dev/a/.git/refs/minds/**)")
        );
        assert!(
            plan.content
                .contains("Edit(//Users/dev/b/.git/refs/minds/**)")
        );
        assert!(!plan.content.contains("Write("), "{}", plan.content);
        // Dieselbe Datei noch einmal: keine doppelten Regeln.
        let again = managed_plan(rules("/Users/dev/a"), Some(&existing)).unwrap();
        let deny: serde_json::Value = serde_json::from_str(&again.content).unwrap();
        let deny = deny["permissions"]["deny"].as_array().unwrap();
        let unique: std::collections::BTreeSet<&str> =
            deny.iter().filter_map(serde_json::Value::as_str).collect();
        assert_eq!(deny.len(), unique.len());
        // Zu groß für ein Skript-Argument: ein Vorschlag.
        let mut many = rules("/Users/dev/a");
        many.extend((0..2000).map(|n| format!("Edit(//Users/dev/r{n}/.git/refs/minds/**)")));
        let large = render_managed(&many).unwrap();
        assert!(
            managed_plan(rules("/Users/dev/b"), Some(&large))
                .unwrap()
                .previous
                .is_empty()
        );
        // Fremd: weitere Schlüssel, anderer Kommentar, die Kennung nur im
        // Text — nie ersetzen.
        let marker = serde_json::to_string(MANAGED_MARKER).unwrap();
        for foreign in [
            format!(r#"{{"$comment":{marker},"permissions":{{"deny":[]}},"env":{{"X":"1"}}}}"#),
            r#"{"$comment":"company policy","permissions":{"deny":[]}}"#.to_owned(),
            format!(
                r#"{{"$comment":"ours too: {}","permissions":{{"deny":[]}}}}"#,
                MANAGED_MARKER
            ),
            format!(r#"{{"$comment":{marker},"permissions":{{"deny":[],"allow":["Bash"]}}}}"#),
            "not json".to_owned(),
        ] {
            let plan = managed_plan(rules("/Users/dev/b"), Some(&foreign)).unwrap();
            assert!(plan.previous.is_empty(), "{foreign}");
        }
    }

    #[test]
    fn group_and_user_names_are_plain_words() {
        for good in ["staff", "pd", "_minds", "minds-witness", "dev.team"] {
            assert!(word(good), "{good}");
        }
        for bad in ["", "-x", "a b", "a;b", "a$b", "a'b"] {
            assert!(!word(bad), "{bad}");
        }
    }

    /// Die Aktivierung mit Passwort: ohne Shell, `-k` (nie ein
    /// zwischengespeichertes Passwort), als Witness-Nutzer, das root-eigene
    /// Binary, das Home als eigenes Argument hinter `--`.
    #[test]
    fn the_elevated_activation_runs_the_installed_binary_as_the_witness() {
        let home = Path::new("/var/lib/minds-witness/0123 it's");
        let args: Vec<String> = activation_args(home)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(args[0], "-k");
        assert_eq!(args[1], "-p");
        assert!(!args[2].contains('%'), "{}", args[2]);
        assert_eq!(
            args[3..],
            [
                "-u",
                WITNESS_USER,
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/var/empty",
                "PATH=/usr/bin:/bin",
                BINARY,
                "witness",
                "activate",
                "--home",
                "/var/lib/minds-witness/0123 it's",
            ]
        );
    }

    /// Ein Sicherheitsschlüssel aus `minds.intentKey` steht gequotet im
    /// Skript und geht an `witness init --intent-key`; ohne bleibt der Wert
    /// leer.
    #[test]
    fn the_script_pins_the_intent_key_only_when_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let source = Path::new("/opt/minds/bin/minds");
        let script = script(&values(&root, source)).unwrap();
        assert!(script.contains("INTENT_KEY=''"), "{script}");
        let key = "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t";
        let script = super::script(&Values {
            intent_key: Some(key),
            ..values(&root, source)
        })
        .unwrap();
        assert!(script.contains(&format!("INTENT_KEY='{key}'")), "{script}");
        assert!(script.contains("--intent-key \"$INTENT_KEY\""), "{script}");
    }

    /// Der Weg über das Passwort: Die Anfrage geht über stdin an das
    /// Programm (im Test ein Ersatz, der sie ablegt); sein Exit-Status
    /// entscheidet — auch wenn es stdin nie liest (falsches Passwort).
    #[test]
    fn the_elevated_activation_hands_the_request_over_stdin() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = |name: &str, body: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let seen = dir.path().join("seen");
        let ok = stub(
            "ok.sh",
            &format!(
                "printf '%s\\n' \"$@\" > '{0}.args'; cat > '{0}'; echo 'intent active b3-x'",
                seen.display()
            ),
        );
        let home = Path::new("/var/lib/minds-witness/abc");
        let output = activate_elevated_with(&ok, home, br#"{"anchor":"a"}"#).unwrap();
        assert!(output.status.success());
        assert_eq!(std::fs::read(&seen).unwrap(), br#"{"anchor":"a"}"#);
        let args = std::fs::read_to_string(format!("{}.args", seen.display())).unwrap();
        assert!(
            args.ends_with("activate\n--home\n/var/lib/minds-witness/abc\n"),
            "{args}"
        );
        // Abgebrochen, bevor es stdin las: kein Fehler beim Schreiben,
        // der Status entscheidet.
        let refused = stub("refused.sh", "exit 1");
        let big = vec![b' '; 1024 * 1024];
        let output = activate_elevated_with(&refused, home, &big).unwrap();
        assert_eq!(output.status.code(), Some(1));
    }

    /// `minds.intentKey`: nicht gesetzt, gesetzt ohne Sicherheitsschlüssel
    /// daneben (Software-`.pub`, keine `.pub`), oder die Zeile ohne
    /// Kommentar, die festgehalten wird.
    #[test]
    fn the_intent_key_is_a_security_keys_pub_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
        assert!(matches!(intent_key_from(None), IntentKey::NotSet));

        std::fs::write(
            dir.path().join("soft.pub"),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl me\n",
        )
        .unwrap();
        assert!(matches!(
            intent_key_from(Some(path("soft"))),
            IntentKey::Unusable(_)
        ));
        assert!(matches!(
            intent_key_from(Some(path("missing"))),
            IntentKey::Unusable(_)
        ));

        // Ein (ausgedachter) Sicherheitsschlüssel: Typ und Base64, ohne
        // Kommentar.
        let blob = "AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIAkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJAAAABHNzaDo=";
        std::fs::write(
            dir.path().join("sk.pub"),
            format!("sk-ssh-ed25519@openssh.com {blob} me@host\n"),
        )
        .unwrap();
        match intent_key_from(Some(path("sk"))) {
            IntentKey::Key(line) => {
                assert_eq!(line, format!("sk-ssh-ed25519@openssh.com {blob}"));
            }
            _ => panic!("expected the key"),
        }
    }

    /// Gelesen wird `minds.intentKey` des Repos.
    #[test]
    fn the_intent_key_comes_from_the_repository_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        let missing = root.join("missing");
        git(&[
            "config",
            "--local",
            "minds.intentKey",
            missing.to_str().unwrap(),
        ]);
        match intent_key(root) {
            IntentKey::Unusable(path) => assert_eq!(path, missing.to_str().unwrap()),
            _ => panic!("expected the configured path"),
        }
    }
}
