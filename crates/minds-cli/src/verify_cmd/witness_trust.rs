//! Wann `minds verify` eine Datei-Beobachtung des Witness (EA-08) gelten
//! lässt.
//!
//! Eine Beobachtung kann eine Zeile **erklären** und damit über
//! `--require-explained` entscheiden. Unter `refs/minds/` kann im geteilten
//! Repository aber auch der Agent schreiben — Objekt, Seal und Signatur. Die
//! Vertrauensfrage ist deshalb strenger als bei der bloßen Anzeige eines
//! Witness-Seals:
//!
//! - **Woher die Signer-Datei kommt:** nur `--signers` oder
//!   `~/.ssh/allowed_signers` — nie `gpg.ssh.allowedSignersFile` aus der
//!   Repo-Konfiguration, die der Agent schreiben kann.
//! - **Wer Witness ist:** ein Principal, dessen **sämtliche** Zeilen auf genau
//!   `namespaces="minds-witness"` beschränkt sind. Ein unbeschränkter
//!   Entwickler-Schlüssel verifiziert unter jedem Namespace — mit ihm könnte
//!   jeder, der ihn nutzen darf (auch ein Agent), einen „Witness"-Seal
//!   signieren.
//! - **Was geprüft wird:** die Signatur über genau den Seal-Text unter
//!   `minds-witness`.
//!
//! Fehlt eines davon, gilt keine Beobachtung — fail-closed.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use minds_core::{ContentHash, Session, SessionId};
use minds_reader::assurance::{IntentSignature, ReplaySummary, SealSignature, SignerKind};
use minds_store::ContextStore;

/// Höchstgröße der Signer-Datei, die gelesen wird.
pub(crate) const MAX_SIGNERS_BYTES: u64 = 1024 * 1024;

/// Das Vertrauens-Prädikat für `witness-fs/v1`-Seals.
pub(super) fn observation_trust<'a>(
    store: &'a dyn ContextStore,
    signers: Option<&str>,
) -> impl Fn(&ContentHash, &str) -> bool + 'a {
    let trust = WitnessTrust::new(store, signers);
    move |seal_id: &ContentHash, text: &str| {
        matches!(
            trust.signature(seal_id, text),
            SealSignature::Witness { .. }
        )
    }
}

/// Die Witness-Prüfung mit denselben Regeln wie [`observation_trust`], aber
/// mit dem ganzen Befund je Seal — für die Assurance (EA-11/EA-12). Jedes
/// Urteil wird einmal gerechnet: Jede Prüfung startet `ssh-keygen`.
pub(crate) struct WitnessTrust<'a> {
    store: &'a dyn ContextStore,
    file: Option<PathBuf>,
    content: Option<String>,
    keygen: bool,
    verdicts: RefCell<HashMap<ContentHash, SealSignature>>,
    /// Intent-Urteile je (Ankertext, Signatur) — eine Prüfung startet
    /// mehrere `ssh-keygen`-Prozesse, und viele Sessions teilen einen Anker.
    intents: RefCell<HashMap<[u8; 32], IntentSignature>>,
    /// Die Replay-Records (EA-18b), einmal je Lauf gelesen
    /// ([`minds_reader::replay::ReplayIndex`]).
    replays: OnceCell<minds_reader::replay::ReplayIndex>,
    /// Signatur-Urteile je (Record-Id, Signatur) — jede Prüfung startet
    /// `ssh-keygen`.
    replay_verdicts: RefCell<HashMap<(ContentHash, String), bool>>,
}

impl<'a> WitnessTrust<'a> {
    pub(crate) fn new(store: &'a dyn ContextStore, signers: Option<&str>) -> Self {
        let file = trusted_signers_file(signers);
        let content = file.as_deref().and_then(|path| {
            let meta = std::fs::metadata(path).ok()?;
            if !meta.is_file() || meta.len() > MAX_SIGNERS_BYTES {
                return None;
            }
            std::fs::read_to_string(path).ok()
        });
        let keygen = content.is_some() && minds_attest::ssh_keygen_available();
        Self {
            store,
            file,
            content,
            keygen,
            verdicts: RefCell::new(HashMap::new()),
            intents: RefCell::new(HashMap::new()),
            replays: OnceCell::new(),
            replay_verdicts: RefCell::new(HashMap::new()),
        }
    }

    /// Ob eine vertrauenswürdige Signer-Datei vorliegt und geprüft werden
    /// kann ([`minds_reader::assurance::AssuranceInput::trusted_signers`]).
    pub(crate) fn trusted(&self) -> bool {
        self.keygen
    }

    /// Der Signaturbefund eines Seals unter `minds-witness`: ohne
    /// vertrauenswürdige Signer `NotChecked`, mit ihnen jede misslungene
    /// Prüfung `NotWitness` (fail-closed).
    pub(crate) fn signature(&self, seal_id: &ContentHash, text: &str) -> SealSignature {
        if let Some(verdict) = self.verdicts.borrow().get(seal_id) {
            return verdict.clone();
        }
        let verdict = self.check(seal_id, text);
        self.verdicts
            .borrow_mut()
            .insert(seal_id.clone(), verdict.clone());
        verdict
    }

    /// Der Store, aus dem die Seals gelesen werden.
    pub(crate) fn store(&self) -> &'a dyn ContextStore {
        self.store
    }

    /// Ob `seal_id` ein gültig signierter Witness-Seal ist — gelesen aus dem
    /// Store, geprüft wie [`signature`](Self::signature). Das Prädikat
    /// `TrustSeal` für [`minds_reader::intent::epoch_chain`].
    pub(crate) fn witnessed(&self, seal_id: &ContentHash) -> bool {
        self.store
            .seal_text(seal_id)
            .ok()
            .flatten()
            .is_some_and(|text| {
                matches!(
                    self.signature(seal_id, &text),
                    SealSignature::Witness { .. }
                )
            })
    }

    /// Die Signaturlage eines Intent-Ankers unter `minds-intent` (EA-15):
    /// ohne vertrauenswürdige Signer `NotChecked`; gültig, wenn ein
    /// Principal, dessen **sämtliche** Zeilen auf genau
    /// `namespaces="minds-intent"` beschränkt sind ([`intent_only`]), die
    /// Signatur über genau den Ankertext unter `minds-intent` trägt. Sonst
    /// `Invalid` — ein Schlüssel nur für `minds` ebenso wie ein
    /// unbeschränkter: ein Assurance-Fakt, keine Manipulation der Evidence.
    ///
    /// `sk key` heißt: Der in der geprüften Signatur stehende Schlüssel ist
    /// ein FIDO-Schlüssel **und** die Signatur trägt das User-Presence-Flag.
    /// Eine Hardware-Attestierung ist das nicht — wer die Signer-Datei
    /// schreiben kann, kann auch einen Software-„sk"-Schlüssel eintragen.
    pub(crate) fn intent_signature(&self, anchor: &str, signature: &str) -> IntentSignature {
        let key = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&(anchor.len() as u64).to_le_bytes());
            hasher.update(anchor.as_bytes());
            hasher.update(signature.as_bytes());
            *hasher.finalize().as_bytes()
        };
        if let Some(verdict) = self.intents.borrow().get(&key) {
            return *verdict;
        }
        let verdict = self.check_intent(anchor, signature);
        self.intents.borrow_mut().insert(key, verdict);
        verdict
    }

    /// Das Replay-Ergebnis der Session (EA-18b) für genau `commit` — die
    /// Regeln (Bindung an den Commit, Vorrang der signierten, der
    /// schwächste gilt, Vergiftung bei ungültiger Signatur oder verändertem
    /// Namensraum) stehen in [`minds_reader::replay::ReplayIndex::summary`]
    /// (W5); hier kommt nur die Signaturprüfung unter `minds-anchor` dazu.
    /// Ohne Commit-Kontext (eine Session-Id als Ziel, `audit`) kein Replay.
    pub(crate) fn replay(
        &self,
        id: SessionId,
        session: Option<&Session>,
        commit: Option<&str>,
    ) -> Option<ReplaySummary> {
        let (session, commit) = (session?, commit?);
        let index = self
            .replays
            .get_or_init(|| minds_reader::replay::ReplayIndex::load(self.store));
        index.summary(
            self.store,
            id,
            session,
            commit,
            &|record, text, signature| self.replay_signed(record, text, signature),
        )
    }

    /// [`anchor_signed`](Self::anchor_signed), einmal je Record und
    /// Signatur.
    fn replay_signed(&self, record: &ContentHash, text: &str, signature: &str) -> bool {
        let key = (record.clone(), signature.to_owned());
        if let Some(verdict) = self.replay_verdicts.borrow().get(&key) {
            return *verdict;
        }
        let verdict = self.anchor_signed(text, signature);
        self.replay_verdicts.borrow_mut().insert(key, verdict);
        verdict
    }

    /// Ob `signature` die gespeicherten Bytes eines Replay-Records unter
    /// `minds-anchor` trägt — von einem Principal, dessen **sämtliche**
    /// Zeilen auf genau diesen Namespace beschränkt sind (dieselbe Regel
    /// wie für Witness und Intent). Ohne vertrauenswürdige Signer: nein.
    fn anchor_signed(&self, text: &str, signature: &str) -> bool {
        let (Some(path), Some(content), true) =
            (self.file.as_deref(), self.content.as_deref(), self.keygen)
        else {
            return false;
        };
        let Ok(principals) = minds_attest::ssh_find_principals(signature, path) else {
            return false;
        };
        principals.iter().any(|principal| {
            anchor_only(content, principal)
                && minds_attest::ssh_verify_ns(
                    text,
                    signature,
                    path,
                    principal,
                    minds_attest::NS_ANCHOR,
                )
                .unwrap_or(false)
        })
    }

    fn check_intent(&self, anchor: &str, signature: &str) -> IntentSignature {
        let (Some(path), Some(content), true) =
            (self.file.as_deref(), self.content.as_deref(), self.keygen)
        else {
            return IntentSignature::NotChecked;
        };
        let Ok(principals) = minds_attest::ssh_find_principals(signature, path) else {
            return IntentSignature::NotChecked;
        };
        let valid = principals.iter().any(|principal| {
            intent_only(content, principal)
                && minds_attest::ssh_verify_ns(
                    anchor,
                    signature,
                    path,
                    principal,
                    minds_attest::NS_INTENT,
                )
                .unwrap_or(false)
        });
        if !valid {
            return IntentSignature::Invalid;
        }
        // `sk key` nur mit gesetztem User-Presence-Flag: `ssh-keygen -Y
        // verify` erzwingt es nicht, und ein `no-touch-required`-Schlüssel
        // signiert ohne Berührung — dann ist er nicht mehr als ein
        // Software-Schlüssel.
        IntentSignature::Valid(signer_kind(minds_attest::signature_key(signature).as_ref()))
    }

    fn check(&self, seal_id: &ContentHash, text: &str) -> SealSignature {
        let signature = match self.store.seal_signature(seal_id) {
            Ok(None) => return SealSignature::Missing,
            Ok(Some(signature)) => Some(signature),
            Err(_) => None,
        };
        let (Some(path), Some(content), true) =
            (self.file.as_deref(), self.content.as_deref(), self.keygen)
        else {
            return SealSignature::NotChecked;
        };
        let Some(signature) = signature else {
            return SealSignature::NotWitness;
        };
        let Ok(principals) = minds_attest::ssh_find_principals(&signature, path) else {
            return SealSignature::NotWitness;
        };
        principals
            .into_iter()
            .find(|principal| {
                witness_only(content, principal)
                    && minds_attest::ssh_verify_ns(
                        text,
                        &signature,
                        path,
                        principal,
                        minds_attest::NS_WITNESS,
                    )
                    .unwrap_or(false)
            })
            .map_or(SealSignature::NotWitness, |principal| {
                SealSignature::Witness { principal }
            })
    }
}

/// Die Schlüsselart einer **gültigen** Intent-Signatur: `sk key` nur für
/// einen FIDO-Schlüssel mit gesetztem User-Presence-Flag — `ssh-keygen -Y
/// verify` erzwingt das Flag nicht, und ein `no-touch-required`-Schlüssel
/// signiert ohne Berührung. Alles andere ist nicht mehr als ein
/// Software-Schlüssel.
fn signer_kind(key: Option<&minds_attest::SignatureKey>) -> SignerKind {
    match key {
        Some(key) if key.user_presence && minds_attest::is_security_key_type(&key.key_type) => {
            SignerKind::SecurityKey
        }
        _ => SignerKind::SoftwareKey,
    }
}

/// `--signers`, sonst `~/.ssh/allowed_signers` — nie aus der Repo-Konfiguration.
fn trusted_signers_file(signers: Option<&str>) -> Option<PathBuf> {
    if let Some(signers) = signers {
        return Some(PathBuf::from(signers));
    }
    let home = std::env::var_os("HOME")?;
    let default = Path::new(&home).join(".ssh/allowed_signers");
    default.is_file().then_some(default)
}

/// Ob `principal` ein Witness ist: Er kommt in `allowed_signers` vor, und
/// **jede** Zeile, die ihn nennt **oder** einen seiner Schlüssel trägt (unter
/// welchem Principal auch immer), ist auf genau `namespaces="minds-witness"`
/// beschränkt. Sonst könnte ein Schlüssel, der anderswo unbeschränkt steht
/// (ein Entwickler-Schlüssel im ssh-agent), Witness-Seals signieren.
///
/// Was sich nicht sicher lesen lässt — Principals in Anführungszeichen,
/// Muster (`*`, `?`, `!`), Zeilen ohne Schlüssel —, macht das Ergebnis
/// `false`. `principal` darf das ganze Principal-Feld sein, wie
/// `ssh-keygen -Y find-principals` es ausgibt.
fn witness_only(allowed_signers: &str, principal: &str) -> bool {
    restricted_to(allowed_signers, principal, minds_attest::NS_WITNESS)
}

/// Ob `principal` ein Intent-Signer ist — dieselbe Regel wie
/// [`witness_only`], für `namespaces="minds-intent"` (EA-15). Ein
/// unbeschränkter Entwickler-Schlüssel (Commit-Signing im ssh-agent, den
/// ein Agent mitbenutzen kann) gibt so keinen Intent frei.
fn intent_only(allowed_signers: &str, principal: &str) -> bool {
    restricted_to(allowed_signers, principal, minds_attest::NS_INTENT)
}

/// Ob `principal` ein CI-Signer für Replay-Records ist — dieselbe Regel
/// wie [`witness_only`], für `namespaces="minds-anchor"` (EA-18b).
fn anchor_only(allowed_signers: &str, principal: &str) -> bool {
    restricted_to(allowed_signers, principal, minds_attest::NS_ANCHOR)
}

/// Ob **jede** Zeile, die `principal` nennt oder einen seiner Schlüssel
/// trägt, auf genau `namespaces="<namespace>"` beschränkt ist.
fn restricted_to(allowed_signers: &str, principal: &str, namespace: &str) -> bool {
    let only = format!("\"{namespace}\"");
    struct Line {
        principals: String,
        restricted: bool,
        key: Option<String>,
    }
    let mut lines = Vec::new();
    for line in allowed_signers.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokens(line);
        let Some(principals) = tokens.first() else {
            continue;
        };

        let (options, rest) = match tokens.get(1) {
            Some(token) if !is_key_type(token) => (Some(token.as_str()), &tokens[2..]),
            _ => (None, &tokens[1.min(tokens.len())..]),
        };
        // Eine CA-Zeile ließe jedes Zertifikat dieser CA als Witness gelten.
        let restricted = options.is_some_and(|options| {
            !options
                .split(',')
                .any(|o| o.eq_ignore_ascii_case("cert-authority"))
                && options
                    .split(',')
                    .filter(|o| o.to_ascii_lowercase().starts_with("namespaces="))
                    .map(|o| &o["namespaces=".len()..])
                    .collect::<Vec<_>>()
                    == [only.as_str()]
        });
        let key = match rest {
            [kind, blob, ..] if is_key_type(kind) => Some(format!("{kind} {blob}")),
            _ => None,
        };
        lines.push(Line {
            principals: principals.clone(),
            restricted,
            key,
        });
    }
    // Eine Zeile „nennt" den Principal, wenn eines ihrer Muster ihn treffen
    // **könnte** (OpenSSH: `*`, `?`, Anführungszeichen; eine Negation zählt
    // vorsichtshalber nicht als Ausschluss) — dann gelten für sie dieselben
    // Regeln. Muster, die ihn nicht treffen können (`*@firma.de` für
    // Commit-Signaturen), sind ohne Belang, außer sie tragen seinen
    // Schlüssel.
    let names = |line: &Line| {
        line.principals == principal
            || line
                .principals
                .trim_matches('"')
                .split(',')
                .map(|p| p.trim_start_matches('!'))
                .any(|pattern| glob(pattern.as_bytes(), principal.as_bytes()))
    };
    let mut keys = Vec::new();
    for line in lines.iter().filter(|line| names(line)) {
        match &line.key {
            Some(key) if line.restricted => keys.push(key.clone()),
            _ => return false,
        }
    }
    !keys.is_empty()
        && lines
            .iter()
            .filter(|line| line.key.as_ref().is_some_and(|key| keys.contains(key)))
            .all(|line| line.restricted)
}

/// `*` und `?` wie in OpenSSH-Principal-Mustern.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&pattern[1..], text) || (!text.is_empty() && glob(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

fn is_key_type(token: &str) -> bool {
    token.starts_with("ssh-") || token.starts_with("ecdsa-") || token.starts_with("sk-")
}

/// Leerzeichen-getrennte Felder; Anführungszeichen schützen Leerzeichen.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample";
    const OTHER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOther";

    #[test]
    fn only_a_touched_security_key_is_an_sk_key() {
        let key = |key_type: &str, user_presence| minds_attest::SignatureKey {
            key_type: key_type.into(),
            user_presence,
        };
        let sk = "sk-ssh-ed25519@openssh.com";
        assert_eq!(signer_kind(Some(&key(sk, true))), SignerKind::SecurityKey);
        assert_eq!(signer_kind(Some(&key(sk, false))), SignerKind::SoftwareKey);
        assert_eq!(
            signer_kind(Some(&key("ssh-ed25519", true))),
            SignerKind::SoftwareKey
        );
        assert_eq!(signer_kind(None), SignerKind::SoftwareKey);
    }

    /// EA-15: Intent-Signer folgen derselben Regel, unter `minds-intent` —
    /// und ein Witness-Principal ist kein Intent-Signer (und umgekehrt).
    #[test]
    fn only_principals_restricted_to_the_intent_namespace_sign_intents() {
        let restricted = format!("h@host namespaces=\"minds-intent\" {KEY}");
        assert!(intent_only(&restricted, "h@host"));
        assert!(!witness_only(&restricted, "h@host"));
        for line in [
            format!("h@host {KEY}"),
            format!("h@host namespaces=\"minds\" {KEY}"),
            format!("h@host namespaces=\"minds,minds-intent\" {KEY}"),
            format!("h@host namespaces=\"minds-witness\" {KEY}"),
            // Derselbe Schlüssel unbeschränkt unter anderem Namen.
            format!("{restricted}\ndev@host {KEY}"),
        ] {
            assert!(!intent_only(&line, "h@host"), "{line}");
        }
    }

    #[test]
    fn only_principals_restricted_to_the_witness_namespace_count() {
        let restricted = format!("w@host namespaces=\"minds-witness\" {KEY}");
        assert!(witness_only(&restricted, "w@host"));
        for line in [
            // Unbeschränkt: verifiziert unter jedem Namespace.
            format!("w@host {KEY}"),
            format!("w@host namespaces=\"minds,minds-witness\" {KEY}"),
            format!("w@host namespaces=\"minds\" {KEY}"),
            format!("w@host valid-after=\"20260101\" {KEY}"),
            // Muster und Anführungszeichen, die ihn treffen, zählen wie ein
            // Name — unbeschränkt also: kein Witness.
            format!("{restricted}\nw@* {OTHER}"),
            format!("{restricted}\n\"w@host\" {OTHER}"),
            format!("{restricted}\n!x@y,* {OTHER}"),
            // Ohne Schlüssel.
            "w@host namespaces=\"minds-witness\"".to_owned(),
        ] {
            assert!(!witness_only(&line, "w@host"), "{line}");
        }
        // Eine weitere, unbeschränkte Zeile desselben Principals …
        assert!(!witness_only(
            &format!("{restricted}\nw@host {OTHER}"),
            "w@host"
        ));
        // … oder derselbe Schlüssel unbeschränkt unter einem anderen
        // Principal (der Entwickler-Schlüssel im ssh-agent).
        assert!(!witness_only(
            &format!("human@x {KEY}\nhuman@x,w@host namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
        // Nicht genannt: kein Witness.
        assert!(!witness_only(&restricted, "other@host"));
        // Kommentare, andere Principals mit eigenen Schlüsseln, und das ganze
        // Feld, wie `find-principals` es ausgibt.
        let file = format!(
            "# Witness\nhuman@x {OTHER}\nw@host,w2@host namespaces=\"minds-witness\" {KEY}"
        );
        assert!(witness_only(&file, "w@host"));
        assert!(witness_only(&file, "w@host,w2@host"));
        assert!(!witness_only(
            &format!("w@host cert-authority,namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
        // Ein Muster für andere Principals (Commit-Signaturen) sperrt nichts.
        assert!(witness_only(
            &format!("*@firma.de namespaces=\"git\" {OTHER}\n{restricted}"),
            "w@host"
        ));
        assert!(witness_only(
            &format!("w@* namespaces=\"minds-witness\" {KEY}"),
            "w@host"
        ));
    }

    #[test]
    fn the_signers_file_never_comes_from_the_repository() {
        assert_eq!(
            trusted_signers_file(Some("/explicit")),
            Some(PathBuf::from("/explicit"))
        );
    }

    /// Ein Record, wie ihn die Pipeline zum Ablegen freigibt.
    fn scanned(record: &minds_core::replay::ReplayRecord) -> minds_redact::ScannedReplayRecord {
        minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .scan_replay(record.clone())
            .unwrap()
    }

    /// EA-18b: Replay-Records zählen nur unter `minds-anchor`, von einem
    /// darauf beschränkten Principal, und nur gegen die eigenen
    /// entscheidenden Befehle der Session.
    #[test]
    fn replay_records_count_only_when_signed_by_an_anchor_principal() {
        use minds_core::replay::{REPLAY_SCHEMA, ReplayRecord, ReplayResult, ReplayVerdict};

        assert!(minds_attest::ssh_keygen_available());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(
            std::process::Command::new("git")
                .current_dir(root)
                .args(["init", "-q", "--template="])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap()
                .status
                .success()
        );
        let store = minds_store::InRepoStore::open(root).unwrap();
        let mut session = minds_core::Session::new(
            minds_core::Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            minds_core::Model {
                provider: "t".into(),
                id: "t".into(),
            },
            minds_core::Intent::default(),
        );
        session.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls: vec![minds_core::ToolCall {
                name: "Bash".into(),
                arguments: r#"{"command":"cargo test"}"#.into(),
                capture: None,
                effect: None,
                outcome: Some(minds_core::ExecOutcome {
                    class: minds_core::ExecClass::Test,
                    runner: "cargo-test".into(),
                    command: vec!["cargo".into(), "test".into()],
                    cwd: Some(".".into()),
                    exit_code: None,
                    tests: Some(minds_core::TestCounts {
                        passed: 12,
                        failed: 0,
                        ignored: 0,
                    }),
                    benches: Vec::new(),
                }),
            }],
            parent: None,
            at: None,
        });
        let redacted = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        let id = store.put(&redacted).unwrap().id();
        let session = store.get(id).unwrap().unwrap();
        let decisive = minds_reader::replay::decisive(&session);
        let record = ReplayRecord {
            kind: minds_core::replay::REPLAY_KIND.into(),
            policy: None,
            project: None,
            schema: REPLAY_SCHEMA,
            commit: "ab".repeat(20),
            session: id.to_string(),
            interpretation_version: minds_reader::replay::INTERPRETATION_VERSION,
            results: vec![ReplayResult {
                turn: decisive[0].turn,
                call: decisive[0].call,
                argv: decisive[0].outcome.command.clone(),
                expected: minds_reader::replay::expected(decisive[0].outcome),
                observed: None,
                verdict: ReplayVerdict::Reproduced,
                reason: None,
            }],
            environment: Default::default(),
        };
        let commit = "ab".repeat(20);
        let record_id = store.put_replay(&scanned(&record)).unwrap();

        let key = root.join("ci");
        assert!(
            std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let public = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let signers_for = |namespace: &str| {
            let path = root.join(format!("signers-{namespace}"));
            std::fs::write(
                &path,
                format!("ci@pipeline namespaces=\"{namespace}\" {}", public.trim()),
            )
            .unwrap();
            path.to_str().unwrap().to_owned()
        };
        let anchor = signers_for("minds-anchor");
        let witness = signers_for("minds-witness");

        // Unsigniert: gezeigt, aber nicht gezählt.
        let trust = WitnessTrust::new(&store, Some(&anchor));
        let summary = trust.replay(id, Some(&session), Some(&commit)).unwrap();
        assert!(!summary.signed);
        assert_eq!((summary.decisive, summary.reproduced), (1, 1));

        // Unter `minds-anchor` signiert, von einem Anchor-Principal: zählt.
        let text = String::from_utf8(record.canonical_bytes().unwrap()).unwrap();
        let signature = minds_attest::ssh_sign_ns(&text, &key, minds_attest::NS_ANCHOR).unwrap();
        store.put_replay_signature(&record_id, &signature).unwrap();
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );

        // Derselbe Schlüssel als Witness-Principal: kein CI-Signer.
        let trust = WitnessTrust::new(&store, Some(&witness));
        assert!(
            !trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );

        // Gebunden an den Commit: Für einen anderen Commit gibt es keinen
        // Replay, ohne Commit-Kontext ebenso wenig.
        assert_eq!(
            trust.replay(id, Some(&session), Some(&"cd".repeat(20))),
            None
        );
        assert_eq!(trust.replay(id, Some(&session), None), None);

        // Unter fremdem Namespace signiert: zählt nicht.
        let wrong = minds_attest::ssh_sign_ns(&text, &key, minds_attest::NS_WITNESS).unwrap();
        store.put_replay_signature(&record_id, &wrong).unwrap();
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            !trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );

        // Ein zweiter, gültig signierter Record desselben Commits hebt das
        // nicht auf: Eine ungültige Signatur macht alle unsigniert.
        let mut again = record.clone();
        again.environment.pipeline = Some("2".into());
        let again_id = store.put_replay(&scanned(&again)).unwrap();
        let text = String::from_utf8(again.canonical_bytes().unwrap()).unwrap();
        let good = minds_attest::ssh_sign_ns(&text, &key, minds_attest::NS_ANCHOR).unwrap();
        store.put_replay_signature(&again_id, &good).unwrap();
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            !trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );

        // Eine fremde Session hat keinen Replay.
        let other: SessionId = format!("b3-{}", "00".repeat(32)).parse().unwrap();
        assert_eq!(trust.replay(other, Some(&session), Some(&commit)), None);

        // Beide Signaturen gültig: signiert.
        let first = String::from_utf8(record.canonical_bytes().unwrap()).unwrap();
        let first = minds_attest::ssh_sign_ns(&first, &key, minds_attest::NS_ANCHOR).unwrap();
        store.put_replay_signature(&record_id, &first).unwrap();
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );

        // Wird der `record`-Blob eines der beiden ersetzt, zählt der andere
        // nicht mehr als signiert: Ein signierter Fehlschlag ließe sich sonst
        // still entfernen.
        let run = |args: &[&str], input: Option<&str>| -> String {
            use std::io::Write as _;
            let mut child = std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            if let Some(input) = input {
                stdin.write_all(input.as_bytes()).unwrap();
            }
            drop(stdin);
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        let forged = String::from_utf8(again.canonical_bytes().unwrap())
            .unwrap()
            .replace("\"turn\"", "\"turn_\"");
        let blob = run(&["hash-object", "-w", "--stdin"], Some(&forged));
        let tree = run(&["mktree"], Some(&format!("100644 blob {blob}\trecord\n")));
        let replaced = run(&["commit-tree", &tree, "-m", "replaced"], None);
        run(
            &[
                "update-ref",
                &format!("refs/minds/anchors/replay/{}", again_id.hex()),
                &replaced,
            ],
            None,
        );
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            !trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );
        // Ebenso unlesbare Bytes unter einem anderen Record-Namen.
        let blob = run(&["hash-object", "-w", "--stdin"], Some("garbage"));
        let tree = run(&["mktree"], Some(&format!("100644 blob {blob}\trecord\n")));
        let replaced = run(&["commit-tree", &tree, "-m", "garbage"], None);
        run(
            &[
                "update-ref",
                &format!("refs/minds/anchors/replay/{}", "ef".repeat(32)),
                &replaced,
            ],
            None,
        );
        let trust = WitnessTrust::new(&store, Some(&anchor));
        assert!(
            !trust
                .replay(id, Some(&session), Some(&commit))
                .unwrap()
                .signed
        );
    }

    #[test]
    fn only_principals_restricted_to_the_anchor_namespace_sign_replays() {
        let anchor = format!("ci@x namespaces=\"minds-anchor\" {KEY}\n");
        assert!(anchor_only(&anchor, "ci@x"));
        let witness = format!("ci@x namespaces=\"minds-witness\" {KEY}\n");
        assert!(!anchor_only(&witness, "ci@x"));
        let open = format!("ci@x {KEY}\n");
        assert!(!anchor_only(&open, "ci@x"));
        // Derselbe Schlüssel zusätzlich unbeschränkt: kein CI-Signer.
        let mixed =
            format!("ci@x namespaces=\"minds-anchor\" {KEY}\nother {OTHER}\nother2 {KEY}\n");
        assert!(!anchor_only(&mixed, "ci@x"));
    }
}
