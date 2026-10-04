//! EA-09: real SSH signatures and CLI verdicts in isolated repositories.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use minds_core::evidence::{Seal, SealOutcome};
use minds_store::{ContextStore, InRepoStore};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");
const PRINCIPAL: &str = "minds-witness@host";

fn command(root: &Path) -> Command {
    let mut cmd = Command::new(MINDS);
    cmd.current_dir(root)
        .env("HOME", root.join("user-home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("XDG_STATE_HOME");
    cmd
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

struct Fixture {
    dir: tempfile::TempDir,
    store: InRepoStore,
    key: PathBuf,
    signers: PathBuf,
    seal: Seal,
}

impl Fixture {
    fn new(scope: &str) -> Self {
        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "--template="]);
        git(root, &["config", "user.email", "human@example.invalid"]);
        git(root, &["config", "user.name", "Witness Test"]);
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        let store = InRepoStore::open(root).unwrap();
        let session = minds_core::Session::new(
            minds_core::Agent {
                name: "test".into(),
                version: "1".into(),
            },
            minds_core::Model {
                provider: "test".into(),
                id: "test".into(),
            },
            minds_core::Intent::default(),
        );
        let session = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        let session = store.put(&session).unwrap().id();
        let mut seal = Seal::parse(include_str!("fixtures/checkpoint-core/epoch-0.seal")).unwrap();
        seal.scope = scope.into();
        seal.outcome = SealOutcome::Stored {
            session: session.to_string(),
        };
        let key = root.join("id");
        assert!(
            Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let signers = root.join("allowed_signers");
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        std::fs::write(
            &signers,
            format!("{PRINCIPAL} namespaces=\"minds-witness\" {}", public.trim()),
        )
        .unwrap();
        Self {
            dir,
            store,
            key,
            signers,
            seal,
        }
    }

    fn store_seal(&self, namespace: Option<&str>) -> String {
        let payload = self.seal.to_text().unwrap();
        let id = self.store.put_seal(&payload).unwrap();
        if let Some(namespace) = namespace {
            let signature = minds_attest::ssh_sign_ns(&payload, &self.key, namespace).unwrap();
            self.store.put_seal_signature(&id, &signature).unwrap();
        }
        id.to_string()
    }

    fn verify(&self, id: &str, signers: bool, expected_code: i32, expected: &str) {
        let SealOutcome::Stored { session } = &self.seal.outcome else {
            unreachable!()
        };
        // Exercise both the single-seal path and session aggregation.
        for target in [vec!["--evidence", id], vec![session.as_str()]] {
            let mut cmd = command(self.dir.path());
            cmd.arg("verify").args(target);
            if signers {
                cmd.arg("--signers").arg(&self.signers);
            }
            // A human's explicit identity must not replace witness discovery.
            cmd.args(["--identity", "unrelated-human@example.invalid"]);
            let out = cmd.output().unwrap();
            assert_eq!(out.status.code(), Some(expected_code), "{}", text(&out));
            assert!(text(&out).contains(expected), "{}", text(&out));
        }
    }
}

#[test]
fn witness_seal_wrong_namespace_is_tampered() {
    for scope in ["witness/v1", "witness-fs/v1"] {
        let f = Fixture::new(scope);
        // Unrestricted human key: only the wrong signing namespace defeats it.
        let public = std::fs::read_to_string(f.key.with_extension("pub")).unwrap();
        std::fs::write(
            &f.signers,
            format!("human@example.invalid {}", public.trim()),
        )
        .unwrap();
        let id = f.store_seal(Some(minds_attest::NS_DEFAULT));
        f.verify(&id, true, 1, "witness seal not signed under minds-witness");
    }
}

#[test]
fn witness_seal_missing_signature_is_tampered() {
    for scope in ["witness/v1", "witness-fs/v1"] {
        let f = Fixture::new(scope);
        let id = f.store_seal(None);
        for signers in [true, false] {
            f.verify(&id, signers, 1, "witness seal missing signature");
        }
    }
}

#[test]
fn witness_seal_valid_signature() {
    for scope in ["witness/v1", "witness-fs/v1"] {
        let f = Fixture::new(scope);
        let id = f.store_seal(Some(minds_attest::NS_WITNESS));
        f.verify(&id, true, 0, "witness-signed (minds-witness@host)");
        f.verify(&id, false, 0, "signature not checked");
        // Find-principals can return multiple candidates. Try all of them,
        // since the first matching key may be restricted to another role.
        let public = std::fs::read_to_string(f.key.with_extension("pub")).unwrap();
        std::fs::write(
            &f.signers,
            format!(
                "human namespaces=\"minds\" {}\n{PRINCIPAL} namespaces=\"minds-witness\" {}\n",
                public.trim(),
                public.trim()
            ),
        )
        .unwrap();
        f.verify(&id, true, 0, "witness-signed (minds-witness@host)");
        git(
            f.dir.path(),
            &[
                "config",
                "gpg.ssh.allowedSignersFile",
                f.signers.to_str().unwrap(),
            ],
        );
        f.verify(&id, false, 0, "witness-signed (minds-witness@host)");
    }
}

#[test]
fn witness_seal_invalid_or_untrusted_signature_is_tampered() {
    for scope in ["witness/v1", "witness-fs/v1"] {
        let f = Fixture::new(scope);
        let id = f.store_seal(Some(minds_attest::NS_WITNESS));
        // Valid signature, but a trust file that allows no witness.
        std::fs::write(&f.signers, "").unwrap();
        f.verify(&id, true, 1, "witness seal not signed under minds-witness");
        let public = std::fs::read_to_string(f.key.with_extension("pub")).unwrap();
        std::fs::write(&f.signers, format!("{PRINCIPAL} {}", public.trim())).unwrap();
        for signature in [
            "malformed".to_owned(),
            minds_attest::ssh_sign_ns("other payload", &f.key, minds_attest::NS_WITNESS).unwrap(),
        ] {
            f.store
                .put_seal_signature(&id.parse().unwrap(), &signature)
                .unwrap();
            f.verify(&id, true, 1, "witness seal not signed under minds-witness");
        }
    }
}

#[test]
fn witness_unreadable_signers_is_operational_failure() {
    let f = Fixture::new("witness/v1");
    let id = f.store_seal(Some(minds_attest::NS_WITNESS));
    std::fs::remove_file(&f.signers).unwrap();
    f.verify(&id, true, 4, "minds verify:");
}

#[test]
fn witness_keygen_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("witness home");
    let generate = || {
        command(dir.path())
            .args(["witness", "keygen", "--home"])
            .arg(&home)
            .output()
            .unwrap()
    };
    let first = generate();
    assert!(first.status.success(), "{}", text(&first));
    let line = String::from_utf8(first.stdout).unwrap();
    assert_eq!(line.lines().count(), 1);
    assert!(line.starts_with("minds-witness@"));
    assert!(line.contains(" namespaces=\"minds-witness\" ssh-ed25519 "));
    let key = home.join("key/witness_ed25519");
    let public = key.with_extension("pub");
    let before = std::fs::read(&key).unwrap();
    let before_pub = std::fs::read(&public).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&key, &public] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    let signers = dir.path().join("signers");
    std::fs::write(&signers, &line).unwrap();
    let principal = line.split_whitespace().next().unwrap();
    let signature = minds_attest::ssh_sign_ns("seal", &key, minds_attest::NS_WITNESS).unwrap();
    assert!(
        minds_attest::ssh_verify_ns(
            "seal",
            &signature,
            &signers,
            principal,
            minds_attest::NS_WITNESS
        )
        .unwrap()
    );
    let second = generate();
    assert!(!second.status.success());
    assert!(text(&second).contains("refusing to overwrite"));
    assert!(second.stdout.is_empty());
    assert!(
        std::fs::read(&key).unwrap() == before,
        "private key changed"
    );
    assert_eq!(std::fs::read(&public).unwrap(), before_pub);
}

#[test]
fn witness_keygen_home_resolution_and_strict_arguments() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--template="]);
    let state = dir.path().join("state");
    let out = command(dir.path())
        .env("XDG_STATE_HOME", &state)
        .args(["witness", "keygen"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let root = dir.path().canonicalize().unwrap();
    let hash = blake3::hash(root.as_os_str().as_encoded_bytes()).to_hex();
    assert!(
        state
            .join("minds-witness")
            .join(&hash[..16])
            .join("key/witness_ed25519")
            .is_file()
    );
    let env_home = dir.path().join("env-home");
    let explicit = dir.path().join("explicit");
    let out = command(dir.path())
        .env("MINDS_WITNESS_HOME", &env_home)
        .args(["witness", "keygen", "--home"])
        .arg(&explicit)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(!env_home.exists());
    let out = command(dir.path())
        .env("MINDS_WITNESS_HOME", &env_home)
        .args(["witness", "keygen"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(env_home.join("key/witness_ed25519").is_file());
    for args in [
        vec!["witness"],
        vec!["witness", "unknown"],
        vec!["witness", "keygen", "extra"],
        vec!["witness", "keygen", "--force"],
    ] {
        let out = command(dir.path()).args(args).output().unwrap();
        assert!(!out.status.success(), "{}", text(&out));
    }
}

#[cfg(unix)]
#[test]
fn witness_keygen_refuses_symlinks_and_insecure_directories() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for obstruction in [
        "home",
        "home-directory",
        "key",
        "private",
        "public",
        "permissions",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("witness");
        let target = dir.path().join("absent");
        if obstruction == "home-directory" {
            std::fs::create_dir(&target).unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
            symlink(&target, &home).unwrap();
        } else if obstruction == "home" {
            symlink(&target, &home).unwrap();
        } else {
            std::fs::create_dir(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            if obstruction == "key" {
                symlink(&target, home.join("key")).unwrap();
            } else if obstruction == "permissions" {
                std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o777)).unwrap();
            } else {
                std::fs::create_dir(home.join("key")).unwrap();
                std::fs::set_permissions(home.join("key"), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                let name = if obstruction == "private" {
                    "witness_ed25519"
                } else {
                    "witness_ed25519.pub"
                };
                symlink(&target, home.join("key").join(name)).unwrap();
            }
        }
        let out = command(dir.path())
            .args(["witness", "keygen", "--home"])
            .arg(home.join("."))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{obstruction}: {}", text(&out));
        assert!(!target.join("key").exists());
        if obstruction != "home-directory" {
            assert!(!target.exists());
        }
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn concurrent_witness_keygens_publish_one_matching_pair() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("witness");
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            command(dir.path())
                .args(["witness", "keygen", "--home"])
                .arg(&home)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let outputs: Vec<_> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    let successes: Vec<_> = outputs.iter().filter(|o| o.status.success()).collect();
    assert_eq!(successes.len(), 1, "{outputs:?}");
    let signers = dir.path().join("signers");
    std::fs::write(&signers, &successes[0].stdout).unwrap();
    let sig = minds_attest::ssh_sign_ns(
        "seal",
        &home.join("key/witness_ed25519"),
        minds_attest::NS_WITNESS,
    )
    .unwrap();
    let principal = minds_attest::ssh_find_principals(&sig, &signers)
        .unwrap()
        .remove(0);
    assert!(
        minds_attest::ssh_verify_ns("seal", &sig, &signers, &principal, minds_attest::NS_WITNESS)
            .unwrap()
    );
}
