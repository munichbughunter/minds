//! Lesesicht eines pre-receive-Hooks auf das Server-Repository (Spec S2
//! §7.1 bis §7.4): Objekte aus Quarantäne und Haupt-Datenbank, ohne
//! Ersatzobjekte, jedes Objekt am Header begrenzt, auf seine Art geprüft
//! und neu gehasht; Bäume streng geprüft; Refs des Vorzustands nur bei
//! Bedarf gelesen und je Aufruf memoisiert; neue Commits über
//! `git rev-list`. Die Fassade gibt keine gix-Typen heraus.
//!
//! Das Modul schreibt nichts und trifft keine Prüfentscheidung; was ein
//! Fehler bedeutet, legt der Aufrufer fest (im Kern `Internal`).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gix::objs::{Find, FindHeader};

/// Grenzen eines Hook-Aufrufs (Spec S2 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Größte zulässige Objektgröße; zugleich `alloc_limit_bytes`.
    pub max_object_bytes: u64,
    /// Höchstzahl der Eltern eines Commits.
    pub max_parents: usize,
    /// Größte Baumtiefe.
    pub max_tree_depth: usize,
    /// Höchstzahl geänderter Pfade je Commit.
    pub max_changed_paths: usize,
    /// Höchstzahl der Refs, die ein Präfix liefert.
    pub max_prefix_refs: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_object_bytes: 16 * 1024 * 1024,
            max_parents: 64,
            max_tree_depth: 256,
            max_changed_paths: 10_000,
            max_prefix_refs: 256,
        }
    }
}

/// Fehler der Lesesicht. Die Texte sind fest und tragen keine Inhalte.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiveError {
    /// Das Repository erfüllt eine Vorbedingung nicht (Spec S2 §7.1).
    #[error("repository precondition: {0}")]
    Precondition(&'static str),
    /// Objekt nicht vorhanden.
    #[error("object not found")]
    NotFound,
    /// Größe, Art oder Hash eines Objekts stimmen nicht.
    #[error("object verification failed")]
    Verify,
    /// Baum verletzt die strenge Baumprüfung (Spec S2 §7.4).
    #[error("malformed tree")]
    MalformedTree,
    /// Commit nicht lesbar.
    #[error("malformed commit")]
    MalformedCommit,
    /// Eine Grenze aus [`Limits`] ist überschritten.
    #[error("limit exceeded: {0}")]
    Limit(&'static str),
    /// Objektdatenbank oder Ref-Speicher nicht lesbar.
    #[error("repository read failed: {0}")]
    Read(&'static str),
    /// `git rev-list` scheiterte, brach ab oder lieferte Unlesbares.
    #[error("git rev-list failed: {0}")]
    RevList(&'static str),
}

type Result<T> = std::result::Result<T, ReceiveError>;

/// Wo der Hook liest: Git-Verzeichnis und Quarantäne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiveEnv {
    /// Das Git-Verzeichnis (`GIT_DIR`, im Hook relativ zum cwd).
    pub git_dir: PathBuf,
    /// Die Quarantäne (`GIT_QUARANTINE_PATH`, ersatzweise
    /// `GIT_OBJECT_DIRECTORY`), falls gesetzt.
    pub quarantine: Option<PathBuf>,
    /// Ob `GIT_NAMESPACE` gesetzt ist.
    pub namespace: bool,
}

impl ReceiveEnv {
    /// Liest die Umgebung des laufenden Prozesses.
    pub fn from_process() -> std::io::Result<Self> {
        let cwd = std::env::current_dir()?;
        let git_dir = std::env::var_os("GIT_DIR").map_or(cwd.clone(), |d| cwd.join(d));
        let quarantine = std::env::var_os("GIT_QUARANTINE_PATH")
            .or_else(|| std::env::var_os("GIT_OBJECT_DIRECTORY"))
            .map(|q| cwd.join(q));
        Ok(Self {
            git_dir,
            quarantine,
            namespace: std::env::var_os("GIT_NAMESPACE").is_some_and(|n| !n.is_empty()),
        })
    }
}

/// Ziel eines Refs im Vorzustand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    /// Direkt auf ein Objekt (Hex-Id).
    Direct(String),
    /// Symbolisch auf einen anderen Ref.
    Symbolic(String),
}

/// Art eines Objekts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// Commit.
    Commit,
    /// Baum.
    Tree,
    /// Blob.
    Blob,
    /// Annotierter Tag.
    Tag,
}

/// Ein Baumeintrag nach der strengen Prüfung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Name (eine Pfadkomponente), rohe Bytes.
    pub name: Vec<u8>,
    /// Modus (`0o100644`, `0o100755`, `0o120000`, `0o160000`, `0o040000`).
    pub mode: u32,
    /// Objekt-Id in Hex.
    pub oid: String,
}

/// Baum und Eltern eines Commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitData {
    /// Der Baum (Hex).
    pub tree: String,
    /// Die Eltern in Commit-Reihenfolge (Hex).
    pub parents: Vec<String>,
}

/// Eine Änderung zwischen zwei Bäumen: voller Pfad (rohe Bytes) und je
/// Seite `(Modus, Oid)`, falls vorhanden. Bäume selbst erscheinen nie; ihre
/// Einträge schon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Voller Pfad, `/`-getrennt.
    pub path: Vec<u8>,
    /// Alter Zustand.
    pub old: Option<(u32, String)>,
    /// Neuer Zustand.
    pub new: Option<(u32, String)>,
}

/// Modus eines Baums.
pub const MODE_TREE: u32 = 0o040000;

/// Die Lesesicht eines Hook-Aufrufs.
pub struct ServerRepo {
    repo: gix::Repository,
    main: gix::odb::Handle,
    quarantine: Option<gix::odb::Handle>,
    quarantine_path: Option<PathBuf>,
    limits: Limits,
    refs: RefCell<HashMap<String, Option<RefTarget>>>,
    prefixes: RefCell<HashMap<String, Vec<(String, String)>>>,
}

impl std::fmt::Debug for ServerRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerRepo")
            .field("git_dir", &self.repo.git_dir())
            .field("quarantine", &self.quarantine_path)
            .finish()
    }
}

fn open_odb(dir: &Path, limits: &Limits) -> Result<gix::odb::Handle> {
    let mut handle = gix::odb::at_opts(
        dir,
        std::iter::empty(),
        gix::odb::store::init::Options {
            alloc_limit_bytes: usize::try_from(limits.max_object_bytes).ok(),
            ..Default::default()
        },
    )
    .map_err(|_| ReceiveError::Read("object database"))?;
    handle.refresh_never();
    Ok(handle)
}

impl ServerRepo {
    /// Öffnet das Repository für einen Hook-Aufruf (Spec S2 §7.1). Lehnt
    /// `GIT_NAMESPACE`, SHA-256, reftable, shallow und `core.ignoreCase`
    /// ab; die Quarantäne muss unter `<common_dir>/objects/` liegen.
    pub fn open_for_receive(env: &ReceiveEnv, limits: Limits) -> Result<Self> {
        if env.namespace {
            return Err(ReceiveError::Precondition("GIT_NAMESPACE"));
        }
        let mut permissions = gix::open::Permissions::all();
        permissions.config.includes = false;
        permissions.config.git_binary = false;
        permissions.env.objects = gix::sec::Permission::Deny;
        let repo = gix::open_opts(
            &env.git_dir,
            gix::open::Options::default()
                .permissions(permissions)
                .open_path_as_is(true)
                .config_overrides([
                    "core.useReplaceRefs=true",
                    "gitoxide.objects.noReplace=true",
                ]),
        )
        .map_err(|_| ReceiveError::Precondition("repository not readable"))?;
        if repo.object_hash() != gix::hash::Kind::Sha1 {
            return Err(ReceiveError::Precondition("object format"));
        }
        let config = repo.config_snapshot();
        if config
            .string("extensions.refStorage")
            .is_some_and(|v| v.as_ref() != "files")
        {
            return Err(ReceiveError::Precondition("ref storage"));
        }
        if repo.is_shallow() {
            return Err(ReceiveError::Precondition("shallow"));
        }
        if config.boolean("core.ignoreCase").unwrap_or(false) {
            return Err(ReceiveError::Precondition("core.ignoreCase"));
        }
        let objects = repo.common_dir().join("objects");
        let main = open_odb(&objects, &limits)?;
        let (quarantine, quarantine_path) = match &env.quarantine {
            None => (None, None),
            Some(q) => {
                let real = std::fs::canonicalize(q)
                    .map_err(|_| ReceiveError::Precondition("quarantine"))?;
                let base = std::fs::canonicalize(&objects)
                    .map_err(|_| ReceiveError::Precondition("objects"))?;
                if !real.starts_with(&base) || real == base {
                    return Err(ReceiveError::Precondition("quarantine outside objects"));
                }
                (Some(open_odb(&real, &limits)?), Some(real))
            }
        };
        Ok(Self {
            repo,
            main,
            quarantine,
            quarantine_path,
            limits,
            refs: RefCell::new(HashMap::new()),
            prefixes: RefCell::new(HashMap::new()),
        })
    }

    /// Das Git-Verzeichnis.
    pub fn git_dir(&self) -> &Path {
        self.repo.git_dir()
    }

    /// Die Grenzen dieses Aufrufs.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn header(&self, id: &gix::oid) -> Result<Option<gix::objs::Header>> {
        if let Some(q) = &self.quarantine
            && let Some(h) = q.try_header(id).map_err(|_| ReceiveError::Read("header"))?
        {
            return Ok(Some(h));
        }
        self.main
            .try_header(id)
            .map_err(|_| ReceiveError::Read("header"))
    }

    fn data(&self, id: &gix::oid, buf: &mut Vec<u8>) -> Result<Option<gix::objs::Kind>> {
        if let Some(q) = &self.quarantine
            && let Some(d) = q
                .try_find(id, buf)
                .map_err(|_| ReceiveError::Read("object"))?
        {
            return Ok(Some(d.kind));
        }
        Ok(self
            .main
            .try_find(id, buf)
            .map_err(|_| ReceiveError::Read("object"))?
            .map(|d| d.kind))
    }

    /// Die Art des Objekts `oid`, `None` wenn es fehlt.
    pub fn kind(&self, oid: &str) -> Result<Option<ObjectKind>> {
        let id = parse_oid(oid)?;
        Ok(self.header(&id)?.map(|h| match h.kind {
            gix::objs::Kind::Commit => ObjectKind::Commit,
            gix::objs::Kind::Tree => ObjectKind::Tree,
            gix::objs::Kind::Blob => ObjectKind::Blob,
            gix::objs::Kind::Tag => ObjectKind::Tag,
        }))
    }

    /// Ein Objekt, am Header auf `limit` begrenzt, auf seine Art geprüft und
    /// neu gehasht (kollisionserkennendes SHA-1).
    fn verified(&self, oid: &str, kind: gix::objs::Kind, limit: u64) -> Result<Vec<u8>> {
        let id = parse_oid(oid)?;
        let header = self.header(&id)?.ok_or(ReceiveError::NotFound)?;
        if header.size > limit {
            return Err(ReceiveError::Limit("object size"));
        }
        if header.kind != kind {
            return Err(ReceiveError::Verify);
        }
        let mut buf = Vec::new();
        let found = self.data(&id, &mut buf)?.ok_or(ReceiveError::NotFound)?;
        if found != kind || buf.len() as u64 > limit {
            return Err(ReceiveError::Verify);
        }
        let actual = gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, &buf)
            .map_err(|_| ReceiveError::Verify)?;
        if actual != id {
            return Err(ReceiveError::Verify);
        }
        Ok(buf)
    }

    /// Baum und Eltern des Commits `id` (Spec S2 §7.2).
    pub fn commit_parents(&self, id: &str) -> Result<CommitData> {
        let data = self.verified(id, gix::objs::Kind::Commit, self.limits.max_object_bytes)?;
        parse_commit(&data, self.limits.max_parents)
    }

    /// Die Einträge des Baums `tree`, streng geprüft (Spec S2 §7.4).
    pub fn tree_entries(&self, tree: &str) -> Result<Vec<Entry>> {
        let data = self.verified(tree, gix::objs::Kind::Tree, self.limits.max_object_bytes)?;
        parse_tree(&data)
    }

    /// Der Eintrag unter `path` (Komponenten) im Baum `tree`.
    pub fn lookup(&self, tree: &str, path: &[&[u8]]) -> Result<Option<Entry>> {
        if path.len() > self.limits.max_tree_depth {
            return Err(ReceiveError::Limit("tree depth"));
        }
        let mut current = tree.to_owned();
        for (i, part) in path.iter().enumerate() {
            let entries = self.tree_entries(&current)?;
            let Some(e) = entries.into_iter().find(|e| e.name == *part) else {
                return Ok(None);
            };
            if i + 1 == path.len() {
                return Ok(Some(e));
            }
            if e.mode != MODE_TREE {
                return Ok(None);
            }
            current = e.oid;
        }
        Ok(None)
    }

    /// Größe des Objekts `oid`, `None` wenn es fehlt.
    pub fn size(&self, oid: &str) -> Result<Option<u64>> {
        let id = parse_oid(oid)?;
        Ok(self.header(&id)?.map(|h| h.size))
    }

    /// Bytes des Blobs `oid`, `None` wenn er größer als `max` ist.
    pub fn blob_bytes(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>> {
        let id = parse_oid(oid)?;
        let header = self.header(&id)?.ok_or(ReceiveError::NotFound)?;
        if header.kind != gix::objs::Kind::Blob {
            return Err(ReceiveError::Verify);
        }
        if header.size > max {
            return Ok(None);
        }
        self.verified(oid, gix::objs::Kind::Blob, max).map(Some)
    }

    /// Unterschiede zwischen zwei Bäumen (`old == None`: leerer Baum).
    /// Gleiche Teilbäume werden per Oid übersprungen; ohne
    /// Rename-Erkennung. Höchstens `max_changed_paths` Änderungen.
    pub fn tree_diff(&self, old: Option<&str>, new: Option<&str>) -> Result<Vec<Change>> {
        let mut out = Vec::new();
        self.diff_into(old, new, &mut Vec::new(), 0, &mut out)?;
        Ok(out)
    }

    fn diff_into(
        &self,
        old: Option<&str>,
        new: Option<&str>,
        prefix: &mut Vec<u8>,
        depth: usize,
        out: &mut Vec<Change>,
    ) -> Result<()> {
        if old == new {
            return Ok(());
        }
        if depth > self.limits.max_tree_depth {
            return Err(ReceiveError::Limit("tree depth"));
        }
        let a = match old {
            Some(t) => self.tree_entries(t)?,
            None => Vec::new(),
        };
        let b = match new {
            Some(t) => self.tree_entries(t)?,
            None => Vec::new(),
        };
        let mut names: BTreeSet<&[u8]> = BTreeSet::new();
        names.extend(a.iter().map(|e| e.name.as_slice()));
        names.extend(b.iter().map(|e| e.name.as_slice()));
        for name in names {
            let x = a.iter().find(|e| e.name == name);
            let y = b.iter().find(|e| e.name == name);
            let len = prefix.len();
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(name);
            let tree_of =
                |e: Option<&Entry>| e.filter(|e| e.mode == MODE_TREE).map(|e| e.oid.clone());
            let file_of = |e: Option<&Entry>| {
                e.filter(|e| e.mode != MODE_TREE)
                    .map(|e| (e.mode, e.oid.clone()))
            };
            let (tx, ty) = (tree_of(x), tree_of(y));
            if tx.is_some() || ty.is_some() {
                self.diff_into(tx.as_deref(), ty.as_deref(), prefix, depth + 1, out)?;
            }
            let (fx, fy) = (file_of(x), file_of(y));
            if fx != fy {
                if out.len() >= self.limits.max_changed_paths {
                    return Err(ReceiveError::Limit("changed paths"));
                }
                out.push(Change {
                    path: prefix.clone(),
                    old: fx,
                    new: fy,
                });
            }
            prefix.truncate(len);
        }
        Ok(())
    }

    /// Die memoisierte Sicht auf die Refs des Vorzustands (Spec S2 §7.1).
    pub fn prestate(&self) -> Prestate<'_> {
        Prestate(self)
    }

    /// Der Ref `name` im Vorzustand, memoisiert: Die erste Lesung gilt.
    pub fn read_ref(&self, name: &str) -> Result<Option<RefTarget>> {
        if let Some(memo) = self.refs.borrow().get(name) {
            return Ok(memo.clone());
        }
        let found = self
            .repo
            .try_find_reference(name)
            .map_err(|_| ReceiveError::Read("reference"))?
            .map(|r| match r.target() {
                gix::refs::TargetRef::Object(id) => RefTarget::Direct(id.to_string()),
                gix::refs::TargetRef::Symbolic(n) => RefTarget::Symbolic(n.as_bstr().to_string()),
            });
        self.refs
            .borrow_mut()
            .insert(name.to_owned(), found.clone());
        Ok(found)
    }

    /// Die direkten Refs unter `prefix` im Vorzustand, aufsteigend nach
    /// Name, höchstens `max_prefix_refs`; memoisiert.
    pub fn read_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        if let Some(memo) = self.prefixes.borrow().get(prefix) {
            return Ok(memo.clone());
        }
        let platform = self
            .repo
            .references()
            .map_err(|_| ReceiveError::Read("references"))?;
        let iter = platform
            .prefixed(prefix)
            .map_err(|_| ReceiveError::Read("references"))?;
        let mut out = Vec::new();
        for r in iter {
            let r = r.map_err(|_| ReceiveError::Read("references"))?;
            let name = r.name().as_bstr().to_string();
            if !name.starts_with(prefix) {
                continue;
            }
            if let gix::refs::TargetRef::Object(id) = r.target() {
                out.push((name, id.to_string()));
            }
        }
        out.sort();
        out.truncate(self.limits.max_prefix_refs);
        self.prefixes
            .borrow_mut()
            .insert(prefix.to_owned(), out.clone());
        Ok(out)
    }

    /// `git rev-list --parents --max-count=<max> <new> [--not <nots>]`
    /// (Spec S2 §7.3) mit genau dem Programm `git`, geleerter Umgebung bis
    /// auf Git-Verzeichnis und Quarantäne, ohne Ersatzobjekte und ohne
    /// Netz. Liefert je Zeile Commit und Eltern.
    pub fn rev_list(
        &self,
        git: &Path,
        new: &str,
        nots: &[String],
        max: usize,
        timeout: Duration,
    ) -> Result<Vec<(String, Vec<String>)>> {
        parse_oid(new)?;
        for n in nots {
            parse_oid(n)?;
        }
        let mut cmd = Command::new(git);
        cmd.env_clear();
        for key in [
            "GIT_QUARANTINE_PATH",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        if let Some(q) = &self.quarantine_path
            && std::env::var_os("GIT_OBJECT_DIRECTORY").is_none()
        {
            cmd.env("GIT_OBJECT_DIRECTORY", q);
            cmd.env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.repo.common_dir().join("objects"),
            );
        }
        cmd.env("GIT_DIR", self.repo.git_dir())
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "--no-replace-objects",
                "-c",
                "core.useReplaceRefs=false",
                "-c",
                "protocol.allow=never",
                "rev-list",
                "--parents",
            ])
            .arg(format!("--max-count={max}"))
            .arg(new);
        if !nots.is_empty() {
            cmd.arg("--not").args(nots);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|_| ReceiveError::RevList("spawn"))?;
        let mut stdout = child.stdout.take().ok_or(ReceiveError::RevList("stdout"))?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            out
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ReceiveError::RevList("timeout"));
                }
            }
        };
        let out = reader.join().map_err(|_| ReceiveError::RevList("reader"))?;
        if !status.success() {
            return Err(ReceiveError::RevList("exit"));
        }
        let text = String::from_utf8(out).map_err(|_| ReceiveError::RevList("output"))?;
        let mut rows = Vec::new();
        for line in text.lines() {
            let mut ids = line.split(' ');
            let commit = ids.next().ok_or(ReceiveError::RevList("output"))?;
            parse_oid(commit).map_err(|_| ReceiveError::RevList("output"))?;
            let parents: Vec<String> = ids.map(str::to_owned).collect();
            for p in &parents {
                parse_oid(p).map_err(|_| ReceiveError::RevList("output"))?;
            }
            rows.push((commit.to_owned(), parents));
        }
        if rows.len() > max {
            return Err(ReceiveError::RevList("output"));
        }
        Ok(rows)
    }
}

/// Die Refs des Vorzustands, nur bei Bedarf gelesen und je Aufruf
/// memoisiert; nie die ganze Ref-Datenbank.
#[derive(Debug, Clone, Copy)]
pub struct Prestate<'a>(&'a ServerRepo);

impl Prestate<'_> {
    /// Siehe [`ServerRepo::read_ref`].
    pub fn read_ref(&self, name: &str) -> Result<Option<RefTarget>> {
        self.0.read_ref(name)
    }

    /// Siehe [`ServerRepo::read_prefix`].
    pub fn read_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        self.0.read_prefix(prefix)
    }
}

fn parse_oid(hex: &str) -> Result<gix::ObjectId> {
    if hex.len() != 40 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ReceiveError::Verify);
    }
    gix::ObjectId::from_hex(hex.as_bytes()).map_err(|_| ReceiveError::Verify)
}

/// Baum und Eltern aus den rohen Commit-Bytes: erste Zeile `tree <hex>`,
/// dann beliebig viele `parent <hex>`.
fn parse_commit(data: &[u8], max_parents: usize) -> Result<CommitData> {
    let mut lines = data.split(|b| *b == b'\n');
    let tree = lines
        .next()
        .and_then(|l| l.strip_prefix(b"tree "))
        .and_then(|h| std::str::from_utf8(h).ok())
        .filter(|h| parse_oid(h).is_ok())
        .ok_or(ReceiveError::MalformedCommit)?
        .to_owned();
    let mut parents = Vec::new();
    for line in lines {
        let Some(hex) = line.strip_prefix(b"parent ") else {
            break;
        };
        let hex = std::str::from_utf8(hex).map_err(|_| ReceiveError::MalformedCommit)?;
        parse_oid(hex).map_err(|_| ReceiveError::MalformedCommit)?;
        if parents.len() >= max_parents {
            return Err(ReceiveError::Limit("parents"));
        }
        parents.push(hex.to_owned());
    }
    Ok(CommitData { tree, parents })
}

/// Unicode-Zeichen, die HFS+ beim Vergleich von Namen ignoriert.
fn hfs_ignorable(c: char) -> bool {
    matches!(c as u32, 0x200C..=0x200F | 0x202A..=0x202E | 0x206A..=0x206F | 0xFEFF)
}

/// Ob `name` eine der Formen von `.git` ist, die `git fsck` ablehnt
/// (ASCII-case-insensitiv, NTFS-Kurzname, NTFS-Punkte und -Leerzeichen am
/// Ende, HFS+-ignorierbare Zeichen).
fn is_dotgit(name: &[u8]) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == b".git" || lower == b"git~1" {
        return true;
    }
    let trimmed: &[u8] = {
        let mut end = lower.len();
        while end > 0 && matches!(lower[end - 1], b'.' | b' ') {
            end -= 1;
        }
        &lower[..end]
    };
    if trimmed == b".git" {
        return true;
    }
    if let Ok(s) = std::str::from_utf8(name) {
        let folded: String = s.chars().filter(|c| !hfs_ignorable(*c)).collect();
        if folded.eq_ignore_ascii_case(".git") {
            return true;
        }
    }
    false
}

/// Streng geprüfte Einträge aus rohen Baum-Bytes (Spec S2 §7.4).
fn parse_tree(data: &[u8]) -> Result<Vec<Entry>> {
    let mut out: Vec<Entry> = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or(ReceiveError::MalformedTree)?;
        let mode = match &rest[..space] {
            b"100644" => 0o100644,
            b"100755" => 0o100755,
            b"120000" => 0o120000,
            b"160000" => 0o160000,
            b"40000" => MODE_TREE,
            _ => return Err(ReceiveError::MalformedTree),
        };
        rest = &rest[space + 1..];
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or(ReceiveError::MalformedTree)?;
        let name = rest[..nul].to_vec();
        rest = &rest[nul + 1..];
        if rest.len() < 20 {
            return Err(ReceiveError::MalformedTree);
        }
        let oid = gix::ObjectId::try_from(&rest[..20])
            .map_err(|_| ReceiveError::MalformedTree)?
            .to_string();
        rest = &rest[20..];
        if name.is_empty()
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
            || is_dotgit(&name)
        {
            return Err(ReceiveError::MalformedTree);
        }
        out.push(Entry { name, mode, oid });
    }
    let key = |e: &Entry| {
        let mut k = e.name.clone();
        if e.mode == MODE_TREE {
            k.push(b'/');
        }
        k
    };
    let sorted = out.windows(2).all(|w| key(&w[0]) < key(&w[1]));
    let mut names = std::collections::HashSet::new();
    if !sorted || !out.iter().all(|e| names.insert(e.name.clone())) {
        return Err(ReceiveError::MalformedTree);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name) in entries {
            out.extend_from_slice(mode.as_bytes());
            out.push(b' ');
            out.extend_from_slice(name);
            out.push(0);
            out.extend_from_slice(&[0xab; 20]);
        }
        out
    }

    #[test]
    fn strict_tree_rules() {
        assert_eq!(
            parse_tree(&raw(&[("100644", b"a"), ("40000", b"b")]))
                .unwrap()
                .len(),
            2
        );
        for bad in [
            raw(&[("100644", b"good"), ("100644", b"good")]),
            raw(&[("100644", b"b"), ("100644", b"a")]),
            raw(&[("100644", b"a"), ("40000", b"a")]),
            raw(&[("040000", b"a")]),
            raw(&[("100664", b"a")]),
            raw(&[("100644", b".git")]),
            raw(&[("100644", b".GIT")]),
            raw(&[("100644", b".git. ")]),
            raw(&[("100644", b"git~1")]),
            raw(&[("100644", ".g\u{200c}it".as_bytes())]),
            raw(&[("100644", b"..")]),
            raw(&[("100644", b".")]),
            raw(&[("100644", b"")]),
        ] {
            assert_eq!(parse_tree(&bad), Err(ReceiveError::MalformedTree));
        }
    }

    #[test]
    fn commit_header() {
        let t = "a".repeat(40);
        let p = "b".repeat(40);
        let data = format!("tree {t}\nparent {p}\nparent {p}\nauthor x\n\nmsg");
        let c = parse_commit(data.as_bytes(), 64).unwrap();
        assert_eq!(c.tree, t);
        assert_eq!(c.parents.len(), 2);
        assert_eq!(
            parse_commit(data.as_bytes(), 1),
            Err(ReceiveError::Limit("parents"))
        );
        assert_eq!(
            parse_commit(b"parent x\n", 64),
            Err(ReceiveError::MalformedCommit)
        );
    }
}
