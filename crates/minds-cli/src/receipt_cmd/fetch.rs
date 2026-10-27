//! Beschaffung der Evidence für `minds verify --receipt --remote <r>` (Spec
//! S3 §6.3, Ergänzung A). `verify` hängt nie von einem früheren `minds sync`
//! ab: Es holt Quittungen, Schlüsselliste, Snapshots, Bindungen, Seals und
//! Observations selbst — ausschließlich in Tracking-Refs unter
//! `refs/minds/remotes/<r>/`, die `sync` nie pusht.
//!
//! - Namensräume mit Platzhalter (`witness/*`, `bindings/<c>/*`) mit
//!   `--prune`: Was der Server nicht mehr hat, verschwindet auch hier.
//! - Seals und Observations per exaktem Namen: erst `ls-remote`, dann nur die
//!   vorhandenen holen; ein früher geholter Tracking-Ref eines heute
//!   fehlenden Objekts wird gelöscht. So prüft auch ein alter Clone genau,
//!   was der Server bereitstellt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::evidence_cmd::run_git;

/// Frist für alle Git-Aufrufe einer Beschaffung zusammen.
const FETCH_TIME: Duration = Duration::from_secs(120);
/// Höchstzahl der exakt geholten Namen je Stufe.
pub(super) const MAX_NAMES: usize = 2048;

/// Ein benannter Remote und sein Tracking-Präfix.
pub(super) struct Remote<'a> {
    root: &'a Path,
    name: &'a str,
    deadline: Instant,
}

/// Warum die Beschaffung scheiterte. Feste Texte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FetchError {
    /// Kein benannter Remote (auch: eine URL) — Exit 4.
    NotNamed,
    /// Git scheiterte oder die Frist lief ab — Exit 4.
    Failed(&'static str),
}

impl<'a> Remote<'a> {
    /// Prüft, dass `name` ein konfigurierter Remote ist, dessen Name sich
    /// als Teil eines Ref-Namens eignet.
    pub(super) fn named(root: &'a Path, name: &'a str) -> Result<Self, FetchError> {
        let deadline = Instant::now() + FETCH_TIME;
        if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_control) {
            return Err(FetchError::NotNamed);
        }
        let listed = match run_git(root, &["remote".to_owned()], deadline, false) {
            Ok((true, out)) => out,
            _ => return Err(FetchError::Failed("git remote")),
        };
        if !listed.lines().any(|l| l == name) {
            return Err(FetchError::NotNamed);
        }
        let probe = format!("refs/minds/remotes/{name}/witness/keys");
        match run_git(
            root,
            &["check-ref-format".to_owned(), probe],
            deadline,
            false,
        ) {
            Ok((true, _)) => {}
            _ => return Err(FetchError::NotNamed),
        }
        Ok(Self {
            root,
            name,
            deadline,
        })
    }

    /// Der Name des Remotes.
    pub(super) fn name(&self) -> &str {
        self.name
    }

    /// `refs/minds/remotes/<r>/`.
    pub(super) fn tracking(&self) -> String {
        format!("refs/minds/remotes/{}/", self.name)
    }

    /// `refs/minds/<rest>` → `refs/minds/remotes/<r>/<rest>`.
    pub(super) fn tracking_of(&self, name: &str) -> Option<String> {
        name.strip_prefix("refs/minds/")
            .map(|rest| format!("{}{rest}", self.tracking()))
    }

    fn git(&self, args: Vec<String>) -> Result<String, FetchError> {
        match run_git(self.root, &args, self.deadline, false) {
            Ok((true, out)) => Ok(out),
            Ok((false, _)) => Err(FetchError::Failed("git fetch")),
            Err(true) => Err(FetchError::Failed("timeout")),
            Err(false) => Err(FetchError::Failed("git not startable")),
        }
    }

    fn fetch(&self, prune: bool, refspecs: Vec<String>) -> Result<(), FetchError> {
        let mut args: Vec<String> = [
            "-c",
            if cfg!(windows) {
                "core.hooksPath=NUL"
            } else {
                "core.hooksPath=/dev/null"
            },
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if prune {
            args.push("--prune".to_owned());
        }
        args.push(self.name.to_owned());
        args.extend(refspecs);
        self.git(args).map(drop)
    }

    /// Schritt 1: Quittungen, Schlüsselliste, Snapshots.
    pub(super) fn witness(&self) -> Result<(), FetchError> {
        self.fetch(
            true,
            vec![format!(
                "+refs/minds/witness/*:{}witness/*",
                self.tracking()
            )],
        )
    }

    /// Schritt 2: die Bindungen von `commit`.
    pub(super) fn bindings(&self, commit: &str) -> Result<(), FetchError> {
        self.fetch(
            true,
            vec![format!(
                "+refs/minds/bindings/{commit}/*:{}bindings/{commit}/*",
                self.tracking()
            )],
        )
    }

    /// Schritt 4: die Branch-Refs, wenn der Commit lokal fehlt.
    pub(super) fn branches(&self) -> Result<(), FetchError> {
        self.fetch(
            false,
            vec![format!("+refs/heads/*:refs/remotes/{}/*", self.name)],
        )
    }

    /// Schritt 3: genau diese Refs (`refs/minds/evidence/<hex>`,
    /// `refs/minds/observations/<hex>`). Liefert die Namen, die der Server
    /// hat; Tracking-Refs der übrigen werden gelöscht.
    pub(super) fn exact(&self, names: &BTreeSet<String>) -> Result<BTreeSet<String>, FetchError> {
        if names.is_empty() {
            return Ok(BTreeSet::new());
        }
        if names.len() > MAX_NAMES {
            return Err(FetchError::Failed("too many evidence refs"));
        }
        let mut args = vec!["ls-remote".to_owned(), self.name.to_owned()];
        args.extend(names.iter().cloned());
        let listed = self.git(args)?;
        let present: BTreeMap<&str, &str> = listed
            .lines()
            .filter_map(|line| {
                let (oid, name) = line.split_once('\t')?;
                names.contains(name).then_some((name, oid))
            })
            .collect();
        let mut specs = Vec::new();
        for name in names {
            let Some(tracked) = self.tracking_of(name) else {
                continue;
            };
            if present.contains_key(name.as_str()) {
                specs.push(format!("+{name}:{tracked}"));
            } else {
                // Nicht mehr am Server: auch hier nicht mehr lesen. Bleibt
                // der Ref (etwa wegen einer Sperrdatei), ist das ein Fehler —
                // nie eine Nachrechnung mit Evidence, die der Server nicht
                // mehr bereitstellt.
                let _ = self.git(vec![
                    "update-ref".to_owned(),
                    "--no-deref".to_owned(),
                    "-d".to_owned(),
                    tracked.clone(),
                ]);
                let still = run_git(
                    self.root,
                    // Exakter Name, keine DWIM-Suche.
                    &[
                        "show-ref".to_owned(),
                        "--verify".to_owned(),
                        "--quiet".to_owned(),
                        tracked,
                    ],
                    self.deadline,
                    false,
                );
                if !matches!(still, Ok((false, _))) {
                    return Err(FetchError::Failed("stale tracking ref not removable"));
                }
            }
        }
        if !specs.is_empty() {
            self.fetch(false, specs)?;
        }
        Ok(present.keys().map(|n| (*n).to_owned()).collect())
    }
}
