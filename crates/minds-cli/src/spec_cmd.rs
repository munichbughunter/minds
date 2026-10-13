//! `minds spec` — das Soll, signiert vom Menschen.
//!
//! ```text
//! minds spec sign [--key <pfad>] [--signers <datei>]
//! minds spec show [<rev>] [--signers <file>]
//! ```
//!
//! # Signieren
//!
//! `sign` liest `.minds/spec.json` aus **HEAD** — nie aus dem Worktree:
//! Freigegeben wird nur eine Fassung, die der Mensch per `git` prüfen kann
//! (`git show HEAD:.minds/spec.json`), auch wenn der Agent sie committet
//! hat. Vor dem Signieren steht auf stderr, was signiert wird: Id und jede
//! Regel. Signiert wird die kanonische Form (RFC 8785) unter `minds-spec`;
//! ein FIDO-Schlüssel verlangt eine Berührung — ein Agent mit voller Shell
//! kann dann kein Soll freigeben. Das Soll geht nur über
//! [`minds_redact::RedactionPipeline::redact_spec`] in den Store
//! (`refs/minds/specs/<id>`, gesynct wie alles unter `refs/minds/`).
//!
//! Was das Soll für einen Commit bedeutet, rechnet erst `minds verify` zur
//! Lesezeit ([`minds_reader::conformance`]) — hier wird nichts davon
//! gespeichert.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use minds_core::spec::{MAX_SPEC, RuleKind, SPEC_PATH, SignedSpec, Spec};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Breite der Beschriftung.
const LABEL: usize = 8;

/// Höchstgröße einer gepinnten Datei (Schema) — wie der Leser sie liest.
const MAX_PINNED: u64 = 4 * 1024 * 1024;

/// Höchstgröße der Redaction-Policy in HEAD.
const MAX_POLICY: u64 = 256 * 1024;

/// Führt `minds spec` aus.
pub fn run(parsed: &crate::Parsed) -> ExitCode {
    match dispatch(parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds spec: {}", crate::text::sanitize(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

fn dispatch(parsed: &crate::Parsed) -> Fallible<()> {
    let command = parsed.positional(0).ok_or("expected: spec sign or show")?;
    match command {
        "sign" => {
            if parsed.positional(1).is_some() {
                return Err(
                    "`minds spec sign` signs the spec in HEAD and takes no revision".into(),
                );
            }
            sign(parsed.value("--key"), parsed.value("--signers"))
        }
        "show" => {
            if parsed.value("--key").is_some() {
                return Err("--key does not apply to `minds spec show`".into());
            }
            show(parsed.positional(1), parsed.value("--signers"))
        }
        other => Err(format!(
            "unknown subcommand \"{}\" — expected sign or show",
            crate::text::sanitize(other)
        )
        .into()),
    }
}

/// Das Soll in `commit` — gelesen, aber noch nicht geprüft.
fn spec_at(ctx: &Context, commit: minds_git::CommitId) -> Fallible<Spec> {
    let (_, bytes) = ctx
        .repo
        .read_blob_bounded(commit, SPEC_PATH, MAX_SPEC as u64)?
        .ok_or_else(|| format!("{SPEC_PATH} is not in {}", &commit.to_string()[..12]))?;
    Spec::parse(&bytes).map_err(|err| format!("{SPEC_PATH}: {err}").into())
}

fn sign(key: Option<&str>, signers: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    // Ein `ssh-keygen` aus einem absoluten PATH-Eintrag außerhalb des
    // Checkouts: Eins, das der Agent abgelegt hat, sähe die Berührung des
    // Menschen und könnte einen anderen Text signieren lassen.
    let program = crate::verify_cmd::conformance::spec_program(&ctx.root)
        .filter(|program| minds_attest::ssh_keygen_available_at(program))
        .ok_or(
            "ssh-keygen not found under an absolute PATH entry outside the checkout — required to sign a spec",
        )?;
    let head = ctx
        .repo
        .head()?
        .commit()
        .ok_or("HEAD has no commit — commit .minds/spec.json first")?;
    let spec = spec_at(&ctx, head)
        .map_err(|err| format!("{err} — commit .minds/spec.json, then sign the version in HEAD"))?;
    if worktree_differs(&ctx.root, &spec) {
        eprintln!(
            "warning: the working-tree {SPEC_PATH} differs from HEAD — signing the version in HEAD"
        );
    }
    // Was das Soll nur per Pfad nennt (Schemas), gehört in der Fassung aus
    // HEAD dazu: als Hash im signierten Umschlag.
    let mut pins = std::collections::BTreeMap::new();
    let mut pinned_blobs = Vec::new();
    for path in minds_core::spec::pinned_paths(&spec) {
        let (blob, bytes) = ctx
            .repo
            .read_blob_bounded(head, &path, MAX_PINNED)?
            .ok_or_else(|| {
                format!(
                    "{} is not in HEAD — a json-schema rule names it",
                    crate::text::sanitize(&path)
                )
            })?;
        pinned_blobs.push((path.clone(), blob));
        pins.insert(path, minds_core::spec::pin_of(&bytes));
    }
    let signed = SignedSpec::new(spec, pins, &head.to_string())?;
    // Die Policy aus HEAD, nie schwächer als der Default — wie beim Replay:
    // Die Worktree-Fassung kann der Agent gelockert haben.
    let pipeline =
        match ctx
            .repo
            .read_blob_bounded(head, crate::config::REDACT_CONFIG, MAX_POLICY)?
        {
            Some((_, bytes)) => crate::config::parse_redaction(&bytes)?,
            None => minds_redact::RedactionConfig::default(),
        }
        .floored_at_default()
        .pipeline()?;
    let spec = pipeline.redact_spec(signed)?;

    let key = crate::sign_cmd::resolve_key(key, &ctx.root)?;
    if crate::text::sanitize(&key) != key {
        return Err("the signing key path contains control or invisible characters".into());
    }
    // Der öffentliche Schlüssel ist Pflicht: An ihm wird geprüft, dass die
    // Signatur von genau diesem Schlüssel stammt.
    let public = crate::intent_cmd::read_public_key(&key).ok_or(
        "the public key (<key>.pub) is missing or unreadable — required to check the signature",
    )?;
    let public_blob = minds_attest::public_key_blob(&public)
        .ok_or("the public key (<key>.pub) is not a readable ssh public key")?;
    let public = Some(public);

    // Erst zeigen, was freigegeben wird — dann die Berührung.
    println!(
        "{}",
        labeled(
            "spec",
            &format!("{} ({} rules)", spec.id(), spec.spec().rules().len())
        )
    );
    std::io::stdout().flush()?;
    eprintln!(
        "{}",
        labeled(
            "review",
            &format!("{SPEC_PATH} in HEAD {}", &head.to_string()[..12])
        )
    );
    for rule in spec.spec().rules() {
        eprintln!(
            "{}",
            labeled(
                "review",
                &format!(
                    "{} [{}] {}",
                    rule.id,
                    rule.kind.name(),
                    describe(&rule.kind)
                )
            )
        );
    }
    // Die Schemas mit ihrer Blob-Id: `git show <blob>` zeigt genau die
    // Fassung, die signiert wird.
    for (path, blob) in &pinned_blobs {
        eprintln!(
            "{}",
            labeled(
                "review",
                &format!("pin {} blob {blob}", crate::text::sanitize(path))
            )
        );
    }
    let public_type = public.as_deref().and_then(minds_attest::public_key_type);
    let software = public_type.is_some_and(|kind| !minds_attest::is_security_key_type(kind));
    let signature = if software {
        minds_attest::ssh_sign_ns_with(
            &program,
            spec.text(),
            Path::new(&key),
            minds_attest::NS_SPEC,
        )?
    } else {
        if public_type.is_some() {
            eprintln!("Touch your security key to approve this spec.");
        }
        minds_attest::ssh_sign_ns_presence_with(
            &program,
            spec.text(),
            Path::new(&key),
            minds_attest::NS_SPEC,
        )?
    };
    // Gegenprobe vor dem Ablegen: vom genannten Schlüssel (nicht von einem
    // anderen im ssh-agent), über genau diesen Umschlag, unter `minds-spec`.
    if minds_attest::signature_public_key(&signature) != Some(public_blob) {
        return Err("the signature was not made with the given key — nothing stored".into());
    }
    if !minds_attest::ssh_check_novalidate_with(
        &program,
        spec.text(),
        &signature,
        minds_attest::NS_SPEC,
    )? {
        return Err("the signature does not cover this spec — nothing stored".into());
    }
    // Vorhandene Signaturen bleiben nur, wenn ihr Schlüssel ein vertrauter
    // `minds-spec`-Signer ist — untergeschobene belegen keinen Platz. Ohne
    // Signer-Datei mit `minds-spec`-Zeile wird nichts verworfen: Ein
    // fehlendes Urteil ist kein „fremd". Umschläge bleiben immer — sie
    // tragen frühere Commits.
    let trust = crate::verify_cmd::witness_trust::WitnessTrust::for_specs(
        ctx.store.as_ref(),
        signers,
        Some(program.clone()),
    );
    ctx.store.put_spec(&spec, &signature, &|s| {
        !trust.has_spec_signers() || trust.spec_key_trusted(s)
    })?;
    println!(
        "{}",
        labeled(
            "signed",
            &crate::intent_cmd::signer_line(&key, public.as_deref(), &signature)
        )
    );
    println!(
        "{}",
        labeled(
            "trust",
            "verify accepts it from a signer restricted to namespaces=\"minds-spec\""
        )
    );
    Ok(())
}

/// Ob die Worktree-Fassung inhaltlich von `spec` abweicht (nur ein Hinweis).
fn worktree_differs(root: &Path, spec: &Spec) -> bool {
    let Ok(file) = crate::checkpoint::core::open_regular(&root.join(SPEC_PATH)) else {
        return false;
    };
    let mut bytes = Vec::new();
    use std::io::Read;
    if file
        .take(MAX_SPEC as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return true;
    }
    Spec::parse(&bytes).map_or(true, |worktree| worktree.id() != spec.id())
}

fn show(rev: Option<&str>, signers: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    let commit = match rev {
        Some(rev) => ctx
            .resolve_rev(rev)
            .ok_or_else(|| format!("unknown revision \"{}\"", crate::text::sanitize(rev)))?,
        None => ctx.repo.head()?.commit().ok_or("HEAD has no commit")?,
    };
    let spec = spec_at(&ctx, commit)?;
    let trust = crate::verify_cmd::witness_trust::WitnessTrust::for_specs(
        ctx.store.as_ref(),
        signers,
        crate::verify_cmd::conformance::spec_program(&ctx.root),
    );
    // Nur der Umschlag: Ob er hier gilt (Historie, Rückfall, Pins), sagt
    // `minds verify`.
    let state = match trust.spec_signature(&spec) {
        minds_reader::conformance::SpecTrust::Valid { signer, .. } => {
            format!("signed by {} (minds-spec)", crate::text::sanitize(&signer))
        }
        minds_reader::conformance::SpecTrust::Unsigned => {
            "unsigned — no signature in this store (minds spec sign)".to_owned()
        }
        minds_reader::conformance::SpecTrust::Invalid => {
            "no valid signature of a trusted minds-spec signer".to_owned()
        }
        minds_reader::conformance::SpecTrust::NotChecked(reason) => {
            format!("not checked ({})", crate::text::sanitize(&reason))
        }
    };
    println!("{}", labeled("spec", &spec.id().to_string()));
    println!("{}", labeled("state", &state));
    for rule in spec.rules() {
        println!(
            "{}",
            labeled(
                "rule",
                &format!(
                    "{} [{}] {}",
                    rule.id,
                    rule.kind.name(),
                    describe(&rule.kind)
                )
            )
        );
    }
    Ok(())
}

/// Eine Regel in einer Zeile, entschärft.
pub(crate) fn describe(kind: &RuleKind) -> String {
    let text = match kind {
        RuleKind::NoDependency { krate, on } => {
            format!("{krate} must not depend on {}", on.join(", "))
        }
        RuleKind::NoImport { paths, import } => {
            format!("{} must not use {}", paths.join(", "), import.join(", "))
        }
        RuleKind::JsonSchema { schema, files } => {
            format!("{} must match {schema}", files.join(", "))
        }
        RuleKind::Graph { expect, paths } => {
            let scope = if paths.is_empty() {
                "every commit".to_owned()
            } else {
                format!("commits changing {}", paths.join(", "))
            };
            format!("{scope} expect {}", expect.as_str())
        }
        RuleKind::Unsupported { reason, .. } => format!("not understood: {reason}"),
    };
    crate::text::sanitize(&text)
}

fn labeled(label: &str, value: &str) -> String {
    format!("{label:<LABEL$}{value}")
}
