//! Die Conformance in `minds verify`: der Commit gegen das signierte Soll
//! ([`minds_reader::conformance`]).
//!
//! Gerechnet wird immer, wenn es ein Soll gibt — eine Ableitung, die den
//! Exit-Code **nicht** ändert. Erst `--conformance` macht DRIFT und UNKNOWN
//! zum Gate (Exit 2, maskiert nie 1/3/4) — für CI.

use std::path::Path;

use minds_git::CommitId;
use minds_reader::assurance::IntentSignature;
use minds_reader::conformance::{GraphFacts, Inputs, Outcome, Report, evaluate};

use super::witness_trust::WitnessTrust;
use crate::context::Context;

/// Das `ssh-keygen` für Soll-Signaturen (Signieren und Prüfen).
///
/// Unter Unix das erste unter einem absoluten `PATH`-Eintrag außerhalb des
/// Checkouts, dessen Datei **und** jedes Verzeichnis darüber root gehören
/// und für niemanden sonst beschreibbar sind (danach `/usr/bin`): Ein
/// Programm in `~/.local/bin`, `~/.cargo/bin` oder einem nutzereigenen
/// Homebrew-Präfix könnte der Agent abgelegt haben — es sähe die Berührung
/// des Menschen und könnte jede Signatur gutheißen. Unter Windows (keine
/// solche Prüfung) das erste außerhalb des Checkouts.
pub(crate) fn spec_program(root: &Path) -> Option<std::path::PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    #[cfg(unix)]
    {
        let checkout = std::fs::canonicalize(root).ok();
        std::env::split_paths(&path)
            .chain([std::path::PathBuf::from("/usr/bin")])
            .filter(|dir| dir.is_absolute())
            .filter_map(|dir| root_owned_program(&dir.join("ssh-keygen"), "ssh-keygen"))
            .find(|real| {
                !real.starts_with(root) && !checkout.as_deref().is_some_and(|c| real.starts_with(c))
            })
    }
    #[cfg(not(unix))]
    {
        crate::replay_cmd::find_ssh_keygen(&path, root)
    }
}

/// Das Programm hinter `candidate` (Symlinks aufgelöst), wenn es eine
/// Datei namens `name` ist, die samt jedem Verzeichnis darüber nur root
/// ändern kann ([`root_owned_chain`]). Der Name zählt: Ein Symlink des
/// Agenten auf ein beliebiges root-eigenes Programm (etwa `/usr/bin/true`,
/// das jede Prüfung „bestünde") ist kein `ssh-keygen`.
#[cfg(unix)]
pub(crate) fn root_owned_program(candidate: &Path, name: &str) -> Option<std::path::PathBuf> {
    let real = std::fs::canonicalize(candidate).ok()?;
    (real.file_name().is_some_and(|file| file == name) && real.is_file() && root_owned_chain(&real))
        .then_some(real)
}

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
pub(crate) fn root_owned_chain(path: &Path) -> bool {
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
pub(crate) fn carries_acl(path: &Path) -> bool {
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
pub(crate) fn carries_acl(_: &Path) -> bool {
    false
}

/// Andere Systeme (etwa NFSv4-ACLs): nicht feststellbar — fail-closed.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(crate) fn carries_acl(_: &Path) -> bool {
    true
}

/// Was der minds-Graph über den Commit weiß: Sessions aus Trailer bzw.
/// Store-Index; je Session, ob ihr gebundener Intent-Anker im Store liegt
/// und gültig unter `minds-intent` signiert ist. Eine Session, die ein
/// Trailer nennt, die aber nicht im Store liegt, oder eine Intent-Signatur,
/// die sich nicht prüfen lässt, macht die Graph-Regeln UNKNOWN.
///
/// Die Fakten stehen auf der Stufe der Sessions: Unter derselben
/// Nutzerkennung kann der Agent Sessions selbst schreiben (A1) — was eine
/// Session über ihren Commit sagt, belegt erst ein Witness.
pub(crate) fn graph_facts(ctx: &Context, trust: &WitnessTrust<'_>, commit: CommitId) -> GraphFacts {
    let Ok(ids) = super::sessions_of_commit(ctx, commit) else {
        return GraphFacts {
            unknown: true,
            ..GraphFacts::default()
        };
    };
    let mut facts = GraphFacts {
        sessions: ids.len(),
        ..GraphFacts::default()
    };
    for (id, _) in ids {
        let session = match ctx.store.get(id) {
            Ok(Some(session)) => session,
            _ => {
                facts.unknown = true;
                continue;
            }
        };
        let Some(bound) = minds_reader::intent::bound_anchor(&session) else {
            facts.without_intent += 1;
            continue;
        };
        let signed = match (
            ctx.store.get_intent(&bound.id),
            ctx.store.intent_signature(&bound.id),
        ) {
            (Ok(Some(stored)), Ok(Some(signature))) => {
                trust.intent_signature(&stored.text, &signature)
            }
            _ => IntentSignature::Unsigned,
        };
        match signed {
            IntentSignature::Valid(_) => {}
            IntentSignature::NotChecked => facts.unknown = true,
            IntentSignature::Unsigned | IntentSignature::Invalid => facts.without_intent += 1,
        }
    }
    facts
}

/// Der Bericht für `commit` — `None`, wenn kein Soll gilt. `trust` liest
/// und prüft die Umschläge ([`WitnessTrust::envelope_candidates`]; nur mit
/// [`WitnessTrust::with_spec_program`]), `signers` dieselben Signer für die
/// Intent-Signaturen der Graph-Regeln.
pub(crate) fn report(
    ctx: &Context,
    trust: &WitnessTrust<'_>,
    commit: CommitId,
    signers: Option<&str>,
) -> Option<Report> {
    let mut envelopes = || trust.envelope_candidates();
    let mut verify = |signed: &minds_core::spec::SignedSpec| trust.verify_envelope(signed);
    // Intent-Signaturen der Graph-Regeln: dieselben Signer (`--signers`),
    // dasselbe festgehaltene `ssh-keygen` — nie über `PATH`.
    let intents = WitnessTrust::for_intents_with_signers(
        ctx.store.as_ref(),
        signers,
        spec_program(&ctx.root),
    );
    let mut graph = || graph_facts(ctx, &intents, commit);
    evaluate(
        &ctx.repo,
        commit,
        Inputs {
            envelopes: &mut envelopes,
            verify: &mut verify,
            graph: &mut graph,
        },
    )
}

/// Was der Witness während der Sessions des Commits über das Soll sah:
/// je Soll-Id die Umschläge, die er im Store vorfand — und welche Solls
/// davon heute **keinen** dieser Umschläge mehr im Store haben.
///
/// Gelesen werden nur die Fakten aus Epochen eines **getrennten** Witness
/// (Principal `minds-witness-user@…`), die ein Fenster der Sessions tragen
/// ([`minds_reader::observations::witness_facts`]) — den Schlüssel eines
/// Witness unter der Kennung des Agenten kann der Agent lesen. Sessions, die
/// nur heuristisch mit dem Commit verknüpft sind, zählen nicht.
///
/// Eine **Auskunft**, kein Urteil und kein Gate: Der Witness hält die
/// Umschläge ungeprüft fest (er kennt die vertrauenswürdigen Signer nicht).
/// Ein untergeschobener Umschlag kann so „noch da" heißen, und ein
/// Mensch, der neu signiert, verdrängt rechtmäßig Umschläge ohne
/// vertrauenswürdige Signatur. Welches Soll gilt, entscheiden weiter
/// Historie und Signaturen (ADR-0013, Entscheidung 2).
#[derive(Debug, Default)]
pub(crate) struct Witnessed {
    pub seen: std::collections::BTreeMap<
        minds_core::ContentHash,
        std::collections::BTreeSet<minds_core::ContentHash>,
    >,
    pub lost: Vec<minds_core::ContentHash>,
}

/// [`Witnessed`] für `commit` — leer ohne Sessions oder ohne bezeugte
/// Epochen.
pub(crate) fn witnessed(ctx: &Context, trust: &WitnessTrust<'_>, commit: CommitId) -> Witnessed {
    let mut witnessed = Witnessed::default();
    let Ok(ids) = super::sessions_of_commit(ctx, commit) else {
        return witnessed;
    };
    let trusted = |seal_id: &minds_core::ContentHash, text: &str| {
        trust.signature(seal_id, text).is_separated_witness()
    };
    let store = ctx.store.as_ref();
    for (id, source) in ids {
        if source == minds_core::EvidenceSource::Heuristic {
            continue;
        }
        let Ok(Some(session)) = store.get(id) else {
            continue;
        };
        let windows =
            minds_reader::observations::witness_windows(store, &[(id, &session)], &trusted);
        for (_, facts) in minds_reader::observations::witness_facts(store, &windows) {
            // Dieselbe Definition wie bei den Starts: bekanntes Profil und
            // belegte Trennung — sonst zählt die Epoche nicht.
            let separated = |facts: &minds_core::observation::WitnessFacts| {
                facts.isolation.proven()
                    && minds_core::evidence::WitnessProfile::parse(&facts.profile).is_some()
            };
            if let Some(spec) = facts.filter(separated).and_then(|facts| facts.spec) {
                witnessed
                    .seen
                    .entry(spec.id)
                    .or_default()
                    .extend(spec.envelopes);
            }
        }
    }
    for (spec, envelopes) in &witnessed.seen {
        if envelopes.is_empty() {
            continue;
        }
        let present: std::collections::BTreeSet<minds_core::ContentHash> = store
            .get_spec(spec)
            .ok()
            .flatten()
            .map(|stored| {
                stored
                    .envelopes
                    .iter()
                    .map(|envelope| minds_core::spec::envelope_id(envelope.signed.text()))
                    .collect()
            })
            .unwrap_or_default();
        if envelopes.is_disjoint(&present) {
            witnessed.lost.push(spec.clone());
        }
    }
    witnessed
}

/// Die Zeilen zu [`Witnessed`] — unter dem Bericht, ohne ihn zu ändern.
pub(crate) fn print_witnessed(witnessed: &Witnessed, report: Option<&Report>) {
    let short = |id: &minds_core::ContentHash| format!("b3-{}", &id.hex()[..12]);
    for (spec, envelopes) in &witnessed.seen {
        let judged = match report.and_then(|r| r.active.as_ref()) {
            Some((active, _)) if active != spec => {
                format!(" — this commit is judged against {}", short(active))
            }
            _ => String::new(),
        };
        println!(
            "               witnessed spec {} during the sessions ({} envelope(s) seen by the witness){judged}",
            short(spec),
            envelopes.len()
        );
        if witnessed.lost.contains(spec) {
            println!(
                "               note      none of them is in the store any more (unchecked when seen; a new signature replaces envelopes without a trusted one)"
            );
        }
    }
}

/// Die Zeilen des Berichts: eine Kopfzeile, dann je Regel
/// *result · id · expected → observed*. Alle Texte sind schon entschärft
/// (der Reader liefert nur Einzeiler).
pub(crate) fn print(report: &Report) {
    let (pass, drift, unknown) = report.counts();
    let source = match (&report.active, &report.signer) {
        (Some((id, _)), Some(signer)) => format!(
            "spec b3-{} signed by {}",
            &id.hex()[..12],
            crate::text::sanitize(signer)
        ),
        (Some((id, _)), None) => format!("spec b3-{} (not verified)", &id.hex()[..12]),
        (None, _) => "no signed spec".to_owned(),
    };
    println!(
        "Conformance    {}  {pass} pass · {drift} drift · {unknown} unknown — {source}",
        report.outcome().label()
    );
    for row in report.rows() {
        println!(
            "               {:<8} {} [{}]",
            row.outcome.label(),
            row.id,
            row.kind
        );
        println!("                        expected  {}", row.expected);
        println!("                        observed  {}", row.observed);
    }
}

/// Das Gate von `--conformance`: jede Regel PASS, sonst die Gate-Zeile.
/// Ohne Soll oder ohne Commit ist es nicht bestanden (fail-closed).
pub(crate) fn gate_failure(report: Option<&Report>, commit: Option<CommitId>) -> Option<String> {
    let Some(report) = report else {
        let why = if commit.is_some() {
            "no spec — .minds/spec.json, signed with minds spec sign"
        } else {
            "no commit to check"
        };
        return Some(format!(
            "Gate           conformance not assessed ({why}) — required all PASS"
        ));
    };
    let (_, drift, unknown) = report.counts();
    (report.outcome() != Outcome::Pass).then(|| {
        format!("Gate           conformance: {drift} drift, {unknown} unknown — required all PASS")
    })
}

#[cfg(all(test, target_os = "macos"))]
mod acl_tests {
    use super::*;

    /// Eine ACL macht einen Ort untauglich, auch wenn die Modus-Bits passen.
    #[test]
    fn an_acl_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, b"").unwrap();
        assert!(!carries_acl(&file));
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone deny delete"])
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(carries_acl(&file));
        assert!(!carries_acl(Path::new("/usr/bin")));
    }
}

#[cfg(all(test, unix))]
mod chain_tests {
    use super::*;

    /// Weder ein beschreibbares Verzeichnis mit Sticky-Bit (wie `/tmp`, wie
    /// `/nix/store`) noch ein Eintrag des Nutzers darin zählt; `/usr/bin`
    /// schon.
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

    /// Ein Symlink des Nutzers auf ein root-eigenes Programm mit anderem
    /// Namen ist kein `ssh-keygen` — auch wenn es jede Prüfung bestünde.
    #[test]
    fn a_program_counts_only_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("ssh-keygen");
        // Ein root-eigenes `true` — wo es keins gibt (NixOS), nichts zu
        // prüfen.
        let Some(truthy) = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .find(|path| std::fs::canonicalize(path).is_ok_and(|real| root_owned_chain(&real)))
        else {
            return;
        };
        std::os::unix::fs::symlink(truthy, &link).unwrap();
        assert_eq!(root_owned_program(&link, "ssh-keygen"), None);
        let real = std::fs::canonicalize(truthy).unwrap();
        let name = real.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(root_owned_program(&link, &name), Some(real));
    }
}
