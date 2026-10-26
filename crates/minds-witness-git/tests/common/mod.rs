//! Gemeinsame Testhelfer: Schlüssel zur Laufzeit, Signieren, Pfade.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const PROJECT: &str = "git.test/project-42";
pub const PRINCIPAL: &str = "minds-witness-user@test";

/// Das `ssh-keygen` der Testumgebung.
pub fn ssh_keygen() -> PathBuf {
    for candidate in [
        "/usr/bin/ssh-keygen",
        "/bin/ssh-keygen",
        "/opt/homebrew/bin/ssh-keygen",
    ] {
        if Path::new(candidate).exists() {
            return PathBuf::from(candidate);
        }
    }
    PathBuf::from("ssh-keygen")
}

pub fn timeout() -> Duration {
    Duration::from_secs(10)
}

/// Ein frischer Ed25519-Schlüssel in `dir`; liefert Pfad und `<typ> <base64>`.
pub fn keypair(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    let status = Command::new(ssh_keygen())
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    let mut words = public.split_whitespace();
    let line = format!("{} {}", words.next().unwrap(), words.next().unwrap());
    (path, line)
}

/// Armierte Signatur über `payload` unter `namespace`.
pub fn sign(payload: &[u8], key: &Path, namespace: &str) -> Vec<u8> {
    minds_attest::ssh_sign_ns(std::str::from_utf8(payload).unwrap(), key, namespace)
        .unwrap()
        .into_bytes()
}

/// Eine beschränkte Witness-Zeile.
pub fn witness_line(principal: &str, key: &str) -> String {
    format!("{principal} namespaces=\"minds-witness\" {key}\n")
}

pub fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

pub fn seal_id(byte: u8) -> String {
    format!("b3-{}", hex(&[byte; 32]))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
