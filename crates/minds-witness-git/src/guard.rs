//! Push-Vorprüfung (Spec S2 §7.0): schützt vor dem Kern die Ref-Formen, auf
//! die sich die Prüfung verlässt. Verletzungen der Regeln 1 bis 7 und
//! technische Fehler lehnen den Push in **beiden** Modi ab (eigener
//! Namensraum, wie der Namensraum-Schutz im Kern). Regel 8 (Ziel eines
//! bewerteten Refs ist kein Commit) prüft `GitObjectAccess::new_commits`
//! und folgt damit dem Modus. Meldungen nennen nur Ref-Namen und feste
//! Gründe.

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal};
use minds_git::quarantine::{Entry, ReceiveError, RefTarget, ServerRepo};
use minds_witness_core::{CommitId, RefUpdate};

use crate::binding::{CommitBinding, REF_PREFIX};
use crate::budget::Budget;
use crate::inspector::{EVIDENCE_PREFIX, MAX_SEAL_BYTES, OBSERVATIONS_PREFIX};
use crate::limits::AdapterLimits;
use crate::trust::{MAX_SIGNATURE_BYTES, SigVerdict, TrustConfig, Verifier};
use crate::view::MODE_BLOB;

/// Ergebnis einer verletzten Regel oder eines technischen Fehlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardError {
    /// Eine Regel ist verletzt; `message` ist die feste Meldung an den
    /// Client (`minds-witness: <grund> (Ref: <ref>)`).
    Reject {
        /// Die Meldung.
        message: String,
    },
    /// Technischer Fehler der Vorprüfung; Ablehnung mit fester Meldung.
    Internal,
}

impl GuardError {
    /// Die Meldung an den Client.
    pub fn message(&self) -> String {
        match self {
            Self::Reject { message } => message.clone(),
            Self::Internal => "minds-witness: interne Vorprüfung fehlgeschlagen".to_owned(),
        }
    }
}

/// Höchstlänge einer stdin-Zeile von receive-pack.
pub const MAX_STDIN_LINE: usize = 4096;

/// Ref-Name für Meldungen: druckbares ASCII, sonst `<ref>`.
pub fn shown(name: &str) -> String {
    if !name.is_empty() && name.bytes().all(|b| (0x21..0x7f).contains(&b)) {
        name.to_owned()
    } else {
        "<ref>".to_owned()
    }
}

fn reject(reason: &str, name: &str) -> GuardError {
    GuardError::Reject {
        message: format!("minds-witness: {reason} (Ref: {})", shown(name)),
    }
}

fn tech(_: impl std::fmt::Debug) -> GuardError {
    GuardError::Internal
}

/// Regel 1: stdin von receive-pack (`<old> <new> <ref>` je Zeile).
pub fn parse_stdin(input: &str, max_refs: usize) -> Result<Vec<RefUpdate>, GuardError> {
    let mut out = Vec::new();
    for line in input.lines() {
        if line.len() > MAX_STDIN_LINE {
            return Err(reject("Zeile von receive-pack zu lang", ""));
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(reject("Zeile von receive-pack unlesbar", ""));
        };
        let (Ok(old), Ok(new)) = (CommitId::parse(old), CommitId::parse(new)) else {
            return Err(reject("Zeile von receive-pack unlesbar", name));
        };
        out.push(RefUpdate {
            old,
            new,
            ref_name: name.to_owned(),
        });
        if out.len() > max_refs {
            return Err(reject("zu viele Ref-Updates in einem Push", name));
        }
    }
    Ok(out)
}

fn lower_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Geschützte Präfixe, die auch in anderer Schreibweise nicht vorkommen
/// dürfen (Regel 4).
const CASE_PROTECTED: [&str; 5] = [
    "refs/minds",
    "refs/minds/witness",
    "refs/minds/bindings",
    "refs/minds/evidence",
    "refs/minds/observations",
];

/// Welche Art von Evidence-Ref `name` ist (nach Regel 4 geprüft).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind<'a> {
    Binding { commit: &'a str, id: &'a str },
    Seal { id: &'a str },
    Observations { id: &'a str },
    Other,
}

fn classify(name: &str) -> Result<Kind<'_>, GuardError> {
    let lower = name.to_ascii_lowercase();
    for p in CASE_PROTECTED {
        let hits = lower == p || lower.starts_with(&format!("{p}/"));
        let exact = name == p || name.starts_with(&format!("{p}/"));
        if hits && !exact {
            return Err(reject(
                "Schreibweise eines geschützten Refs unter refs/minds ist unzulässig",
                name,
            ));
        }
    }
    let form = || reject("unzulässige Form eines Evidence-Refs", name);
    if name == "refs/minds/bindings" || name.starts_with(REF_PREFIX) {
        let rest = name.strip_prefix(REF_PREFIX).ok_or_else(form)?;
        let (commit, id) = rest.split_once('/').ok_or_else(form)?;
        if !lower_hex(commit, &[40, 64]) || !lower_hex(id, &[64]) {
            return Err(form());
        }
        return Ok(Kind::Binding { commit, id });
    }
    if name == "refs/minds/evidence" || name.starts_with(EVIDENCE_PREFIX) {
        let id = name.strip_prefix(EVIDENCE_PREFIX).ok_or_else(form)?;
        if !lower_hex(id, &[64]) {
            return Err(form());
        }
        return Ok(Kind::Seal { id });
    }
    if name == "refs/minds/observations" || name.starts_with(OBSERVATIONS_PREFIX) {
        let id = name.strip_prefix(OBSERVATIONS_PREFIX).ok_or_else(form)?;
        if !lower_hex(id, &[64]) {
            return Err(form());
        }
        return Ok(Kind::Observations { id });
    }
    Ok(Kind::Other)
}

/// Die Vorprüfung eines Pushes.
pub struct Guard<'a> {
    repo: &'a ServerRepo,
    verifier: &'a Verifier<'a>,
    budget: &'a Budget,
    trust: &'a TrustConfig,
    limits: &'a AdapterLimits,
    project: &'a str,
}

impl<'a> Guard<'a> {
    /// Eine Vorprüfung über `repo` für das Projekt `project`.
    pub fn new(
        repo: &'a ServerRepo,
        verifier: &'a Verifier<'a>,
        budget: &'a Budget,
        trust: &'a TrustConfig,
        limits: &'a AdapterLimits,
        project: &'a str,
    ) -> Self {
        Self {
            repo,
            verifier,
            budget,
            trust,
            limits,
            project,
        }
    }

    fn entries(&self, commit: &str) -> Result<Vec<Entry>, GuardError> {
        let c = self.repo.commit_parents(commit).map_err(tech)?;
        self.repo.tree_entries(&c.tree).map_err(tech)
    }

    fn bytes(&self, oid: &str, max: u64) -> Result<Option<Vec<u8>>, GuardError> {
        let Some(bytes) = self.repo.blob_bytes(oid, max).map_err(tech)? else {
            return Ok(None);
        };
        self.budget.take_bytes(bytes.len() as u64).map_err(tech)?;
        Ok(Some(bytes))
    }

    /// Prüft alle Regeln; `Ok(())` lässt den Push zum Kern durch.
    pub fn check(&self, updates: &[RefUpdate]) -> Result<(), GuardError> {
        if updates.len() > self.limits.max_refs_per_push {
            return Err(reject(
                "zu viele Ref-Updates in einem Push",
                &updates[0].ref_name,
            ));
        }
        let mut new_bindings: Vec<(String, String)> = Vec::new();
        for u in updates {
            self.budget.check_deadline().map_err(tech)?;
            let name = u.ref_name.as_str();
            // Regel 2: symbolische Refs.
            if let Some(RefTarget::Symbolic(_)) =
                self.repo.prestate().read_ref(name).map_err(tech)?
            {
                return Err(reject(
                    "Update eines symbolischen Refs ist unzulässig",
                    name,
                ));
            }
            // Regel 3: Replace-Refs.
            if name.starts_with("refs/replace/") {
                return Err(reject("refs/replace/ ist unzulässig", name));
            }
            // Regel 4: Formen.
            let kind = classify(name)?;
            if kind == Kind::Other {
                continue;
            }
            // Regel 7: Löschen ist nie zulässig.
            if u.new.is_null() {
                return Err(reject("Evidence-Refs sind nicht löschbar", name));
            }
            match kind {
                Kind::Binding { commit, id } => {
                    if !u.old.is_null() {
                        return Err(reject("Bindungen sind unveränderlich", name));
                    }
                    self.check_binding(name, commit, id, u.new.as_str())?;
                    new_bindings.push((commit.to_owned(), name.to_owned()));
                }
                Kind::Seal { id } => self.check_seal(name, id, u)?,
                Kind::Observations { id } => {
                    if !u.old.is_null() {
                        return Err(reject("Observations sind unveränderlich", name));
                    }
                    self.check_observations(name, id, u.new.as_str())?;
                }
                Kind::Other => {}
            }
        }
        self.check_binding_caps(&new_bindings, updates)
    }

    /// Regel 5: eine neue Bindung, vollständig geprüft.
    fn check_binding(
        &self,
        name: &str,
        commit: &str,
        id: &str,
        target: &str,
    ) -> Result<(), GuardError> {
        let bad = |why: &str| reject(why, name);
        let entries = self.entries(target)?;
        let names: Vec<&[u8]> = entries.iter().map(|e| e.name.as_slice()).collect();
        if names != [b"binding".as_slice(), b"binding.sig".as_slice()]
            || entries.iter().any(|e| e.mode != MODE_BLOB)
        {
            return Err(bad("Bindung muss genau binding und binding.sig enthalten"));
        }
        let bytes = self
            .bytes(&entries[0].oid, self.limits.max_binding_bytes)?
            .ok_or_else(|| bad("Bindung zu groß"))?;
        let sig = self
            .bytes(&entries[1].oid, MAX_SIGNATURE_BYTES as u64)?
            .ok_or_else(|| bad("Signatur zu groß"))?;
        let binding = CommitBinding::parse(&bytes, self.limits.max_seals_per_binding)
            .map_err(|_| bad("Bindung unlesbar"))?;
        if &binding.id()[3..] != id
            || binding.commit() != commit
            || binding.project() != self.project
        {
            return Err(bad("Bindung passt nicht zu Ref-Name oder Projekt"));
        }
        match self
            .verifier
            .verify(&bytes, &sig, Some(binding.key()))
            .map_err(tech)?
        {
            SigVerdict::Accepted { .. } => Ok(()),
            _ => Err(bad("Bindung nicht von einem anerkannten Witness signiert")),
        }
    }

    /// Regel 5, Deckel: je Commit höchstens `max_bindings_per_commit`
    /// Bindungen; aus dem Vorzustand zählen nur solche anerkannter
    /// Schlüssel.
    fn check_binding_caps(
        &self,
        new: &[(String, String)],
        updates: &[RefUpdate],
    ) -> Result<(), GuardError> {
        let mut commits: Vec<&str> = new.iter().map(|(c, _)| c.as_str()).collect();
        commits.sort_unstable();
        commits.dedup();
        for commit in commits {
            let prefix = format!("{REF_PREFIX}{commit}/");
            let mut count = new.iter().filter(|(c, _)| c == commit).count();
            for (name, target) in self.repo.prestate().read_prefix(&prefix).map_err(tech)? {
                if updates.iter().any(|u| u.ref_name == name) {
                    continue;
                }
                if self.recognized(&target)? {
                    count += 1;
                }
            }
            if count > self.limits.max_bindings_per_commit {
                let name = &new
                    .iter()
                    .find(|(c, _)| c == commit)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_default();
                return Err(reject("zu viele Bindungen für diesen Commit", name));
            }
        }
        Ok(())
    }

    fn recognized(&self, target: &str) -> Result<bool, GuardError> {
        let Ok(entries) = self.entries(target) else {
            return Ok(false);
        };
        let Some(e) = entries
            .iter()
            .find(|e| e.name == b"binding" && e.mode == MODE_BLOB)
        else {
            return Ok(false);
        };
        let Some(bytes) = self.bytes(&e.oid, self.limits.max_binding_bytes)? else {
            return Ok(false);
        };
        Ok(
            CommitBinding::parse(&bytes, self.limits.max_seals_per_binding)
                .is_ok_and(|b| self.trust.recognizes(b.key())),
        )
    }

    /// Prüft Text und Signatur eines Seal-Baums; liefert den Seal und die
    /// Oid seines Textes.
    fn read_seal(
        &self,
        name: &str,
        hex_id: &str,
        entries: &[Entry],
    ) -> Result<(Seal, Vec<u8>), GuardError> {
        let bad = |why: &str| reject(why, name);
        let text_entry = entries
            .iter()
            .find(|e| e.name == b"seal")
            .ok_or_else(|| bad("Seal unlesbar"))?;
        let bytes = self
            .bytes(&text_entry.oid, MAX_SEAL_BYTES)?
            .ok_or_else(|| bad("Seal zu groß"))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| bad("Seal unlesbar"))?;
        if Seal::id_of_text(text).hex() != hex_id {
            return Err(bad("Seal passt nicht zu seiner Id"));
        }
        let seal = Seal::parse(text).map_err(|_| bad("Seal unlesbar"))?;
        if seal.to_text().ok().as_deref() != Some(text) {
            return Err(bad("Seal nicht kanonisch"));
        }
        Ok((seal, bytes))
    }

    /// Regel 6 (Signatur je Scope) für einen Seal mit `seal.sig`.
    fn check_seal_signature(
        &self,
        name: &str,
        seal: &Seal,
        bytes: &[u8],
        sig_entry: &Entry,
    ) -> Result<(), GuardError> {
        let bad = |why: &str| reject(why, name);
        let sig = self
            .bytes(&sig_entry.oid, MAX_SIGNATURE_BYTES as u64)?
            .ok_or_else(|| bad("Seal-Signatur zu groß"))?;
        let armored = std::str::from_utf8(&sig).map_err(|_| bad("Seal-Signatur unlesbar"))?;
        if minds_attest::signature_public_key(armored).is_none() {
            return Err(bad("Seal-Signatur unlesbar"));
        }
        if seal.scope == SCOPE_WITNESS_V1 || seal.scope == SCOPE_WITNESS_FS_V1 {
            match self.verifier.verify(bytes, &sig, None).map_err(tech)? {
                SigVerdict::Accepted { .. } => {}
                _ => {
                    return Err(bad(
                        "Witness-Seal nicht von einem anerkannten Witness signiert",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Regeln 6 und 7 für Seal-Refs.
    fn check_seal(&self, name: &str, hex_id: &str, u: &RefUpdate) -> Result<(), GuardError> {
        let bad = |why: &str| reject(why, name);
        let new = self.entries(u.new.as_str())?;
        let names: Vec<&[u8]> = new.iter().map(|e| e.name.as_slice()).collect();
        let signed = names == [b"seal".as_slice(), b"seal.sig".as_slice()];
        let unsigned = names == [b"seal".as_slice()];
        if !(signed || unsigned) || new.iter().any(|e| e.mode != MODE_BLOB) {
            return Err(bad("Seal muss genau seal oder seal und seal.sig enthalten"));
        }
        let (seal, bytes) = self.read_seal(name, hex_id, &new)?;
        let witness_scope = seal.scope == SCOPE_WITNESS_V1 || seal.scope == SCOPE_WITNESS_FS_V1;
        if u.old.is_null() {
            if unsigned && witness_scope {
                return Err(bad("Witness-Seals sind nur signiert anlegbar"));
            }
        } else {
            // Regel 7: nur {seal} → {seal, seal.sig} mit gleichem Seal.
            let old = self.entries(u.old.as_str())?;
            let old_names: Vec<&[u8]> = old.iter().map(|e| e.name.as_slice()).collect();
            if old_names != [b"seal".as_slice()] || !signed || old[0].oid != new[0].oid {
                return Err(bad(
                    "Seals sind nur durch Nachreichen der Signatur änderbar",
                ));
            }
        }
        if signed {
            self.check_seal_signature(name, &seal, &bytes, &new[1])?;
        }
        Ok(())
    }

    /// Regel 6 für Observation-Refs.
    fn check_observations(&self, name: &str, hex_id: &str, target: &str) -> Result<(), GuardError> {
        let bad = |why: &str| reject(why, name);
        let entries = self.entries(target)?;
        let [e] = entries.as_slice() else {
            return Err(bad("Observations müssen genau observations.json enthalten"));
        };
        if e.name != b"observations.json" || e.mode != MODE_BLOB {
            return Err(bad("Observations müssen genau observations.json enthalten"));
        }
        let bytes = self
            .bytes(&e.oid, self.limits.max_observations_bytes)?
            .ok_or_else(|| bad("Observations zu groß"))?;
        if hex::encode(blake3::hash(&bytes).as_bytes()) != hex_id {
            return Err(bad("Observations passen nicht zu ihrer Id"));
        }
        Ok(())
    }
}

impl From<ReceiveError> for GuardError {
    fn from(_: ReceiveError) -> Self {
        GuardError::Internal
    }
}
