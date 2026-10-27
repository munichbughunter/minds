//! Spec S2 §13 Ebene 1: `s2_23_config_validation` gegen das echte
//! Hook-Binary. Jeder Konfigurationsfehler endet mit Exit 1 und der festen
//! Meldung, auch im Observe-Modus.
#![cfg(unix)]

mod support;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use minds_witness_core::Mode;
use support::*;

struct Run {
    code: i32,
    stderr: String,
}

/// Ruft das Binary so auf, wie receive-pack es täte (cwd = Bare-Repo,
/// `GIT_DIR=.`), mit leerem stdin.
fn run_hook(server: &Repo, config: &Path) -> Run {
    let mut child = Command::new(HOOK_BIN)
        .arg("pre-receive")
        .arg(config)
        .current_dir(&server.git_dir)
        .env("GIT_DIR", ".")
        .env_remove("GIT_QUARANTINE_PATH")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env("HOME", &server.home)
        .env("GIT_CONFIG_GLOBAL", server.home.join("no-such-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn assert_config_error(r: &Run, what: &str) {
    assert_eq!(r.code, 1, "{what}: {}", r.stderr);
    assert_eq!(
        r.stderr, "minds-witness: Konfiguration ungültig\n",
        "{what}"
    );
}

#[test]
fn s2_23_config_validation() {
    let fx = GitTestFixture::new();
    let server = fx.server();
    let observe = policy(Mode::Observe, &[], 100);
    let good = fx.hook_config(&observe);

    // Gegenprobe: gültige Konfiguration, leerer Push → Exit 0, kein stderr.
    let path = fx.write_config("good.json", &good);
    let r = run_hook(&server, &path);
    assert_eq!((r.code, r.stderr.as_str()), (0, ""));

    let variant = |name: &str, f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = good.clone();
        f(&mut v);
        let p = fx.write_config(&format!("{name}.json"), &v);
        assert_config_error(&run_hook(&server, &p), name);
    };

    // Pflichtfelder fehlen.
    for field in [
        "operation",
        "project",
        "signers_file",
        "allowed_witness_principals",
        "ssh_keygen",
        "git",
        "policy",
        "seed_file",
        "key_valid_from",
        "repository",
        "state_dir",
    ] {
        variant(&format!("missing-{field}"), &|v| {
            v.as_object_mut().unwrap().remove(field);
        });
    }
    // Unbekanntes Feld.
    variant("unknown-field", &|v| v["extra"] = serde_json::json!(1));
    // Relative Pfade.
    for field in [
        "signers_file",
        "ssh_keygen",
        "git",
        "seed_file",
        "repository",
        "state_dir",
    ] {
        variant(&format!("relative-{field}"), &|v| {
            v[field] = serde_json::json!("relative/path");
        });
    }
    // Leere Principal-Liste, ungültiges Projekt, ungültige Policy.
    variant("no-principals", &|v| {
        v["allowed_witness_principals"] = serde_json::json!([]);
    });
    variant("bad-project", &|v| {
        v["project"] = serde_json::json!("git.test/Project-42")
    });
    variant("bad-policy", &|v| {
        v["policy"]["coverage_threshold_percent"] = serde_json::json!(101);
    });
    // `require_separated_witness=false` + Enforce (s2_12, Teil Konfiguration).
    variant("unseparated-enforce", &|v| {
        v["require_separated_witness"] = serde_json::json!(false);
        v["policy"]["mode"] = serde_json::json!("enforce");
    });
    // `"operation" != "test"`.
    variant("operation-prod", &|v| {
        v["operation"] = serde_json::json!("production")
    });
    // ssh-keygen nicht in einer root-eigenen Kette.
    let fake = fx.dir.path().join("fake-ssh-keygen");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    variant("ssh-keygen-chain", &|v| {
        v["ssh_keygen"] = serde_json::json!(fake)
    });

    // Seed für andere lesbar, bzw. kein gültiger Seed.
    {
        use std::os::unix::fs::PermissionsExt;
        let seed = fx.dir.path().join("seed-open.hex");
        std::fs::write(&seed, "42".repeat(32)).unwrap();
        std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o644)).unwrap();
        variant("seed-readable", &|v| {
            v["seed_file"] = serde_json::json!(seed)
        });
        let short = fx.dir.path().join("seed-short.hex");
        std::fs::write(&short, "42").unwrap();
        std::fs::set_permissions(&short, std::fs::Permissions::from_mode(0o600)).unwrap();
        variant("seed-short", &|v| v["seed_file"] = serde_json::json!(short));
    }

    // Trust-Datei-Regeln (§9) bei jedem Aufruf auf genau den gelesenen
    // Bytes: Dieselbe Konfiguration wird gültig gestartet, dann wird die
    // Signer-Datei verändert; der nächste Aufruf endet mit Exit 1.
    let signers = Path::new(good["signers_file"].as_str().unwrap()).to_owned();
    let (_, other) = keypair(fx.dir.path(), "dev");
    let witness = fx.public.clone();
    let bad_files = [
        (
            "unrestricted-copy",
            format!("{}dev@test {witness}\n", fx.signers()),
        ),
        ("wildcard", format!("{}* {other}\n", fx.signers())),
        (
            "cert-authority",
            format!("{PRINCIPAL} cert-authority,namespaces=\"minds-witness\" {witness}\n"),
        ),
        (
            "valid-before",
            format!(
                "{PRINCIPAL} namespaces=\"minds-witness\",valid-before=\"20300101\" {witness}\n"
            ),
        ),
        ("same-key-twice", format!("{0}{0}", fx.signers())),
        (
            "two-principals",
            format!(
                "{PRINCIPAL},minds-witness-user@other namespaces=\"minds-witness\" {witness}\nminds-witness-user@other namespaces=\"minds-witness\" {other}\n"
            ),
        ),
        (
            "unreadable-line",
            format!("{PRINCIPAL} namespaces=\"minds-witness\" ssh-ed25519 !!!\n"),
        ),
    ];
    let mut two = good.clone();
    two["allowed_witness_principals"] = serde_json::json!([PRINCIPAL, "minds-witness-user@other"]);
    let path_two = fx.write_config("two.json", &two);
    let two_base = format!(
        "{}minds-witness-user@other namespaces=\"minds-witness\" {other}\n",
        fx.signers()
    );
    for (name, text) in bad_files {
        let (cfg, base) = if name == "two-principals" {
            (&path_two, two_base.clone())
        } else {
            (&path, fx.signers())
        };
        std::fs::write(&signers, base).unwrap();
        let ok = run_hook(&server, cfg);
        assert_eq!(ok.code, 0, "{name} (vorher gültig): {}", ok.stderr);
        std::fs::write(&signers, text).unwrap();
        assert_config_error(&run_hook(&server, cfg), name);
    }
}

/// Spec S3 §3 (Commit 3): Regeln der Betriebsart `pilot`.
#[test]
fn s3_07_pilot_mode_rules() {
    use std::os::unix::fs::PermissionsExt;
    let fx = GitTestFixture::new();
    let server = fx.server();
    let state = fx.dir.path().join("pilot-state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut good = fx.hook_config(&policy(Mode::Observe, &[], 100));
    good["operation"] = serde_json::json!("pilot");
    good["state_dir"] = serde_json::json!(state);
    let write = |name: &str, v: &serde_json::Value, mode: u32| {
        let p = fx.write_config(name, v);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    };
    let ok = run_hook(&server, &write("pilot.json", &good, 0o600));
    assert_eq!((ok.code, ok.stderr.as_str()), (0, ""));

    let bad = |name: &str, f: &dyn Fn(&mut serde_json::Value), mode: u32| {
        let mut v = good.clone();
        f(&mut v);
        let r = run_hook(&server, &write(name, &v, mode));
        assert_config_error(&r, name);
    };
    bad(
        "pilot-switch",
        &|v| v["test_switches"] = serde_json::json!({"skip_quarantine": true}),
        0o600,
    );
    bad(
        "pilot-memory",
        &|v| v["server_kind"] = serde_json::json!("memory"),
        0o600,
    );
    bad("pilot-config-writable", &|_| {}, 0o620);
    // Zustandsverzeichnis für andere zugänglich.
    let open = fx.dir.path().join("open-state");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    bad(
        "pilot-open-state",
        &|v| v["state_dir"] = serde_json::json!(open),
        0o600,
    );
    // Fehlendes Zustandsverzeichnis wird im Pilotbetrieb nicht angelegt.
    let missing = fx.dir.path().join("missing-state");
    bad(
        "pilot-missing-state",
        &|v| v["state_dir"] = serde_json::json!(missing),
        0o600,
    );
    // Repository passt nicht zu GIT_DIR.
    bad(
        "pilot-other-repo",
        &|v| v["repository"] = serde_json::json!(fx.dir.path().join("other.git")),
        0o600,
    );
    // Seed für andere lesbar (gilt in beiden Betriebsarten).
    let seed = fx.dir.path().join("seed-open.hex");
    std::fs::write(&seed, "42".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o644)).unwrap();
    bad(
        "pilot-seed",
        &|v| v["seed_file"] = serde_json::json!(seed),
        0o600,
    );
    // Konfiguration für andere lesbar.
    bad("pilot-config-readable", &|_| {}, 0o640);
    // Zu kurze Verfallszeit.
    bad(
        "pilot-short-ttl",
        &|v| v["pending_ttl_secs"] = serde_json::json!(59),
        0o600,
    );
    // Trust-Datei für andere beschreibbar.
    let signers = fx.dir.path().join("signers-open");
    std::fs::copy(good["signers_file"].as_str().unwrap(), &signers).unwrap();
    std::fs::set_permissions(&signers, std::fs::Permissions::from_mode(0o666)).unwrap();
    bad(
        "pilot-signers-writable",
        &|v| v["signers_file"] = serde_json::json!(signers),
        0o600,
    );
    // Trust-Datei als Symlink.
    let link = fx.dir.path().join("signers-link");
    std::os::unix::fs::symlink(good["signers_file"].as_str().unwrap(), &link).unwrap();
    bad(
        "pilot-signers-symlink",
        &|v| v["signers_file"] = serde_json::json!(link),
        0o600,
    );
    // Seed in einem für andere beschreibbaren Verzeichnis (ohne Sticky-Bit).
    let shared = fx.dir.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    let shared_seed = shared.join("seed.hex");
    std::fs::copy(good["seed_file"].as_str().unwrap(), &shared_seed).unwrap();
    std::fs::set_permissions(&shared_seed, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    bad(
        "pilot-seed-dir-writable",
        &|v| v["seed_file"] = serde_json::json!(shared_seed),
        0o600,
    );
    // Mit Sticky-Bit (wie /tmp) zulässig.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let mut sticky = good.clone();
    sticky["seed_file"] = serde_json::json!(shared_seed);
    let r = run_hook(&server, &write("pilot-sticky.json", &sticky, 0o600));
    assert_eq!((r.code, r.stderr.as_str()), (0, ""));
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Konfiguration in einem für andere beschreibbaren Verzeichnis.
    let open_cfg_dir = fx.dir.path().join("open-cfg");
    std::fs::create_dir_all(&open_cfg_dir).unwrap();
    std::fs::set_permissions(&open_cfg_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let cfg = open_cfg_dir.join("pilot.json");
    std::fs::write(&cfg, serde_json::to_vec(&good).unwrap()).unwrap();
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_config_error(&run_hook(&server, &cfg), "pilot-config-dir-writable");
    std::fs::set_permissions(&open_cfg_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Im Testbetrieb ist `memory` erlaubt.
    let mut test = fx.hook_config(&policy(Mode::Observe, &[], 100));
    test["server_kind"] = serde_json::json!("memory");
    let r = run_hook(&server, &write("test-memory.json", &test, 0o644));
    assert_eq!(r.code, 0, "{}", r.stderr);
}
