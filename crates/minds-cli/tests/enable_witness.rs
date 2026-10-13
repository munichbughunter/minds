//! EA-10: `minds enable --witness <profile>` in isolierten Repositories.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MINDS: &str = env!("CARGO_BIN_EXE_minds");

/// Das Binary, mit dem diese Tests `enable --witness` aufrufen: eine eigene
/// Kopie mit genau einem Namen. Unter Linux ist `target/debug/minds` ein
/// harter Link auf `target/debug/deps/minds-<hash>` — `enable --witness`
/// lehnt ein beschreibbares Binary mit mehreren Namen zu Recht ab (ein
/// zweiter Name könnte dort liegen, wo der Agent schreibt). Ein installiertes
/// `minds` hat einen Namen; so auch diese Kopie.
fn minds() -> &'static Path {
    static COPY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    COPY.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("enable-witness-bin");
        fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("minds");
        // Erst unter eigenem Namen kopieren, dann umbenennen: Ein parallel
        // laufender Testprozess sieht nie eine halb geschriebene Datei.
        let temp = dir.join(format!("minds.{}", std::process::id()));
        fs::copy(MINDS, &temp).unwrap();
        fs::rename(&temp, &copy).unwrap();
        copy
    })
}

fn template(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("templates/witness")
            .join(name),
    )
    .unwrap()
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
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Kurze Pfade: Der Socket im Witness-Home unterliegt sockaddr_un.
        let dir = tempfile::Builder::new()
            .prefix("mew-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("demo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "--template="]);
        git(&root, &["config", "user.name", "Witness Test"]);
        git(&root, &["config", "user.email", "witness@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        git(&root, &["commit", "-qm", "initial", "--allow-empty"]);
        for name in ["user-home", "state", "config"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        Self { dir, root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn minds(&self, args: &[&str]) -> Output {
        {
            let mut minds = Command::new(minds());
            minds
                .env("MINDS_WITNESS", "off")
                .env_remove("MINDS_WITNESS_HOME")
                .env_remove("MINDS_WITNESS_SOCKET");
            minds
        }
        .current_dir(&self.root)
        .args(args)
        .env("HOME", self.path("user-home"))
        .env("XDG_STATE_HOME", self.path("state"))
        .env("XDG_CONFIG_HOME", self.path("config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("MINDS_WITNESS_HOME")
        .env_remove("MINDS_WITNESS_SOCKET")
        .output()
        .unwrap()
    }

    /// `minds enable` ohne `--witness`, mit `MINDS_WITNESS=<mode>`.
    fn enable_default(&self, mode: &str) -> Output {
        self.enable_default_with(mode, &[])
    }

    /// Wie [`Self::enable_default`], mit weiteren Variablen.
    fn enable_default_with(&self, mode: &str, vars: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(minds());
        cmd.env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        for (key, value) in vars {
            cmd.env(key, value);
        }
        cmd.current_dir(&self.root)
            .args(["enable", "--agent", "claude-code"])
            .env("MINDS_WITNESS", mode)
            .env("HOME", self.path("user-home"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    fn enable(&self, profile: &str) -> Output {
        self.minds(&["enable", "--agent", "claude-code", "--witness", profile])
    }

    /// Das einzige Witness-Home unter `$XDG_STATE_HOME/minds-witness`.
    fn home(&self) -> PathBuf {
        let homes: Vec<_> = fs::read_dir(self.path("state").join("minds-witness"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(homes.len(), 1, "{homes:?}");
        homes[0].clone()
    }

    /// `<repo-id>` wie in 00-conventions: die ersten 16 Hex von
    /// `blake3(kanonische Repo-Wurzel)`.
    fn repo_id(&self) -> String {
        blake3::hash(self.root.as_os_str().as_encoded_bytes()).to_hex()[..16].to_owned()
    }

    fn service_parent(&self) -> PathBuf {
        self.service().parent().unwrap().to_path_buf()
    }

    fn service(&self) -> PathBuf {
        let id = self.repo_id();
        if cfg!(target_os = "macos") {
            self.path("user-home")
                .join("Library/LaunchAgents")
                .join(format!("dev.minds.witness.{id}.plist"))
        } else {
            self.path("config")
                .join("systemd/user")
                .join(format!("minds-witness-{id}.service"))
        }
    }

    /// Ein Ersatz für den Dienstverwalter (`MINDS_SERVICE_MANAGER_STUB`):
    /// protokolliert jeden Aufruf nach `<name>.log`, kennt keinen geladenen
    /// launchd-Job und meldet für `systemctl show` unsere Dienstdatei mit
    /// `drop_ins`.
    fn manager_stub(&self, name: &str, drop_ins: &str) -> PathBuf {
        let stub = self.path(name);
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{log}'\n\
                 case \"$1 $2\" in 'launchctl print') exit 1;; esac\n\
                 case \"$3\" in show) echo 'FragmentPath={unit}'; echo 'DropInPaths={drop_ins}';; esac\n\
                 exit 0\n",
                log = self.path(&format!("{name}.log")).display(),
                unit = self.service().display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        stub
    }

    fn service_proposal(&self) -> PathBuf {
        let service = self.service();
        let mut name = service.file_name().unwrap().to_owned();
        name.push(".minds-proposed");
        service.with_file_name(name)
    }
}

fn proposals(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().contains(".minds-proposed") {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn enable_witness_container_writes_templates() {
    let f = Fixture::new();
    let out = f.enable("container");
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        fs::read_to_string(f.root.join(".devcontainer/devcontainer.json")).unwrap(),
        template("devcontainer.json")
    );
    assert_eq!(
        fs::read_to_string(f.root.join(".devcontainer/compose.yaml")).unwrap(),
        template("compose.yaml")
    );

    // Das Witness-Home außerhalb des Repos, Profil und Pfadabbildung
    // festgehalten, samt Pins.
    let home = f.home();
    assert!(!home.starts_with(&f.root));
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join("witness.json")).unwrap()).unwrap();
    assert_eq!(config["profile"], "container");
    assert_eq!(config["schema_version"], 2);
    assert_eq!(config["path_map"][0][0], "/workspaces/demo");
    assert_eq!(config["path_map"][0][1], f.root.to_str().unwrap());
    assert_eq!(config["store"]["backend"], "in-repo");
    assert!(config["git_dir"].as_str().unwrap().ends_with("/demo/.git"));
    assert!(config["redaction"].is_object());
    assert!(home.join("key/witness_ed25519").is_file());

    // Der Nutzerdienst: geschrieben, nicht gestartet — mit dem kanonischen
    // Home, das auch Compose bindet.
    let home = home.canonicalize().unwrap();
    let service = fs::read_to_string(f.service()).unwrap();
    assert!(service.contains("witness"), "{service}");
    assert!(service.contains(home.to_str().unwrap()), "{service}");
    assert!(
        stdout.contains(&format!("MINDS_WITNESS_HOME='{}'", home.display())),
        "{stdout}"
    );
    if cfg!(target_os = "macos") {
        if let Ok(lint) = Command::new("plutil")
            .arg("-lint")
            .arg(f.service())
            .output()
        {
            assert!(lint.status.success(), "{}", text(&lint));
        }
    }
    assert!(
        stdout.contains("minds does not start it"),
        "Start-Befehl gedruckt: {stdout}"
    );

    // allowed_signers-Zeile und wohin damit.
    assert!(
        stdout.contains("namespaces=\"minds-witness\" ssh-ed25519 "),
        "{stdout}"
    );
    assert!(stdout.contains("outside the repository"), "{stdout}");
    assert!(stdout.contains("MINDS_WITNESS_HOME="), "{stdout}");
    assert!(stdout.contains("minds doctor --probe-home"), "{stdout}");

    // Die gewöhnlichen Schritte liefen auch.
    assert!(f.root.join(".claude/settings.json").is_file());
    assert!(f.root.join(".git/hooks/post-commit").is_file());
    assert!(proposals(&f.root).is_empty());
}

#[test]
fn enable_witness_is_idempotent() {
    let f = Fixture::new();
    let first = f.enable("container");
    assert!(first.status.success(), "{}", text(&first));
    let snapshot = |f: &Fixture| {
        [
            f.root.join(".devcontainer/devcontainer.json"),
            f.root.join(".devcontainer/compose.yaml"),
            f.home().join("witness.json"),
            f.home().join("key/witness_ed25519.pub"),
            f.service(),
        ]
        .map(|path| fs::read(path).unwrap())
    };
    let before = snapshot(&f);

    let second = f.enable("container");
    assert!(second.status.success(), "{}", text(&second));
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert_eq!(snapshot(&f), before);
    assert!(stdout.contains("(unchanged)"), "home: {stdout}");
    assert!(stdout.contains("key        present"), "{stdout}");
    assert_eq!(stdout.matches("  unchanged").count(), 2, "{stdout}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("  service ") && line.ends_with(" unchanged")),
        "{stdout}"
    );
    assert!(proposals(&f.root).is_empty());
    assert!(!f.service_proposal().exists());
    // Dieselbe allowed_signers-Zeile wie beim ersten Lauf.
    let line = |out: &Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.contains("namespaces=\"minds-witness\""))
            .unwrap()
            .to_owned()
    };
    assert_eq!(line(&first), line(&second));
}

#[test]
fn enable_witness_never_overwrites_devcontainer() {
    let f = Fixture::new();
    let own = "{ \"name\": \"our own dev container\" }\n";
    fs::create_dir(f.root.join(".devcontainer")).unwrap();
    fs::write(f.root.join(".devcontainer/devcontainer.json"), own).unwrap();

    for _ in 0..2 {
        let out = f.enable("container");
        assert!(out.status.success(), "{}", text(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("exists and differs — left untouched, proposal in .devcontainer/devcontainer.json.minds-proposed"),
            "{stdout}"
        );
        assert_eq!(
            fs::read_to_string(f.root.join(".devcontainer/devcontainer.json")).unwrap(),
            own
        );
        assert_eq!(
            fs::read_to_string(
                f.root
                    .join(".devcontainer/devcontainer.json.minds-proposed")
            )
            .unwrap(),
            template("devcontainer.json")
        );
        assert_eq!(
            fs::read_to_string(f.root.join(".devcontainer/compose.yaml")).unwrap(),
            template("compose.yaml")
        );
    }
}

#[test]
fn enable_witness_never_overwrites_a_service_file() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let service = f.service();
    fs::write(&service, "# hand-tuned\n").unwrap();

    let out = f.enable("container");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&service).unwrap(), "# hand-tuned\n");
    assert!(
        fs::read_to_string(f.service_proposal())
            .unwrap()
            .contains("witness"),
        "proposal next to the service file"
    );
}

/// Ein Binary im Repo könnte der Agent im Container ersetzen — der Dienst
/// liefe es dann auf dem Host. `enable --witness` lehnt es ab.
#[test]
fn enable_witness_refuses_a_binary_inside_the_repository() {
    let f = Fixture::new();
    let inside = f.root.join("target/release/minds");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::copy(MINDS, &inside).unwrap();
    let out = {
        let mut minds = Command::new(&inside);
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["enable", "--agent", "claude-code", "--witness", "container"])
    .env("HOME", f.path("user-home"))
    .env("XDG_STATE_HOME", f.path("state"))
    .env("XDG_CONFIG_HOME", f.path("config"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_HOME")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("lies inside the repository"),
        "{}",
        text(&out)
    );
    assert!(!f.path("state").join("minds-witness").exists());
    assert!(!f.root.join(".devcontainer").exists());
}

/// Der Schutzblock der `user`-Schritte stoppt bei einem Symlink, bevor
/// irgendein chgrp/chmod läuft — ausgeführt, nicht nur gelesen. Und kein
/// solcher Befehl auf das Repo steht außerhalb des Blocks.
#[test]
fn the_user_steps_stop_before_chmod_on_a_symlinked_repository() {
    let steps = template("user-profile.txt");
    let opener = "sh -euc '";
    let start = steps.find(opener).unwrap() + opener.len();
    let end = steps[start..].find("' sh {{ROOT}}").unwrap() + start;
    let script = &steps[start..end];
    let outside = format!("{}{}", &steps[..start], &steps[end..]);
    for line in outside.lines() {
        let on_repo = line.contains("{{ROOT}}");
        for command in ["chgrp", "chmod", "find"] {
            assert!(
                !(on_repo && line.contains(command)),
                "{command} on the repository outside the guarded block: {line}"
            );
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("etc-like");
    fs::create_dir_all(target.join(".git")).unwrap();
    let link = dir.path().join("repo");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let out = Command::new("sh")
        .args(["-euc", script, "sh"])
        .arg(&link)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("stop:"));
}

/// Die übrigen Zweige des Schutzblocks: `.git` als Symlink, `.git` fehlt.
#[test]
fn the_user_steps_stop_on_a_symlinked_or_missing_git_dir() {
    let steps = template("user-profile.txt");
    let opener = "sh -euc '";
    let start = steps.find(opener).unwrap() + opener.len();
    let end = steps[start..].find("' sh {{ROOT}}").unwrap() + start;
    let script = &steps[start..end];
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let linked = dir.path().join("linked");
    fs::create_dir(&linked).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join(".git")).unwrap();
    let missing = dir.path().join("missing");
    fs::create_dir(&missing).unwrap();
    for repo in [&linked, &missing] {
        let out = Command::new("sh")
            .args(["-euc", script, "sh"])
            .arg(repo)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert!(String::from_utf8_lossy(&out.stderr).contains("stop:"));
    }
}

/// Code, den ein `git` auf dem Host ausführte: ein fremder Hook, ein
/// `core.fsmonitor`. Beides meldet die Heuristik als warn — es könnten auch
/// die eigenen des Menschen sein.
#[test]
fn doctor_warns_on_host_code_in_git() {
    for plant in ["hook", "fsmonitor"] {
        let f = Fixture::new();
        assert!(f.enable("container").status.success());
        let before = f.minds(&["doctor"]);
        assert!(
            String::from_utf8_lossy(&before.stdout).contains("ok    host code in .git:"),
            "{}",
            text(&before)
        );
        match plant {
            "hook" => {
                fs::write(
                    f.root.join(".git/hooks/post-checkout"),
                    "#!/bin/sh\ncat ~/.ssh/id_ed25519\n",
                )
                .unwrap();
            }
            _ => git(&f.root, &["config", "core.fsmonitor", "/tmp/agent-code"]),
        }
        let out = f.minds(&["doctor"]);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            stdout
                .lines()
                .any(|line| line.starts_with("warn  host code in .git:")),
            "{plant}: {stdout}"
        );
    }
}

/// Ein `core.worktree` aus der Hand des Agenten lenkt den Weg zum Home
/// nicht mehr um — `doctor` prüft die Grenze weiter.
#[test]
fn doctor_finds_the_home_despite_a_planted_core_worktree() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    git(&f.root, &["config", "core.worktree", "/"]);
    let out = f.minds(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("isolation boundary:"),
        "host side still checked: {stdout}"
    );
}

/// Ein Witness-Container ohne auffindbares Home ist fail, nicht „nicht
/// eingerichtet".
#[test]
fn doctor_fails_when_a_witness_container_has_no_home() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    fs::rename(f.path("state"), f.path("state-moved")).unwrap();
    fs::create_dir(f.path("state")).unwrap();
    let out = f.minds(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("fail  witness: a witness dev container is set up")),
        "{stdout}"
    );
}

/// Genau die beiden Vorschlagsnamen, als gewöhnliche Dateien: ok. Ein
/// Verzeichnis oder ein Symlink unter diesem Namen: fail.
#[test]
fn doctor_accepts_only_regular_proposal_files() {
    let boundary = |f: &Fixture| {
        let out = f.minds(&["doctor"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.contains("isolation boundary:"))
            .unwrap_or_default()
            .to_owned()
    };
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    fs::write(
        f.root
            .join(".devcontainer/devcontainer.json.minds-proposed"),
        template("devcontainer.json"),
    )
    .unwrap();
    assert!(boundary(&f).starts_with("ok "), "{}", boundary(&f));

    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let dir = f
        .root
        .join(".devcontainer/devcontainer.json.minds-proposed");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("devcontainer.json"), "{}").unwrap();
    assert!(boundary(&f).starts_with("fail"), "{}", boundary(&f));

    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    std::os::unix::fs::symlink(
        f.root.join(".devcontainer/compose.yaml"),
        f.root.join(".devcontainer/compose.yaml.minds-proposed"),
    )
    .unwrap();
    assert!(boundary(&f).starts_with("fail"), "{}", boundary(&f));
}

/// Ein hart verlinktes Binary teilt den Inode mit einem anderen Namen, der im
/// Repo liegen kann — abgelehnt.
#[test]
fn enable_witness_refuses_a_hard_linked_binary() {
    let f = Fixture::new();
    let bin = f.path("bin");
    fs::create_dir(&bin).unwrap();
    let copy = bin.join("minds");
    fs::copy(MINDS, &copy).unwrap();
    fs::hard_link(&copy, bin.join("other-name")).unwrap();
    let out = {
        let mut minds = Command::new(&copy);
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["enable", "--agent", "claude-code", "--witness", "container"])
    .env("HOME", f.path("user-home"))
    .env("XDG_STATE_HOME", f.path("state"))
    .env("XDG_CONFIG_HOME", f.path("config"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_HOME")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("more than one hard link"),
        "{}",
        text(&out)
    );
}

/// Die Isolationsgrenze steht in Dateien, die der Agent ändern kann:
/// `doctor` auf dem Host meldet eine Abweichung von den Vorlagen als fail.
#[test]
fn doctor_fails_when_the_isolation_boundary_changes() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let line = |out: &Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.contains("isolation boundary:"))
            .unwrap_or_default()
            .to_owned()
    };
    let before = f.minds(&["doctor"]);
    assert!(line(&before).starts_with("ok "), "{}", text(&before));

    let compose = f.root.join(".devcontainer/compose.yaml");
    let mut widened = fs::read_to_string(&compose).unwrap();
    widened.push_str("    privileged: true\n");
    fs::write(&compose, widened).unwrap();
    let after = f.minds(&["doctor"]);
    assert_eq!(after.status.code(), Some(1), "{}", text(&after));
    assert!(line(&after).starts_with("fail"), "{}", text(&after));
    assert!(line(&after).contains("compose.yaml"), "{}", text(&after));
}

#[test]
fn doctor_refuses_an_empty_probe_home() {
    let f = Fixture::new();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["doctor", "--probe-home", ""])
    .env("HOME", f.path("user-home"))
    .env("XDG_STATE_HOME", f.path("state"))
    .env_remove("MINDS_WITNESS_HOME")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("--probe-home is empty"),
        "{}",
        text(&out)
    );
}

#[test]
fn enable_witness_refuses_a_symlinked_devcontainer_directory() {
    let f = Fixture::new();
    let elsewhere = f.path("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join(".devcontainer")).unwrap();
    let out = f.enable("container");
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("is a symlink"), "{}", text(&out));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn enable_witness_refuses_a_home_inside_the_repository() {
    let f = Fixture::new();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["enable", "--agent", "claude-code", "--witness", "container"])
    .env("HOME", f.path("user-home"))
    .env("XDG_CONFIG_HOME", f.path("config"))
    .env("MINDS_WITNESS_HOME", f.root.join("witness"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("must lie outside the repository"),
        "{}",
        text(&out)
    );
    assert!(!f.root.join("witness").exists());
}

#[test]
fn enable_witness_rejects_an_unknown_profile_before_writing() {
    let f = Fixture::new();
    let out = f.enable("root");
    assert!(!out.status.success());
    assert!(
        text(&out).contains("unknown witness profile"),
        "{}",
        text(&out)
    );
    assert!(!f.root.join(".claude").exists(), "nichts halb eingerichtet");
}

#[test]
fn enable_witness_user_prints_the_steps_and_runs_nothing() {
    let f = Fixture::new();
    let out = f.enable("user");
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for step in [
        "useradd --system",
        "groupadd --system minds-agents",
        "--profile user --socket-group minds-agents",
        "User=minds-witness",
        "SupplementaryGroups=minds-agents",
        "systemctl enable --now minds-witness-",
        "MINDS_WITNESS_SOCKET=",
    ] {
        assert!(stdout.contains(step), "missing {step:?}: {stdout}");
    }
    assert!(!stdout.contains("{{"), "{stdout}");
    // Nichts eingerichtet: kein Witness-Home, kein Dienst, kein Container.
    assert!(!f.path("state").join("minds-witness").exists());
    assert!(!f.root.join(".devcontainer").exists());
}

#[test]
fn enable_witness_managed_writes_only_the_proposal() {
    let f = Fixture::new();
    let out = f.enable("managed");
    assert!(out.status.success(), "{}", text(&out));
    let path = f.root.join("managed-settings.minds-proposed.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(value["$comment"].as_str().unwrap().contains("A1"));
    assert_eq!(value["allowManagedHooksOnly"], true);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("yields A1"),
        "{}",
        text(&out)
    );
    assert!(!f.path("state").join("minds-witness").exists());
    assert!(!f.root.join(".devcontainer").exists());

    let before = fs::read(&path).unwrap();
    let again = f.enable("managed");
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("unchanged"));
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// AC: Kein Kommando dieser Spec ruft `sudo` auf. Die EA-10-Module nennen es
/// nicht einmal; die gedruckten Schritte des `user`-Profils stehen als Text
/// in einer Vorlage. Und nirgends im CLI-Quelltext wird `sudo` als Programm
/// oder Argument übergeben.
#[test]
fn no_sudo_in_enable_witness() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for module in [
        "enable_witness.rs",
        "doctor.rs",
        "witness_cmd.rs",
        "witness_cmd/daemon/worker.rs",
    ] {
        let source = fs::read_to_string(src.join(module)).unwrap();
        assert!(!source.contains("sudo"), "{module} mentions sudo");
    }
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                assert!(
                    !source.contains("\"sudo\""),
                    "{} passes sudo to a process",
                    path.display()
                );
            }
        }
    }
    // Die Schritte selbst nennen sudo — als Text zum Lesen.
    assert!(template("user-profile.txt").contains("sudo "));
}

/// Neue Dateien weiten die Grenze, ohne die Vorlagen anzufassen: eine
/// `.env` (erfüllt Composes `${…:?}`), eine zweite `devcontainer.json`, eine
/// `.devcontainer.json` an der Wurzel. Jede ist ein fail.
#[test]
fn doctor_fails_on_extra_devcontainer_files() {
    for extra in [
        ".devcontainer/.env",
        ".devcontainer/alt/devcontainer.json",
        ".devcontainer/x.minds-proposed/devcontainer.json",
        ".devcontainer.json",
    ] {
        let f = Fixture::new();
        assert!(f.enable("container").status.success());
        let path = f.root.join(extra);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "MINDS_WITNESS_HOME=/\n").unwrap();
        let out = f.minds(&["doctor"]);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(out.status.code(), Some(1), "{extra}: {}", text(&out));
        let line = stdout
            .lines()
            .find(|line| line.contains("isolation boundary:"))
            .unwrap_or_default();
        assert!(line.starts_with("fail"), "{extra}: {stdout}");
    }
}

/// Weicht eine vorhandene Unit ab, druckt `enable` keinen Befehl, der sie
/// ungeprüft startete, ohne zuerst auf den Vorschlag zu verweisen.
#[test]
fn enable_witness_points_at_the_proposal_before_any_start_command() {
    let f = Fixture::new();
    fs::create_dir_all(f.service_parent()).unwrap();
    fs::write(f.service(), "# old unit\n").unwrap();
    let out = f.enable("container");
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("minds does not start it"), "{stdout}");
    assert!(
        stdout.contains("differs from what minds would write"),
        "{stdout}"
    );
    assert!(stdout.contains(".minds-proposed"), "{stdout}");
}

/// Eine Dienstdatei im Repo könnte der Agent umschreiben.
#[test]
fn enable_witness_refuses_a_service_file_inside_the_repository() {
    let f = Fixture::new();
    let inside = f.root.join("dotfiles");
    fs::create_dir_all(&inside).unwrap();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["enable", "--agent", "claude-code", "--witness", "container"])
    .env("HOME", &inside)
    .env("XDG_CONFIG_HOME", inside.join(".config"))
    .env("XDG_STATE_HOME", f.path("state"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_HOME")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("service file would lie inside the repository"),
        "{}",
        text(&out)
    );
    assert!(
        !f.path("state").join("minds-witness").exists(),
        "nichts halb"
    );
}

#[test]
fn enable_witness_refuses_an_equals_sign_in_the_directory_name() {
    let dir = tempfile::Builder::new()
        .prefix("mew-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().join("de=mo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "--template="]);
    fs::create_dir(dir.path().join("state")).unwrap();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&root)
    .args(["enable", "--agent", "claude-code", "--witness", "container"])
    .env("HOME", dir.path())
    .env("XDG_STATE_HOME", dir.path().join("state"))
    .env("XDG_CONFIG_HOME", dir.path().join("config"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_HOME")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("contains '='"), "{}", text(&out));
    assert_eq!(fs::read_dir(dir.path().join("state")).unwrap().count(), 0);
}

/// Ein halbes Schlüsselpaar ist weder „vorhanden" noch wird es ergänzt.
#[test]
fn enable_witness_refuses_an_incomplete_key_pair() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let key = f.home().join("key");
    fs::remove_file(key.join("witness_ed25519")).unwrap();
    let out = f.enable("container");
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("key pair is incomplete"),
        "{}",
        text(&out)
    );
    assert!(!key.join("witness_ed25519").exists());
}

/// `managed` lehnt ab, was `container` ablehnt, und druckt den Hash des
/// Vorschlags als Anker für den Administrator.
#[test]
fn enable_witness_managed_refuses_unsafe_paths_and_prints_a_digest() {
    let f = Fixture::new();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["enable", "--agent", "claude-code", "--witness", "managed"])
    .env("HOME", f.path("user-home"))
    .env("MINDS_WITNESS_HOME", f.root.join("witness"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env_remove("MINDS_WITNESS_SOCKET")
    .output()
    .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("outside the repository"),
        "{}",
        text(&out)
    );
    assert!(!f.root.join("managed-settings.minds-proposed.json").exists());

    let out = f.enable("managed");
    assert!(out.status.success(), "{}", text(&out));
    let content = fs::read(f.root.join("managed-settings.minds-proposed.json")).unwrap();
    let digest = blake3::hash(&content).to_hex().to_string();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&digest),
        "{}",
        text(&out)
    );
}

/// Host-Seite: Ein Home ohne Pins (Schema 1) ist fail, nicht warn.
#[test]
fn doctor_fails_on_an_unpinned_witness_home() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let path = f.home().join("witness.json");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let object = config.as_object_mut().unwrap();
    object.insert("schema_version".into(), 1.into());
    for pin in ["git_dir", "store", "redaction"] {
        object.remove(pin);
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::set_permissions(&temp, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    fs::rename(&temp, &path).unwrap();
    let out = f.minds(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("fail  witness pins: ")),
        "{stdout}"
    );
}

/// Dasselbe Home aus Variable und Standardpfad: eine Zeile, nicht zwei.
#[test]
fn doctor_probes_a_default_home_once() {
    let f = Fixture::new();
    assert!(f.enable("container").status.success());
    let home = f.home();
    let out = {
        let mut minds = Command::new(minds());
        minds
            .env("MINDS_WITNESS", "off")
            .env_remove("MINDS_WITNESS_HOME")
            .env_remove("MINDS_WITNESS_SOCKET");
        minds
    }
    .current_dir(&f.root)
    .args(["doctor"])
    .env("HOME", f.path("user-home"))
    .env("XDG_STATE_HOME", f.path("state"))
    .env("MINDS_WITNESS_HOME", &home)
    .env("MINDS_WITNESS_SOCKET", home.join("run/witness.sock"))
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .output()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.contains("isolation:"))
            .count(),
        1,
        "{stdout}"
    );
}

/// Ohne `--witness` richtet `enable` immer einen Witness ein (Profil
/// `local`): Home, Schlüssel, Nutzerdienst — und warnt, dass er ungeschützt
/// läuft. `MINDS_WITNESS=print` startet ihn nicht, sondern druckt den Start.
#[test]
fn enable_sets_up_a_local_witness_by_default() {
    let f = Fixture::new();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("witness set up (profile local)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("warning: the witness runs unprotected under your user ID"),
        "{stderr}"
    );
    assert!(!stderr.contains('\x1b'), "NO_COLOR: keine Farbe: {stderr}");
    let home = f.home();
    let config = fs::read_to_string(home.join("witness.json")).unwrap();
    assert!(config.contains("\"local\""), "{config}");
    assert!(home.join("key/witness_ed25519").exists());
    assert!(f.service().exists(), "Dienstdatei geschrieben");
    // Ein zweites `enable` ist idempotent.
    let again = f.enable_default("print");
    assert!(again.status.success(), "{}", text(&again));
}

/// `MINDS_WITNESS=off` lässt den Witness weg; ein unbekannter Wert ist ein
/// Fehler, bevor irgendetwas geschrieben wird.
#[test]
fn enable_without_witness_and_unknown_mode() {
    let f = Fixture::new();
    let bad = f.enable_default("sometimes");
    assert!(!bad.status.success(), "{}", text(&bad));
    assert!(text(&bad).contains("MINDS_WITNESS must be on, print or off"));
    assert!(!f.root.join(".claude").exists(), "nichts geschrieben");
    assert!(!f.path("state").join("minds-witness").exists());
    let out = f.enable_default("off");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!f.path("state").join("minds-witness").exists());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("warning"));
}

/// Ein Witness-Home, das ein anderes Repo pinnt (ein global gesetztes
/// `MINDS_WITNESS_HOME`), wird nicht übernommen: Warnung, kein „running".
#[test]
fn enable_refuses_a_witness_home_of_another_repository() {
    let a = Fixture::new();
    let shared = a.path("shared-home");
    let out = a.enable_default_with("print", &[("MINDS_WITNESS_HOME", shared.to_str().unwrap())]);
    assert!(out.status.success(), "{}", text(&out));
    let b = Fixture::new();
    let out = b.enable_default_with("print", &[("MINDS_WITNESS_HOME", shared.to_str().unwrap())]);
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("belongs to another repository"), "{stderr}");
    assert!(!stderr.contains("witness set up"), "{stderr}");
}

/// Scheitert der Dienstverwalter, warnt `enable` mit seiner Meldung und
/// bleibt erfolgreich; antwortet danach kein Witness, ebenso.
#[test]
fn enable_warns_when_the_witness_cannot_be_started() {
    let f = Fixture::new();
    let stub = f.path("stub.sh");
    fs::write(&stub, "#!/bin/sh\necho 'no session bus' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = f.enable_default_with(
        "on",
        &[("MINDS_SERVICE_MANAGER_STUB", stub.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: witness not set up for this repository"),
        "{stderr}"
    );
    assert!(stderr.contains("no session bus"), "{stderr}");
    assert!(!stderr.contains("witness running"), "{stderr}");

    // Der Dienstverwalter gelingt (systemd: die Unit aus genau unserer Datei,
    // ohne Drop-ins), aber kein Witness antwortet.
    let ok = f.manager_stub("ok.sh", "");
    let out = f.enable_default_with(
        "on",
        &[("MINDS_SERVICE_MANAGER_STUB", ok.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not answer"), "{stderr}");
    // Der Ersatz bekam den Namen des Programms, das er ersetzt.
    let calls = fs::read_to_string(f.path("ok.sh.log")).unwrap();
    let manager = if cfg!(target_os = "macos") {
        "launchctl bootstrap gui/"
    } else {
        "systemctl --user restart minds-witness-"
    };
    assert!(calls.contains(manager), "{calls}");
}

/// systemd: Eine Unit mit Drop-ins (oder aus einer anderen Datei) startet
/// `enable` nicht — es warnt und bleibt erfolgreich.
#[cfg(not(target_os = "macos"))]
#[test]
fn enable_refuses_a_unit_with_drop_ins() {
    let f = Fixture::new();
    let stub = f.manager_stub("dropin.sh", "/etc/systemd/user/x.service.d/evil.conf");
    let out = f.enable_default_with(
        "on",
        &[("MINDS_SERVICE_MANAGER_STUB", stub.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("has drop-ins or comes from another file"),
        "{stderr}"
    );
    let calls = fs::read_to_string(f.path("dropin.sh.log")).unwrap();
    assert!(!calls.contains("restart"), "{calls}");
}

/// Ein Home unter der eigenen Nutzerkennung, das Profil `user` behauptet
/// (ein `user`-Witness läuft unter eigenem Nutzer — der Agent könnte die
/// Datei geschrieben haben): Warnung, nichts geändert, nichts gestartet.
#[test]
fn enable_warns_about_a_same_uid_home_claiming_profile_user() {
    let f = Fixture::new();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let path = f.home().join("witness.json");
    let config = fs::read_to_string(&path).unwrap();
    let planted = config.replace("\"local\"", "\"user\"");
    assert_ne!(planted, config);
    fs::write(&path, &planted).unwrap();

    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("claims profile user"), "{stderr}");
    assert!(!stderr.contains("witness set up"), "{stderr}");
    assert_eq!(fs::read_to_string(&path).unwrap(), planted);
}

/// `local` hält die Policy bei jedem `enable` aus HEAD neu fest — eine
/// später committete Verschärfung gilt, statt am ersten Stand zu hängen.
#[test]
fn enable_repins_the_policy_of_a_local_witness() {
    let f = Fixture::new();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let path = f.home().join("witness.json");
    assert!(!fs::read_to_string(&path).unwrap().contains("acme-internal"));

    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_pii":["acme-internal"]}"#,
    )
    .unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "policy"]);
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let config = fs::read_to_string(&path).unwrap();
    assert!(config.contains("acme-internal"), "{config}");
}

/// Auch neben einem laufenden Witness (er hält den Lock seines Homes) hält
/// `enable` die neue Policy fest — und sagt, dass er neu starten muss.
#[test]
fn enable_repins_the_policy_next_to_a_running_witness() {
    let f = Fixture::new();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let home = f.home();
    let mut daemon = Command::new(minds())
        .args(["witness", "run", "--home"])
        .arg(&home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let socket = home.join("run/witness.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "witness did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    fs::create_dir_all(f.root.join(".minds")).unwrap();
    fs::write(
        f.root.join(".minds/redact.json"),
        r#"{"deny_pii":["acme-internal"]}"#,
    )
    .unwrap();
    git(&f.root, &["add", ".minds/redact.json"]);
    git(&f.root, &["commit", "-qm", "policy"]);
    let out = f.enable_default("print");
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("keeps its old policy until restarted"),
        "{stderr}"
    );
    assert!(!stderr.contains("not set up"), "{stderr}");
    let config = fs::read_to_string(home.join("witness.json")).unwrap();
    assert!(config.contains("acme-internal"), "{config}");
}

/// Ein relatives `MINDS_WITNESS_HOME` fände der Hook nie — `enable` warnt
/// statt „running"; mit `MINDS_WITNESS_SOCKET` (der Container-Socket) richtet
/// es gar keinen lokalen Witness ein.
#[test]
fn enable_skips_the_local_witness_when_another_is_named() {
    let f = Fixture::new();
    let out = f.enable_default_with("print", &[("MINDS_WITNESS_HOME", "relative-home")]);
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MINDS_WITNESS_HOME must be an absolute path"),
        "{stderr}"
    );
    assert!(!f.root.join("relative-home").exists());

    let out = f.enable_default_with(
        "print",
        &[("MINDS_WITNESS_SOCKET", "/run/minds/witness.sock")],
    );
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("witness via MINDS_WITNESS_SOCKET"),
        "{stderr}"
    );
    assert!(!f.path("state").join("minds-witness").exists());
}

/// Im Home eines `local`-Witness kann der Agent den Kommentar des
/// öffentlichen Schlüssels umschreiben. Behauptet er den Principal eines
/// getrennten Witness, druckt `enable` diese Zeile nicht — wer sie übernähme,
/// bürgte ahnungslos für eine Trennung.
#[test]
fn enable_refuses_a_separated_principal_in_a_local_home() {
    let f = Fixture::new();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    let public = f.home().join("key/witness_ed25519.pub");
    let line = fs::read_to_string(&public).unwrap();
    let forged = line.replace("minds-witness@", "minds-witness-user@");
    assert_ne!(forged, line);
    fs::write(&public, forged).unwrap();

    let out = f.enable_default_with("print", &[]);
    assert!(out.status.success(), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("claims the principal of a separated witness"),
        "{stderr}"
    );
    assert!(!stderr.contains("witness set up"), "{stderr}");
}

/// Ein schon eingerichteter Witness (hier `container`) bleibt, wie er ist.
#[test]
fn enable_keeps_an_existing_witness_profile() {
    let f = Fixture::new();
    let out = f.enable("container");
    assert!(out.status.success(), "{}", text(&out));
    let config = fs::read_to_string(f.home().join("witness.json")).unwrap();
    let out = f.enable_default("print");
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("a witness with profile container"),
        "{}",
        text(&out)
    );
    assert_eq!(
        fs::read_to_string(f.home().join("witness.json")).unwrap(),
        config
    );
}
