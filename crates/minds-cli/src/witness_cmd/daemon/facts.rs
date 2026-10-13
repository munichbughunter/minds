//! Was der Witness beim Versiegeln über sich festhält (Observation-Objekt,
//! Feld `witness`): Profil, Trennung vom Agenten und das Soll des geprüften
//! Commits.
//!
//! Geprüft wird bei **jedem** Checkpoint, im Worker — derselben Kennung und
//! demselben Binary wie der Witness. Festgehalten wird der Befund, nicht das
//! Urteil: Ob die Trennung belegt ist, leitet der Leser ab
//! ([`minds_core::observation::Isolation::proven`]) — und nur unter einem
//! Principal, den ein Mensch als getrennten Witness eingetragen hat.
//!
//! Der Agent ist, wem Worktree-Wurzel, `.git` und (bei verknüpften
//! Worktrees) das gemeinsame Git-Verzeichnis gehören: Unter dieser Kennung
//! arbeitet er, schreibt er Dateien und Refs. Ein Witness unter derselben
//! Kennung (Profil `local`) trennt sich nicht; einer unter eigener Kennung
//! nur, wenn ihm auch Home und Binary allein gehören — sonst ersetzte der
//! Agent das Programm oder läse den Schlüssel. „Allein" heißt: Eigentümer
//! root oder der Witness, keine Schreibrechte für Gruppe und andere (das Home
//! nicht einmal Leserechte), über die ganze Verzeichniskette — und keine ACL
//! (macOS: erweiterte ACL; Linux: POSIX-, NFSv4- und jede andere
//! `system.*`-ACL; sonst fail-closed), die an den Modus-Bits vorbei Rechte
//! gäbe. Was sich nicht prüfen lässt, gilt als nicht geschützt.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use minds_core::observation::{Isolation, SpecSeen, WitnessFacts};
use minds_core::spec::{MAX_SPEC, SPEC_PATH, Spec, envelope_id};
use minds_store::ContextStore;

/// Die Fakten für die Epochen dieses Checkpoints; `commit` ist der Commit,
/// den der Checkpoint geprüft hat (sonst HEAD).
pub(super) fn witness_facts(
    profile: minds_core::evidence::WitnessProfile,
    home: &Path,
    root: &Path,
    repo: &minds_git::Repo,
    store: &dyn ContextStore,
    commit: Option<minds_git::CommitId>,
) -> WitnessFacts {
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    WitnessFacts {
        profile: profile.name().to_owned(),
        isolation: Isolation {
            witness_uid: euid,
            agent_uids: owners(&[root, repo.git_dir(), repo.common_dir()]),
            home_private: home_private(home, euid),
            binary_protected: binary_protected(euid),
        },
        spec: spec_seen(repo, store, commit),
    }
}

/// Die Eigentümer der Pfade — aufsteigend, ohne Doppelte. Ein Pfad, der
/// sich nicht lesen lässt, trägt nichts bei; bleibt keiner, ist die Liste
/// leer, und die Trennung gilt nicht als belegt.
pub(super) fn owners(paths: &[&Path]) -> Vec<u32> {
    let owners: std::collections::BTreeSet<u32> = paths
        .iter()
        .filter_map(|path| std::fs::symlink_metadata(path).ok())
        .map(|meta| meta.uid())
        .collect();
    owners.into_iter().collect()
}

/// Die Variable, in der der Witness dem Worker seinen Startpfad nennt: Der
/// Worker selbst startet über das aufgelöste Binary (Linux:
/// `/proc/self/exe`), sein `argv[0]` sagt über den Dienst nichts.
pub(crate) const STARTED_AS: &str = "MINDS_WITNESS_STARTED_AS";

/// Das laufende Binary — über den Pfad, unter dem es gestartet wurde, und
/// über seinen kanonischen: Ein Symlink in einem Verzeichnis des Agenten
/// ließe ihn sonst das Programm des nächsten Laufs tauschen. Den Startpfad
/// des Dienstes nennt [`STARTED_AS`] (im Worker) oder ein absolutes
/// `argv[0]` (im Witness selbst); `current_exe` ist unter Linux schon
/// aufgelöst. Der Dienst startet mit absolutem Pfad (Unit, plist) — ein
/// relativer Start über `PATH` bliebe ungeprüft.
pub(super) fn binary_protected(euid: u32) -> bool {
    let Ok(started) = std::env::current_exe() else {
        return false;
    };
    let Ok(canonical) = std::fs::canonicalize(&started) else {
        return false;
    };
    // Nur der Worker liest die Variable — sein Witness hat sie gesetzt;
    // anderswo stammte sie aus einer Umgebung, die der Agent prägen kann.
    let invoked = super::worker::is_worker()
        .then(|| std::env::var_os(STARTED_AS))
        .flatten()
        .or_else(|| std::env::args_os().next())
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute());
    protected_chain(&started, euid)
        && protected_chain(&canonical, euid)
        && invoked.is_none_or(|path| protected_chain(&path, euid))
}

/// Home und alles darin, was der Agent nicht sehen darf, gehören dem
/// Witness: das Home darf die Gruppe höchstens durchqueren (zum Socket,
/// Profil `user`), andere gar nicht; Schlüssel, `witness.json` und Journal
/// (rohe Hook-Events) sind nur für ihn. Die Kette über dem Home gehört root
/// oder ihm — sonst ließe sich das Home beiseiteschieben.
pub(super) fn home_private(home: &Path, euid: u32) -> bool {
    let chain = |path: &Path| {
        path.parent()
            .is_some_and(|parent| protected_chain(parent, euid))
    };
    home_contents_private(home, euid)
        && chain(home)
        && std::fs::canonicalize(home).is_ok_and(|real| chain(&real))
}

/// Der Teil von [`home_private`] ohne die Kette über dem Home.
fn home_contents_private(home: &Path, euid: u32) -> bool {
    home_entries_private(home, euid, true)
}

/// Wie [`home_contents_private`]; `with_key: false` lässt den Schlüssel
/// aus — für die Prüfung **vor** `keygen`.
pub(super) fn home_entries_private(home: &Path, euid: u32, with_key: bool) -> bool {
    // Wie die Startprüfung des Witness (`validate_tree_shared`): eigene,
    // private Einträge, Dateien mit genau einem Link — dazu keine ACL.
    let own = |path: &Path, deny: u32| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            !meta.file_type().is_symlink()
                && meta.uid() == euid
                && meta.mode() & deny == 0
                && (meta.is_dir() || meta.nlink() == 1)
        }) && !has_acl(path)
    };
    own(home, 0o067)
        && own(&home.join("run"), 0o067)
        && [
            "key",
            "key/witness_ed25519",
            "witness.json",
            "journal",
            "ledger",
            "evidence",
            "log",
        ]
        .iter()
        .filter(|entry| with_key || !entry.starts_with("key/"))
        .all(|entry| own(&home.join(entry), 0o077))
}

/// Ob `path` und jedes Verzeichnis darüber root oder dem Witness gehören,
/// für niemanden sonst beschreibbar sind und keine ACL tragen. Bei einem
/// Symlink zählen nur Eigentümer und ACL — seine Modus-Bits haben keine
/// Bedeutung (Linux: 0777).
fn protected_chain(path: &Path, euid: u32) -> bool {
    path.ancestors().all(|node| {
        std::fs::symlink_metadata(node).is_ok_and(|meta| {
            (meta.uid() == 0 || meta.uid() == euid)
                && (meta.file_type().is_symlink() || meta.mode() & 0o022 == 0)
        }) && !has_acl(node)
    })
}

/// Ob `path` (ohne einem Symlink zu folgen) eine erweiterte ACL trägt —
/// im Zweifel ja (fail-closed).
#[cfg(target_os = "macos")]
fn has_acl(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }
    /// `ACL_TYPE_EXTENDED` aus `<sys/acl.h>`.
    const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `path` ist NUL-terminiert; ein Ergebnis ungleich NULL wird mit
    // `acl_free` freigegeben.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        // Ohne ACL meldet macOS `ENOENT`; jeder andere Fehler: im Zweifel ja.
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT);
    }
    // SAFETY: `acl` stammt aus `acl_get_link_np`.
    unsafe { acl_free(acl) };
    true
}

/// Ob `path` (ohne einem Symlink zu folgen) eine ACL trägt: POSIX, NFSv4
/// oder jedes andere `system.*`-Attribut, dessen Name nach ACL aussieht
/// (OpenZFS, CIFS/SMB, richacl). Eine Minimal-ACL steht in den Modus-Bits
/// und erzeugt kein Attribut. Im Zweifel ja (fail-closed); ein Dateisystem
/// ohne erweiterte Attribute (`ENOTSUP`) trägt keine.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn has_acl(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // Alle Namen: Was nach einer ACL aussieht, zählt.
    // SAFETY: `path` ist NUL-terminiert; mit Größe 0 wird nichts geschrieben.
    let size = unsafe { libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOTSUP) {
            return true;
        }
    } else if size > 0 {
        let mut names = vec![0u8; size as usize];
        // SAFETY: Der Puffer fasst `names.len()` Bytes.
        let read =
            unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        // Inzwischen gewachsen oder nicht lesbar: im Zweifel ja.
        let Ok(read) = usize::try_from(read) else {
            return true;
        };
        let acl_like = names[..read].split(|byte| *byte == 0).any(|name| {
            name.starts_with(b"system.")
                && (name.windows(3).any(|w| w == b"acl") || name.windows(4).any(|w| w == b"ntsd"))
        });
        if acl_like {
            return true;
        }
    }
    // NFSv4 meldet seine ACL nicht jedem `listxattr` — ausdrücklich fragen.
    for name in [
        c"system.posix_acl_access",
        c"system.posix_acl_default",
        c"system.nfs4_acl",
    ] {
        // SAFETY: beide Zeichenketten sind NUL-terminiert; mit Größe 0 wird
        // nichts geschrieben.
        let size =
            unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if size >= 0 {
            return true;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENODATA | libc::ENOTSUP) => {}
            _ => return true,
        }
    }
    false
}

/// Wo ACLs nicht geprüft werden können: im Zweifel ja (fail-closed).
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn has_acl(_path: &Path) -> bool {
    true
}

/// Ob dieser Witness, so wie er hier läuft, vom Agenten getrennt ist —
/// dieselben Prüfungen wie in den Fakten. Grundlage dafür, ob `keygen` und
/// `enable` den Principal eines getrennten Witness ausgeben
/// ([`minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL`]): Wer die Zeile
/// übernimmt, bürgt damit für die Trennung. `with_key: false` vor `keygen`.
pub(super) fn separated_here(
    home: &Path,
    root: &Path,
    git_dir: &Path,
    common_dir: &Path,
    with_key: bool,
) -> bool {
    // SAFETY: geteuid hat keine Vorbedingungen.
    let euid = unsafe { libc::geteuid() };
    let chain = |path: &Path| {
        path.parent()
            .is_some_and(|parent| protected_chain(parent, euid))
    };
    let home_ok = home_entries_private(home, euid, with_key)
        && chain(home)
        && std::fs::canonicalize(home).is_ok_and(|real| chain(&real));
    Isolation {
        witness_uid: euid,
        agent_uids: owners(&[root, git_dir, common_dir]),
        home_private: home_ok,
        binary_protected: binary_protected(euid),
    }
    .proven()
}

/// Das Soll im geprüften Commit (sonst an HEAD) und seine Umschläge im
/// Store — `None` ohne Commit, ohne Soll oder wenn es sich nicht lesen
/// lässt. Die Umschläge sind ungeprüft; ein Store, der sich nicht lesen
/// lässt, hält für den Witness keine.
fn spec_seen(
    repo: &minds_git::Repo,
    store: &dyn ContextStore,
    commit: Option<minds_git::CommitId>,
) -> Option<SpecSeen> {
    let commit = match commit {
        Some(commit) => commit,
        None => repo.head().ok()?.commit()?,
    };
    let (_, bytes) = repo
        .read_blob_bounded(commit, SPEC_PATH, MAX_SPEC as u64)
        .ok()??;
    let spec = Spec::parse(&bytes).ok()?;
    let mut envelopes: Vec<minds_core::ContentHash> = store
        .get_spec(spec.id())
        .ok()
        .flatten()
        .map(|stored| {
            stored
                .envelopes
                .iter()
                .map(|envelope| envelope_id(envelope.signed.text()))
                .collect()
        })
        .unwrap_or_default();
    envelopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    envelopes.dedup();
    Some(SpecSeen {
        id: spec.id().clone(),
        envelopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Für Gruppe oder andere beschreibbar oder fremd: nicht geschützt;
    /// root-eigene Systempfade ohne ACL: geschützt.
    #[test]
    fn the_binary_chain_must_be_owned_and_not_writable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("minds");
        std::fs::write(&exe, "").unwrap();
        // SAFETY: geteuid hat keine Vorbedingungen.
        let euid = unsafe { libc::geteuid() };
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(!protected_chain(&exe, euid));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        if euid != 0 {
            assert!(!protected_chain(&exe, euid.wrapping_add(1)));
        }
        assert!(protected_chain(Path::new("/"), euid));
        // Ein Programm des Systems (root, 0755, keine ACL) — über den
        // Symlink `/bin`, wo es einer ist, und kanonisch.
        let sh = Path::new("/bin/sh");
        assert!(protected_chain(sh, euid), "{}", sh.display());
        assert!(protected_chain(&std::fs::canonicalize(sh).unwrap(), euid));
    }

    /// Eine erweiterte ACL gibt an den Modus-Bits vorbei Rechte: nicht
    /// geschützt.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_acl_makes_a_path_unprotected() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        let shared = dir.path().join("shared");
        std::fs::write(&plain, "").unwrap();
        std::fs::write(&shared, "").unwrap();
        assert!(!has_acl(&plain));
        let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg(format!("user:{user} allow read"))
            .arg(&shared)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(has_acl(&shared));
    }

    /// Home 0700 (die Gruppe darf durchqueren), Schlüssel, Konfiguration und
    /// Journal nur für den Witness: privat — jede Abweichung nicht.
    #[test]
    fn the_home_contents_are_private_only_when_everything_is() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        for entry in ["key", "journal", "evidence", "log", "run"] {
            std::fs::create_dir_all(home.join(entry)).unwrap();
        }
        let key = home.join("key/witness_ed25519");
        let config = home.join("witness.json");
        std::fs::write(&key, "k").unwrap();
        std::fs::write(&config, "{}").unwrap();
        std::fs::write(home.join("ledger"), "").unwrap();
        // SAFETY: geteuid hat keine Vorbedingungen.
        let euid = unsafe { libc::geteuid() };
        let mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        for entry in ["", "key", "journal", "evidence", "log", "run"] {
            mode(&home.join(entry), 0o700);
        }
        for path in [&key, &config, &home.join("ledger")] {
            mode(path, 0o600);
        }
        assert!(home_contents_private(&home, euid));
        mode(&home, 0o710);
        assert!(
            home_contents_private(&home, euid),
            "Durchqueren für die Gruppe"
        );
        for (path, open, closed) in [
            (&home, 0o750, 0o700),
            (&home, 0o701, 0o700),
            (&home.join("key"), 0o710, 0o700),
            (&key, 0o640, 0o600),
            (&config, 0o644, 0o600),
            (&home.join("journal"), 0o750, 0o700),
            (&home.join("ledger"), 0o640, 0o600),
            (&home.join("evidence"), 0o750, 0o700),
        ] {
            mode(path, open);
            assert!(!home_contents_private(&home, euid), "{}", path.display());
            mode(path, closed);
        }
        assert!(home_contents_private(&home, euid));
        assert!(!home_contents_private(&home, euid.wrapping_add(1)));
        // Ein zweiter harter Link auf den Schlüssel: nicht privat.
        let other = dir.path().join("elsewhere");
        std::fs::hard_link(&key, &other).unwrap();
        assert!(!home_contents_private(&home, euid), "zweiter Link");
        std::fs::remove_file(&other).unwrap();
        assert!(home_contents_private(&home, euid));
        std::fs::remove_file(&config).unwrap();
        assert!(!home_contents_private(&home, euid), "witness.json fehlt");
    }

    fn git(root: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    /// Das Soll an HEAD wird mit seiner Id festgehalten; ohne Umschlag im
    /// Store mit leerer Liste, ohne Soll gar nicht.
    #[test]
    fn the_spec_at_head_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.name", "Facts"]);
        git(root, &["config", "user.email", "facts@example.invalid"]);
        git(root, &["commit", "-qm", "init", "--allow-empty"]);
        let repo = minds_git::Repo::open(root).unwrap();
        let store = minds_store::InRepoStore::open(root).unwrap();
        assert_eq!(spec_seen(&repo, &store, None), None);

        let text =
            r#"{"rules":[{"expect":"session","id":"s","kind":"graph"}],"version":"minds-spec-v1"}"#;
        std::fs::create_dir_all(root.join(".minds")).unwrap();
        std::fs::write(root.join(SPEC_PATH), text).unwrap();
        git(root, &["add", SPEC_PATH]);
        git(root, &["commit", "-qm", "spec"]);
        let repo = minds_git::Repo::open(root).unwrap();
        let seen = spec_seen(&repo, &store, None).unwrap();
        assert_eq!(&seen.id, Spec::parse(text.as_bytes()).unwrap().id());
        assert!(seen.envelopes.is_empty());
    }

    #[test]
    fn owners_are_sorted_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: geteuid hat keine Vorbedingungen.
        let euid = unsafe { libc::geteuid() };
        assert_eq!(owners(&[dir.path(), dir.path()]), vec![euid]);
        assert!(owners(&[&dir.path().join("missing")]).is_empty());
    }
}
