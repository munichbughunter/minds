//! `bind` im Witness (Spec S3-MVP §6.1, ADR-S3-005): bindet einen Commit an
//! eigene, inhaltsgebundene `witness-fs/v1`-Seals.
//!
//! Kein Signierdienst: Die Anfrage trägt nur Commit und **erwartetes**
//! Projekt. Der Witness bindet ausschließlich für das Projekt aus seiner
//! `witness.json`, baut die Bindungsbytes selbst, wählt nur Seals aus
//! seinem eigenen Ledger (mit Signatur im Store) und nur für einen Commit,
//! der im festgehaltenen Git-Verzeichnis liegt. Die Auswahl ist die
//! geteilte des Servers ([`minds_witness_git::selection`]): Die gemeldete
//! Abdeckung ist die, die der Server berechnet — sofern er die Seals
//! anerkennt (Signer-Datei, getrennter Witness, übertragene Evidence).
//!
//! Offene Beobachtungs-Streams versiegelt `bind` **nicht** (Abweichung von
//! Spec S3 §6.1 Schritt 1, Umsetzungsnotiz Commit 5): Eine Epoche außerhalb
//! eines Checkpoints verschöbe die Grenzen, an denen der Leser die
//! Session-Fenster festmacht. Was ein Commit enthält, hat sein
//! post-commit-Checkpoint schon versiegelt.

use super::*;
use minds_witness_git::binding::{self, CommitBinding, DEFAULT_MAX_SEALS};
use minds_witness_git::selection::{self, SealEvidence};

/// Feste Gründe, die ein `bind` als `Nack` nennt; jeder andere Fehler
/// erscheint als [`BIND_FAILED`] (Ursache im eigenen Log).
pub(super) const PROJECT_UNPINNED: &str = "project_unpinned";
pub(super) const PROJECT_MISMATCH: &str = "project_mismatch";
pub(super) const UNKNOWN_COMMIT: &str = "unknown commit";
pub(super) const BINDING_CAP: &str = "binding cap reached";
pub(super) const IN_REPO_REQUIRED: &str = "the in-repo store is required";
/// Unter dem Namen der neuen Bindung liegt schon etwas, das nicht genau
/// diese, von diesem Witness signierte Bindung ist.
pub(super) const REF_OCCUPIED: &str = "binding ref occupied";
pub(super) const BIND_FAILED: &str = "bind failed";
pub(super) const KNOWN: &[&str] = &[
    PROJECT_UNPINNED,
    PROJECT_MISMATCH,
    UNKNOWN_COMMIT,
    BINDING_CAP,
    IN_REPO_REQUIRED,
    REF_OCCUPIED,
    "rate limited",
    "requester gone",
];

/// Höchstzahl der Bindungen je Commit, wie der Server (Spec S2 §9).
pub(super) const MAX_BINDINGS_PER_COMMIT: usize = 4;
/// Größtes Observation-Objekt, das der Server liest (Spec S2 §9).
const MAX_OBSERVATIONS_BYTES: u64 = 8 * 1024 * 1024;
/// So viele jüngste `witness-fs/v1`-Seals des Ledgers kommen als Kandidaten
/// in Frage — eine Grenze für Laufzeit und Lesevolumen eines Laufs.
pub(super) const MAX_CANDIDATES: usize = 1024;

/// Prüft das Projekt gegen den Pin — vor allem anderen, ohne Repo-Zugriff.
pub(super) fn check_project(config: &Config, expected: &str) -> Result<(), &'static str> {
    match config.project() {
        None => Err(PROJECT_UNPINNED),
        Some(pinned) if pinned != expected => Err(PROJECT_MISMATCH),
        Some(_) => Ok(()),
    }
}

/// Ein `bind`-Lauf mit der Konfiguration `config` aus `home`. Liefert die
/// Statuszeile für den `Ack`; ein `Err` mit einem der [`KNOWN`]-Gründe geht
/// so an den Client.
pub(super) fn bind(
    home: &Path,
    config: &Config,
    commit: &str,
    expected: &str,
    dry_run: bool,
) -> Fallible<String> {
    check_project(config, expected)?;
    let project = config.project().ok_or(PROJECT_UNPINNED)?.to_owned();
    // Bindungen liegen im beobachteten Repository: Dort liest sie der
    // Server nach dem Push (Spec S3 §4, InRepo als einziges Backend).
    if !matches!(config.store(), PinnedStore::InRepo { .. }) {
        return Err(IN_REPO_REQUIRED.into());
    }
    if !binding::is_commit_id(commit) {
        return Err(UNKNOWN_COMMIT.into());
    }
    // Dieselben Prüfungen wie für jeden Checkpoint (EA-10): Layout, ohne
    // Includes, ohne Namespace, eigenes `.git`.
    let repo = config.pinned_repo()?;
    let store = config.open_pinned_store(&repo)?;
    // Die Lesesicht des Servers über demselben Git-Verzeichnis: dieselben
    // geänderten Pfade (S2 §7.4), dieselben Objekt- und Baumgrenzen.
    let server = minds_git::quarantine::ServerRepo::open_for_receive(
        &minds_git::quarantine::ReceiveEnv {
            git_dir: config.git_dir(),
            quarantine: None,
            namespace: false,
        },
        minds_git::quarantine::Limits::default(),
    )
    .map_err(|err| format!("repository not readable for binding: {err}"))?;
    match server.kind(commit) {
        Ok(Some(minds_git::quarantine::ObjectKind::Commit)) => {}
        _ => return Err(UNKNOWN_COMMIT.into()),
    }
    let view = minds_witness_git::access::RepoView::new(&server);

    let key_path = home.join("key/witness_ed25519");
    let key_line = public_key_line(&key_path)?;
    let program = intent::witness_keygen().ok_or("no root-owned ssh-keygen found")?;
    let own_binding = |name: &str, target: &str| {
        read_own_binding(
            &view,
            &program,
            OwnBinding {
                name,
                target,
                commit,
                key_line: &key_line,
                project: &project,
            },
        )
    };

    // Was schon für diesen Commit gebunden ist: gezählt wird jeder Ref (wie
    // der Server), als eigene gilt nur eine nachweislich von diesem Witness
    // signierte Bindung unter ihrem eigenen Namen.
    let existing = server
        .prestate()
        .read_prefix(&minds_witness_git::candidates::prefix(commit))
        .map_err(|err| format!("bindings not readable: {err}"))?;
    let own: Vec<CommitBinding> = existing
        .iter()
        .filter_map(|(name, target)| own_binding(name, target))
        .collect();

    let candidates = own_candidates(home, store.as_ref())?;
    let chosen = selection::select_seals(&view, commit, &candidates, DEFAULT_MAX_SEALS)
        .map_err(|err| format!("selection failed: {err}"))?;
    let total = chosen.total;
    if chosen.seals.is_empty() {
        return Ok(format!("no-evidence 0/{total}"));
    }
    // Was die eigenen Bindungen schon belegen (nach derselben Regel). Eine
    // neue entsteht nur, wenn sie mehr belegt — eine andere Seal-Menge mit
    // derselben Abdeckung (dieselben Bytes erneut versiegelt) ist keine.
    if let Some(first) = own.first() {
        let bound: Vec<SealEvidence> = candidates
            .iter()
            .filter(|c| own.iter().any(|b| b.seals().iter().any(|s| s == c.id())))
            .cloned()
            .collect();
        let covered = selection::select_seals(&view, commit, &bound, usize::MAX)
            .map_err(|err| format!("selection failed: {err}"))?
            .covered;
        if chosen.covered.is_subset(&covered) {
            return Ok(format!(
                "unchanged {} {}/{total}",
                first.id(),
                covered.len()
            ));
        }
    }
    let shown = format!("{}/{total}", chosen.covered.len());
    if existing.len() >= MAX_BINDINGS_PER_COMMIT {
        return Err(BINDING_CAP.into());
    }
    if dry_run {
        return Ok(format!("selected {} {shown}", chosen.seals.len()));
    }
    let built = CommitBinding::new(commit, &key_line, &project, chosen.seals, DEFAULT_MAX_SEALS)
        .map_err(|err| format!("binding not buildable: {err}"))?;
    let bytes = built.to_bytes();
    let text = String::from_utf8(bytes).map_err(|_| "binding is not UTF-8")?;
    fingerprint(home)?;
    let signature =
        minds_attest::ssh_sign_ns_file_only(&program, &text, &key_path, minds_attest::NS_WITNESS)
            .map_err(|err| format!("binding not signed: {err}"))?;
    let blob = repo.write_blob(text.as_bytes())?;
    let sig = repo.write_blob(signature.as_bytes())?;
    let tree = repo.write_tree(None, [("binding", blob), ("binding.sig", sig)])?;
    let name = built.ref_name();
    match repo.commit_tree_onto_expected(&name, tree, None, "minds: commit binding") {
        Ok(_) => Ok(format!("bound {} {shown}", built.id())),
        // Der Name ist inhaltsadressiert. Liegt dort schon etwas (ein
        // paralleler Lauf), zählt es nur, wenn es genau diese, eigene
        // Bindung ist — sonst hat es jemand anderes belegt.
        Err(minds_git::GitError::RefRaced { .. }) => {
            let fresh = minds_git::quarantine::ServerRepo::open_for_receive(
                &minds_git::quarantine::ReceiveEnv {
                    git_dir: config.git_dir(),
                    quarantine: None,
                    namespace: false,
                },
                minds_git::quarantine::Limits::default(),
            )
            .map_err(|err| format!("repository not readable for binding: {err}"))?;
            let target = match fresh.prestate().read_ref(&name) {
                Ok(Some(minds_git::quarantine::RefTarget::Direct(id))) => id,
                _ => return Err(REF_OCCUPIED.into()),
            };
            let view = minds_witness_git::access::RepoView::new(&fresh);
            match read_own_binding(
                &view,
                &program,
                OwnBinding {
                    name: &name,
                    target: &target,
                    commit,
                    key_line: &key_line,
                    project: &project,
                },
            ) {
                Some(found) if found == built => Ok(format!("unchanged {} {shown}", built.id())),
                _ => Err(REF_OCCUPIED.into()),
            }
        }
        Err(err) => Err(err.into()),
    }
}

/// Die eigenen Kandidaten: die jüngsten `witness-fs/v1`-Seals aus dem
/// Ledger, die signiert im Store liegen und deren Observations der Server
/// anrechnen würde.
fn own_candidates(
    home: &Path,
    store: &dyn minds_store::ContextStore,
) -> Fallible<Vec<SealEvidence>> {
    let ledger = match fs::read_to_string(home.join("ledger")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut ids: Vec<&str> = Vec::new();
    for line in ledger.lines().rev() {
        let mut parts = line.split_whitespace();
        let (Some(id), Some(scope)) = (parts.next(), parts.next()) else {
            continue;
        };
        if scope == minds_core::evidence::SCOPE_WITNESS_FS_V1 && !ids.contains(&id) {
            ids.push(id);
            if ids.len() == MAX_CANDIDATES {
                break;
            }
        }
    }
    let mut out = Vec::new();
    for id in ids {
        let Ok(hash) = id.parse::<minds_core::ContentHash>() else {
            continue;
        };
        // Unlesbares oder Fehlendes ist kein Kandidat — der Server fände es
        // ebenso wenig.
        let Ok(Some(text)) = store.seal_text(&hash) else {
            continue;
        };
        if !matches!(store.seal_signature(&hash), Ok(Some(_))) {
            continue;
        }
        let Some((_, Some(obs_id))) = selection::seal_observations(id, &text) else {
            continue;
        };
        let Ok(obs_hash) = obs_id.parse::<minds_core::ContentHash>() else {
            continue;
        };
        let Ok(Some(bytes)) = store.observations_bytes(&obs_hash) else {
            continue;
        };
        if let Some(evidence) = SealEvidence::from_parts(id, &text, &bytes, MAX_OBSERVATIONS_BYTES)
        {
            out.push(evidence);
        }
    }
    Ok(out)
}

/// Was eine eigene Bindung ausmacht.
struct OwnBinding<'a> {
    name: &'a str,
    target: &'a str,
    commit: &'a str,
    key_line: &'a str,
    project: &'a str,
}

/// Die Bindung unter `name`, wenn sie genau die Form des Servers hat
/// (`binding` und `binding.sig`, beide `100644`, nichts sonst), unter ihrem
/// eigenen Namen liegt, für diesen Commit, dieses Projekt und den eigenen
/// Schlüssel gilt und ihre Signatur mit dem eigenen Schlüssel unter
/// `minds-witness` trägt. Sonst `None` — eine fremd abgelegte oder
/// unsignierte Kopie ist keine eigene Bindung.
fn read_own_binding(
    view: &minds_witness_git::access::RepoView<'_>,
    program: &Path,
    want: OwnBinding<'_>,
) -> Option<CommitBinding> {
    use minds_witness_git::view::{EvidenceView, MODE_BLOB};
    let tree = view.tree(want.target).ok()?;
    let [bytes_entry, sig_entry] = tree.as_slice() else {
        return None;
    };
    if bytes_entry.name != "binding"
        || sig_entry.name != "binding.sig"
        || bytes_entry.mode != MODE_BLOB
        || sig_entry.mode != MODE_BLOB
    {
        return None;
    }
    let bytes = view.blob(&bytes_entry.oid, 64 * 1024).ok()??;
    let found = CommitBinding::parse(&bytes, DEFAULT_MAX_SEALS).ok()?;
    if found.ref_name() != want.name
        || found.commit() != want.commit
        || found.key() != want.key_line
        || found.project() != want.project
    {
        return None;
    }
    let sig = view
        .blob(
            &sig_entry.oid,
            minds_witness_git::trust::MAX_SIGNATURE_BYTES as u64,
        )
        .ok()??;
    let sig = String::from_utf8(sig).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let ours = minds_attest::public_key_blob(want.key_line)?;
    (minds_attest::signature_public_key(&sig)? == ours
        && minds_attest::ssh_check_novalidate_with(program, text, &sig, minds_attest::NS_WITNESS)
            .unwrap_or(false))
    .then_some(found)
}

/// `<typ> <base64>` des eigenen Schlüssels — das Feld `key` der Bindung.
fn public_key_line(key: &Path) -> Fallible<String> {
    let pub_path = key.with_extension("pub");
    validate_tree(&pub_path)?;
    let text = fs::read_to_string(&pub_path)?;
    let mut parts = text.split_whitespace();
    let line = match (parts.next(), parts.next()) {
        (Some(kind), Some(blob)) => format!("{kind} {blob}"),
        _ => return Err("witness public key unreadable".into()),
    };
    if !binding::is_public_key(&line) {
        return Err("witness public key unreadable".into());
    }
    Ok(line)
}
