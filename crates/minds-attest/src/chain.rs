//! Ob ein Programm oder eine Datei nur von root geändert werden kann: Pfad
//! und jedes Verzeichnis darüber gehören root, sind für Gruppe und andere
//! nicht beschreibbar, tragen keine ACL und liegen auf einem von root
//! eingehängten Dateisystem. Bis Spec S2 lag die Prüfung im Binary-Crate
//! `minds-cli`; der Server-Witness braucht dieselbe Regel (Spec S2 §6, §8.1).

use std::path::Path;

/// Ob `path` und jedes Verzeichnis darüber root gehören, weder für die
/// Gruppe noch für andere beschreibbar sind und (macOS) keine ACL tragen —
/// eine ACL könnte gewähren, was die Modus-Bits verweigern. Ohne Ausnahme
/// für Sticky-Verzeichnisse: In `/nix/store` legt der Nix-Daemon jedem
/// Nutzer root-eigene Dateien ab, in `/tmp` ein Hardlink eine fremde.
///
/// Und `path` liegt auf einem Dateisystem, das root eingehängt hat: Auf
/// einem, das ein Nutzer einhängen darf (udisks, FUSE, `hdiutil`), nennt
/// das Abbild selbst Eigentümer und Modus — dort könnte „root" der Agent
/// sein. Solche Mounts sind immer `nosuid`; `/usr` nie (sonst ginge `sudo`
/// nicht).
#[cfg(unix)]
pub fn root_owned_chain(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    root_mounted(path)
        && path.ancestors().all(|node| {
            std::fs::symlink_metadata(node)
                .is_ok_and(|meta| meta.uid() == 0 && meta.mode() & 0o022 == 0)
                && !carries_acl(node)
        })
}

/// Ob das Dateisystem von `path` ohne `nosuid` eingehängt ist (und unter
/// macOS root gehört). Fail-closed: Lässt es sich nicht feststellen, nein.
#[cfg(unix)]
fn root_mounted(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `name` ist NUL-terminiert, die Strukturen sind beschreibbar
    // und werden nur bei Erfolg gelesen.
    unsafe {
        let mut vfs: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(name.as_ptr(), &mut vfs) != 0 || vfs.f_flag & libc::ST_NOSUID != 0 {
            return false;
        }
        #[cfg(target_os = "macos")]
        {
            let mut fs: libc::statfs = std::mem::zeroed();
            if libc::statfs(name.as_ptr(), &mut fs) != 0 || fs.f_owner != 0 {
                return false;
            }
        }
    }
    true
}

/// Ob `path` (ohne Symlinks zu folgen) eine erweiterte ACL mit mindestens
/// einem Eintrag trägt. Fail-closed: Lässt sich das nicht feststellen, ja.
/// Außerhalb von macOS: nein (dort prüft die Einrichtung die Modus-Bits).
#[cfg(target_os = "macos")]
pub fn carries_acl(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(
            acl: *mut libc::c_void,
            entry: libc::c_int,
            out: *mut *mut libc::c_void,
        ) -> libc::c_int;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `name` ist NUL-terminiert; eine zurückgegebene ACL wird genau
    // einmal mit `acl_free` freigegeben, `entry` zeigt in sie und wird
    // danach nicht mehr benutzt.
    unsafe {
        let acl = acl_get_link_np(name.as_ptr(), ACL_TYPE_EXTENDED);
        if acl.is_null() {
            // Keine ACL: ENOENT.
            return std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT);
        }
        let mut entry = std::ptr::null_mut();
        let found = acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) == 0;
        acl_free(acl);
        found
    }
}

/// Linux: Eine POSIX-ACL zeigt sich in den Gruppen-Bits (der Maske), die
/// `root_owned_chain` ohnehin prüft.
#[cfg(target_os = "linux")]
pub fn carries_acl(_: &Path) -> bool {
    false
}

/// Andere Systeme (etwa NFSv4-ACLs): nicht feststellbar — fail-closed.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub fn carries_acl(_: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Weder ein beschreibbares Verzeichnis mit Sticky-Bit (wie `/tmp`)
    /// noch ein Eintrag des Nutzers darin zählt; `/usr/bin` schon.
    #[test]
    fn only_a_chain_only_root_can_change_counts() {
        let tmp = std::fs::canonicalize("/tmp").unwrap();
        assert!(!root_owned_chain(&tmp));
        let dir = tempfile::tempdir_in(&tmp).unwrap();
        let own = dir.path().join("ssh-keygen");
        std::fs::write(&own, b"").unwrap();
        assert!(!root_owned_chain(&own));
        if Path::new("/usr/bin/env").exists() {
            assert!(root_owned_chain(Path::new("/usr/bin")));
        }
    }
}
