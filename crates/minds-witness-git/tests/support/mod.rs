//! `GitTestFixture` (Spec S2 §12): temporäre Repositories mit echtem `git`,
//! isoliert von globaler und systemweiter Konfiguration, feste Identität und
//! festes Datum. Evidence entsteht als echte Git-Objekte; Signaturen mit
//! `ssh-keygen` und Schlüsseln, die je Lauf erzeugt werden.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use minds_core::ContentHash;
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, Seal, SealOutcome};
use minds_core::observation::{
    Isolation, Observation, ObservationReason, Observations, WitnessFacts,
};
use minds_witness_git::binding::{CommitBinding, DEFAULT_MAX_SEALS};

pub const PROJECT: &str = "git.test/project-42";
pub const PRINCIPAL: &str = "minds-witness-user@test";

/// Prüft die Voraussetzungen; fehlt etwas, scheitert der Test mit klarer
/// Meldung (Spec S2 §12: nichts wird still übersprungen).
pub fn require_tools() {
    let git = Command::new("git").arg("--version").output();
    assert!(
        git.as_ref().is_ok_and(|o| o.status.success()),
        "git (≥ 2.30) wird für die Ebene-2-Tests gebraucht"
    );
    let ssh = Command::new(ssh_keygen()).arg("-?").output();
    assert!(ssh.is_ok(), "ssh-keygen (OpenSSH ≥ 8.2) wird gebraucht");
}

pub fn ssh_keygen() -> PathBuf {
    for c in [
        "/usr/bin/ssh-keygen",
        "/bin/ssh-keygen",
        "/opt/homebrew/bin/ssh-keygen",
    ] {
        if Path::new(c).exists() {
            return PathBuf::from(c);
        }
    }
    PathBuf::from("ssh-keygen")
}

pub fn git_program() -> PathBuf {
    for c in ["/usr/bin/git", "/bin/git", "/opt/homebrew/bin/git"] {
        if Path::new(c).exists() {
            return PathBuf::from(c);
        }
    }
    PathBuf::from("git")
}

/// Ein Repository (bare oder mit Arbeitsverzeichnis) im Temp-Verzeichnis.
#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub git_dir: PathBuf,
    pub home: PathBuf,
}

impl Repo {
    pub fn command(&self) -> Command {
        let mut c = Command::new(git_program());
        let missing = self.home.join("no-such-config");
        c.current_dir(&self.path)
            .env_remove("GIT_DIR")
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", &missing)
            .env("GIT_CONFIG_SYSTEM", &missing)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@git.test")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@git.test")
            .env("GIT_AUTHOR_DATE", "2026-10-26T10:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-10-26T10:00:00Z");
        c
    }

    pub fn try_git(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut c = self.command();
        c.args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        if let Some(input) = stdin {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    pub fn git_with(&self, args: &[&str], stdin: Option<&[u8]>) -> String {
        let out = self.try_git(args, stdin);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.git_with(args, None)
    }

    pub fn blob(&self, bytes: &[u8]) -> String {
        self.git_with(&["hash-object", "-w", "--stdin"], Some(bytes))
    }

    /// Ein Baum aus `(modus, name, oid)`; `git mktree` sortiert.
    pub fn tree(&self, entries: &[(&str, &[u8], &str)]) -> String {
        let mut spec = Vec::new();
        for (mode, name, oid) in entries {
            let kind = match *mode {
                "040000" => "tree",
                "160000" => "commit",
                _ => "blob",
            };
            spec.extend_from_slice(format!("{mode} {kind} {oid}\t").as_bytes());
            spec.extend_from_slice(name);
            spec.push(0);
        }
        self.git_with(&["mktree", "-z", "--missing"], Some(&spec))
    }

    /// Ein Baum aus rohen Bytes, ohne jede Prüfung (für fehlerhafte Bäume).
    pub fn raw_tree(&self, entries: &[(&str, &[u8], &str)]) -> String {
        let mut data = Vec::new();
        for (mode, name, oid) in entries {
            data.extend_from_slice(mode.as_bytes());
            data.push(b' ');
            data.extend_from_slice(name);
            data.push(0);
            data.extend_from_slice(&hex_decode(oid));
        }
        self.git_with(
            &["hash-object", "-t", "tree", "-w", "--literally", "--stdin"],
            Some(&data),
        )
    }

    /// Ein Baum aus Dateien (voller Pfad, Modus, Inhalt); Unterbäume
    /// werden gebaut.
    pub fn files(&self, files: &[(&str, &str, &[u8])]) -> String {
        self.files_bytes(
            &files
                .iter()
                .map(|(p, m, b)| (p.as_bytes(), *m, *b))
                .collect::<Vec<_>>(),
        )
    }

    pub fn files_bytes(&self, files: &[(&[u8], &str, &[u8])]) -> String {
        use std::collections::BTreeMap;
        let mut here: Vec<(String, Vec<u8>, String)> = Vec::new();
        type Sub = Vec<(Vec<u8>, String, Vec<u8>)>;
        let mut dirs: BTreeMap<Vec<u8>, Sub> = BTreeMap::new();
        for (path, mode, bytes) in files {
            match path.iter().position(|b| *b == b'/') {
                None => {
                    let oid = if *mode == "160000" {
                        String::from_utf8(bytes.to_vec()).unwrap()
                    } else {
                        self.blob(bytes)
                    };
                    here.push((mode.to_string(), path.to_vec(), oid));
                }
                Some(i) => dirs.entry(path[..i].to_vec()).or_default().push((
                    path[i + 1..].to_vec(),
                    mode.to_string(),
                    bytes.to_vec(),
                )),
            }
        }
        for (dir, list) in dirs {
            let sub: Vec<(&[u8], &str, &[u8])> = list
                .iter()
                .map(|(p, m, b)| (p.as_slice(), m.as_str(), b.as_slice()))
                .collect();
            let t = self.files_bytes(&sub);
            here.push(("040000".to_owned(), dir, t));
        }
        let entries: Vec<(&str, &[u8], &str)> = here
            .iter()
            .map(|(m, n, o)| (m.as_str(), n.as_slice(), o.as_str()))
            .collect();
        self.tree(&entries)
    }

    pub fn commit(&self, tree: &str, parents: &[&str]) -> String {
        self.commit_at(tree, parents, "2026-10-26T10:00:00Z", "c")
    }

    pub fn commit_at(&self, tree: &str, parents: &[&str], date: &str, msg: &str) -> String {
        let mut c = self.command();
        c.env("GIT_COMMITTER_DATE", date)
            .env("GIT_AUTHOR_DATE", date);
        c.arg("commit-tree").arg(tree);
        for p in parents {
            c.arg("-p").arg(p);
        }
        c.arg("-m").arg(msg);
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    pub fn commit_files(&self, files: &[(&str, &str, &[u8])], parents: &[&str]) -> String {
        let t = self.files(files);
        self.commit(&t, parents)
    }

    pub fn update_ref(&self, name: &str, target: &str) {
        self.git(&["update-ref", name, target]);
    }

    pub fn symref(&self, name: &str, target: &str) {
        self.git(&["symbolic-ref", name, target]);
    }

    pub fn ref_value(&self, name: &str) -> Option<String> {
        let out = self.try_git(&["rev-parse", "--verify", "-q", name], None);
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }
}

pub fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Der Testaufbau: Temp-Verzeichnis, Witness-Schlüssel, Signer-Datei.
pub struct GitTestFixture {
    pub dir: tempfile::TempDir,
    pub key: PathBuf,
    pub public: String,
}

impl Default for GitTestFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl GitTestFixture {
    pub fn new() -> Self {
        require_tools();
        let dir = tempfile::tempdir().unwrap();
        let (key, public) = keypair(dir.path(), "witness");
        Self { dir, key, public }
    }

    fn home(&self) -> PathBuf {
        let h = self.dir.path().join("home");
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    /// Ein Bare-Repository mit `receive.fsckObjects=true`.
    pub fn server(&self) -> Repo {
        self.server_named("server.git")
    }

    pub fn server_named(&self, name: &str) -> Repo {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let r = Repo {
            path: path.clone(),
            git_dir: path,
            home: self.home(),
        };
        r.git(&["init", "-q", "--bare", "-b", "main"]);
        r.git(&["config", "receive.fsckObjects", "true"]);
        r
    }

    /// Ein Arbeits-Repository mit Remote `origin` auf `server`.
    pub fn client(&self, server: &Repo) -> Repo {
        let path = self.dir.path().join("client");
        std::fs::create_dir_all(&path).unwrap();
        let r = Repo {
            git_dir: path.join(".git"),
            path,
            home: self.home(),
        };
        r.git(&["init", "-q", "-b", "main"]);
        let url = format!("file://{}", server.path.display());
        r.git(&["remote", "add", "origin", &url]);
        r
    }

    pub fn signers(&self) -> String {
        format!("{PRINCIPAL} namespaces=\"minds-witness\" {}\n", self.public)
    }

    pub fn signers_file(&self) -> PathBuf {
        let p = self.dir.path().join("allowed_signers");
        std::fs::write(&p, self.signers()).unwrap();
        p
    }
}

pub fn keypair(dir: &Path, name: &str) -> (PathBuf, String) {
    let path = dir.join(name);
    let status = Command::new(ssh_keygen())
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    let mut w = public.split_whitespace();
    (path, format!("{} {}", w.next().unwrap(), w.next().unwrap()))
}

pub fn sign(payload: &[u8], key: &Path, namespace: &str) -> Vec<u8> {
    minds_attest::ssh_sign_ns(std::str::from_utf8(payload).unwrap(), key, namespace)
        .unwrap()
        .into_bytes()
}

pub enum Obs<'a> {
    Content(&'a [u8]),
    Deleted,
    Reason(ObservationReason),
}

pub fn separated() -> WitnessFacts {
    WitnessFacts {
        profile: "user".to_owned(),
        isolation: Isolation {
            witness_uid: 1001,
            agent_uids: vec![1000],
            home_private: true,
            binary_protected: true,
        },
        spec: None,
    }
}

pub fn observations(entries: &[(&str, Obs<'_>)]) -> Observations {
    let list = entries
        .iter()
        .enumerate()
        .map(|(i, (path, o))| {
            let (content, reason) = match o {
                Obs::Content(b) => (
                    Some(ContentHash::from_bytes(*blake3::hash(b).as_bytes())),
                    None,
                ),
                Obs::Deleted => (None, Some(ObservationReason::Deleted)),
                Obs::Reason(r) => (None, Some(*r)),
            };
            Observation {
                seq: i as u64 + 1,
                at: format!("2026-10-26T10:00:{:02}Z", i + 1),
                path: (*path).to_owned(),
                content,
                reason,
            }
        })
        .collect();
    Observations::new("2026-10-26T10:00:00Z", list).with_witness(separated())
}

pub fn seal_text(scope: &str, outcome: SealOutcome) -> String {
    Seal {
        root: ContentHash::from_bytes([7; 32]),
        agent: "witness".to_owned(),
        scope: scope.to_owned(),
        first_seq: 1,
        last_seq: 3,
        events: 3,
        gaps: 0,
        pre_chain: 0,
        outcome,
        previous: None,
        last_event_at: "2026-10-26T10:00:03Z".to_owned(),
    }
    .to_text()
    .unwrap()
}

/// Ein Ref-Update, das ein Push bringen würde (Objekte liegen schon).
#[derive(Debug, Clone)]
pub struct Pending {
    pub name: String,
    pub target: String,
}

/// Evidence-Bau in einem Repository: Objekte schreiben und die Refs
/// entweder setzen (Vorzustand) oder als Push-Updates zurückgeben.
pub struct Evidence<'a> {
    pub repo: &'a Repo,
    pub key: &'a Path,
    pub public: &'a str,
}

impl Evidence<'_> {
    /// Observation-Objekt; liefert Id und Ref-Update.
    pub fn observations(&self, obs: &Observations) -> (String, Pending) {
        let bytes = obs.canonical_bytes().unwrap();
        let id = format!("b3-{}", hex::encode(blake3::hash(&bytes).as_bytes()));
        let blob = self.repo.blob(&bytes);
        let tree = self.repo.tree(&[("100644", b"observations.json", &blob)]);
        let c = self.repo.commit(&tree, &[]);
        let name = format!("refs/minds/observations/{}", &id[3..]);
        (id, Pending { name, target: c })
    }

    /// Seal als `{seal}` mit Kind-Commit `{seal, seal.sig}`, signiert
    /// unter `namespace` (oder unsigniert); liefert Id, Ref-Update und den
    /// Wurzel-Commit.
    pub fn seal(&self, text: &str, namespace: Option<&str>) -> (String, Pending, String) {
        let id = Seal::id_of_text(text).as_str().to_owned();
        let blob = self.repo.blob(text.as_bytes());
        let root_tree = self.repo.tree(&[("100644", b"seal", &blob)]);
        let root = self.repo.commit(&root_tree, &[]);
        let target = match namespace {
            None => root.clone(),
            Some(ns) => {
                let sig = self.repo.blob(&sign(text.as_bytes(), self.key, ns));
                let t = self
                    .repo
                    .tree(&[("100644", b"seal", &blob), ("100644", b"seal.sig", &sig)]);
                self.repo.commit(&t, &[&root])
            }
        };
        let name = format!("refs/minds/evidence/{}", &id[3..]);
        (id, Pending { name, target }, root)
    }

    /// Observations plus `witness-fs/v1`-Seal; liefert Seal-Id und beide
    /// Ref-Updates.
    pub fn witness_fs(&self, obs: &Observations) -> (String, Vec<Pending>) {
        let (obs_id, p1) = self.observations(obs);
        let text = seal_text(
            SCOPE_WITNESS_FS_V1,
            SealOutcome::ObservationsStored {
                observations: obs_id,
            },
        );
        let (seal, p2, _) = self.seal(&text, Some("minds-witness"));
        (seal, vec![p1, p2])
    }

    /// Bindung für `commit`, signiert mit `key` (oder unsigniert).
    pub fn binding_with(
        &self,
        commit: &str,
        project: &str,
        seals: &[String],
        key: Option<(&Path, &str)>,
    ) -> (CommitBinding, Pending) {
        let mut seals = seals.to_vec();
        seals.sort();
        seals.dedup();
        let public = key.map_or(self.public, |(_, p)| p);
        let b = CommitBinding::new(commit, public, project, seals, DEFAULT_MAX_SEALS).unwrap();
        let bytes = b.to_bytes();
        let blob = self.repo.blob(&bytes);
        let tree = match key {
            None => self.repo.tree(&[("100644", b"binding", &blob)]),
            Some((k, _)) => {
                let sig = self.repo.blob(&sign(&bytes, k, "minds-witness"));
                self.repo.tree(&[
                    ("100644", b"binding", &blob),
                    ("100644", b"binding.sig", &sig),
                ])
            }
        };
        let c = self.repo.commit(&tree, &[]);
        (
            b.clone(),
            Pending {
                name: b.ref_name(),
                target: c,
            },
        )
    }

    pub fn binding(&self, commit: &str, seals: &[String]) -> (CommitBinding, Pending) {
        self.binding_with(commit, PROJECT, seals, Some((self.key, self.public)))
    }
}

// ---------------------------------------------------------------------
// Hook und echter Push (Spec S2 §11, §12, §13 Ebene 2 Push).
// ---------------------------------------------------------------------

pub const HOOK_BIN: &str = env!("CARGO_BIN_EXE_minds-witness-hook");

/// Ein installierter Hook: Konfiguration, Zustandsverzeichnis, Server.
#[derive(Debug, Clone)]
pub struct Hook {
    pub config: PathBuf,
    pub state: PathBuf,
    pub server: Repo,
}

/// Ergebnis eines `git push`.
#[derive(Debug)]
pub struct PushOutcome {
    pub exit: i32,
    /// Vollständiges stderr des Clients.
    pub stderr: String,
    /// Nur die Zeilen von minds (`remote: minds-witness: …`), ohne Präfix,
    /// am Ende ohne Leerraum.
    pub minds: Vec<String>,
    /// Alle anderen nicht leeren `remote:`-Zeilen (erwartet: keine; §11
    /// erlaubt auf dem Push-Kanal nur die Zeilen von minds).
    pub foreign: Vec<String>,
}

/// Eine Policy für `refs/heads/*` mit Modus, erzwungenen Refs und Schwelle.
pub fn policy(
    mode: minds_witness_core::Mode,
    enforce: &[&str],
    threshold: u8,
) -> minds_witness_core::Policy {
    minds_witness_core::Policy {
        mode,
        enforce_refs: enforce.iter().map(|s| (*s).to_owned()).collect(),
        coverage_threshold_percent: threshold,
        ..minds_witness_core::Policy::default()
    }
}

fn canonical(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

impl GitTestFixture {
    /// Seed-Datei, nur für den Eigentümer lesbar.
    pub fn seed_file(&self) -> PathBuf {
        let p = self.dir.path().join("seed.hex");
        if !p.exists() {
            std::fs::write(&p, format!("{}\n", "42".repeat(32))).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        p
    }

    /// Eine gültige Konfiguration für `policy` als JSON-Wert.
    pub fn hook_config(&self, policy: &minds_witness_core::Policy) -> serde_json::Value {
        serde_json::json!({
            "operation": "test",
            "project": PROJECT,
            "signers_file": self.signers_file(),
            "allowed_witness_principals": [PRINCIPAL],
            "ssh_keygen": canonical(ssh_keygen()),
            "git": canonical(git_program()),
            "policy": policy,
            "seed_file": self.seed_file(),
            "key_valid_from": "2026-01-01T00:00:00Z",
            "repository": self.dir.path().join("server.git"),
            "state_dir": self.dir.path().join("state"),
        })
    }

    /// Schreibt eine Konfiguration und liefert ihren Pfad.
    pub fn write_config(&self, name: &str, value: &serde_json::Value) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        p
    }

    /// Installiert `minds-witness-hook` als pre- und post-receive-Hook von
    /// `server` (über `minds-witness-hook install`).
    pub fn install_hook(&self, server: &Repo, value: &serde_json::Value) -> Hook {
        let mut value = value.clone();
        value["repository"] = serde_json::json!(server.git_dir);
        let config = self.write_config("hook.json", &value);
        let out = Command::new(HOOK_BIN)
            .args(["install"])
            .arg(&config)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "install: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Hook {
            config,
            state: PathBuf::from(value["state_dir"].as_str().unwrap()),
            server: server.clone(),
        }
    }
}

impl Hook {
    /// Die Quittungen unter `refs/minds/witness/receipts/<commit>/`, in
    /// Ref-Namensordnung.
    pub fn receipts(&self, commit: &str) -> Vec<Vec<u8>> {
        let prefix = format!("refs/minds/witness/receipts/{commit}/");
        let names = self
            .server
            .git(&["for-each-ref", "--format=%(refname)", &prefix]);
        names
            .lines()
            .filter(|l| !l.is_empty())
            .map(|r| {
                self.server
                    .try_git(&["cat-file", "blob", &format!("{r}:receipt.json")], None)
                    .stdout
            })
            .collect()
    }

    /// Die Schlüsselliste unter `refs/minds/witness/keys`.
    pub fn keys(&self) -> Vec<minds_witness_core::PublicKeyEntry> {
        let out = self.server.try_git(
            &["cat-file", "blob", "refs/minds/witness/keys:keys.json"],
            None,
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// Bytes des Trust-Snapshots mit diesem Hash.
    pub fn trust(&self, hash: &minds_witness_core::HashStr) -> Option<Vec<u8>> {
        let hex = hash.as_str().split_once(':').unwrap().1;
        let out = self.server.try_git(
            &[
                "cat-file",
                "blob",
                &format!("refs/minds/witness/trust/{hex}:trust.json"),
            ],
            None,
        );
        out.status.success().then_some(out.stdout)
    }

    /// Prüft eine Quittung vollständig (`/2` mit Snapshot aus dem Server).
    pub fn verify(&self, bytes: &[u8]) -> minds_witness_core::Receipt {
        let r = minds_witness_core::Receipt::from_bytes(bytes).unwrap();
        let trust = r.trust.clone().expect("Quittung /2");
        let snapshot = self.trust(&trust).expect("Trust-Snapshot vorhanden");
        minds_witness_core::verify_receipt_with_trust(bytes, &self.keys(), &snapshot)
            .unwrap()
            .0
            .receipt
    }

    /// `<state>/log.jsonl` als Text (leer, wenn nicht vorhanden).
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.state.join("log.jsonl")).unwrap_or_default()
    }

    /// Dateien im Unterverzeichnis `sub` des Zustandsverzeichnisses.
    pub fn handoff(&self, sub: &str) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(self.state.join(sub))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

impl Repo {
    /// `git push` mit den Argumenten; liefert Exit, stderr und minds-Zeilen.
    pub fn push(&self, args: &[&str]) -> PushOutcome {
        let mut all = vec!["push", "--porcelain"];
        all.extend_from_slice(args);
        let out = self.try_git(&all, None);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let remote: Vec<String> = stderr
            .lines()
            .filter_map(|l| l.strip_prefix("remote:"))
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        let (minds, foreign) = remote
            .into_iter()
            .partition(|l| l.starts_with("minds-witness:"));
        PushOutcome {
            exit: out.status.code().unwrap_or(-1),
            stderr,
            minds,
            foreign,
        }
    }

    /// Ein Ref-Update als Refspec `<target>:<name>`.
    pub fn spec(p: &Pending) -> String {
        format!("{}:{}", p.target, p.name)
    }
}
