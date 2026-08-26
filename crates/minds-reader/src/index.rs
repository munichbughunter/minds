//! Der Index: alles, was die Seite braucht, einmal aus Git und Store gezogen.
//!
//! Der Reader ist **zustandslos** (Architektur-Prinzip 6): Er hält keine
//! Datenbank, sondern baut bei jedem Lauf ein Bild aus zwei Quellen — den
//! Sessions im Store und den Trailern in der Historie. Dieses Modul ist dieses
//! Bild.
//!
//! ```text
//!   Store  ──list/get──►  SessionId → Session
//!   Repo   ──revwalk───►  Commit    → [SessionId], Change-Id, Betreff
//! ```
//!
//! # Zwei Richtungen, eine Wahrheit
//!
//! Die verbindliche Verknüpfung ist der **Trailer** (Commit → Session); genau
//! die wird hier gesammelt. Der Store liefert dazu die Nutzlast. Was der Index
//! *nicht* tut, ist raten: Eine Session ohne Trailer taucht in `sessions` auf,
//! aber unter keinem Commit — sie ist erfasst, aber (noch) nicht mit Code
//! verbunden. Das ist ein legitimer Zustand und kein Fehler.
//!
//! Der Store-Index steuert die **vermuteten** Kanten bei (importierte
//! Sessions, Datei-Schnittmenge plus Zeitfenster). Welche Quelle eine Kante
//! belegt, bleibt je Kante erhalten ([`Index::evidence_of`]) — der Reader
//! darf eine Vermutung nie wie einen Beleg zeigen.
//!
//! # Eine kaputte Session bringt nicht die Seite zu Fall
//!
//! Lässt sich eine Session nicht auflösen (Inhalt passt nicht zum Hash, kaputtes
//! JSON, Tombstone nach `minds forget`), wird sie übersprungen und **mit
//! Ursache vermerkt** statt verschwiegen — siehe [`Index::degraded`]. Der
//! Reader ist ein Leser; er darf an einem faulen Eintrag nicht sterben, aber er
//! darf ihn auch nicht unterschlagen. `minds fsck` ist das Werkzeug, um dem
//! nachzugehen; ein Tombstone dagegen ist gewollt und kein Defekt.

use std::collections::{BTreeMap, BTreeSet};

use minds_core::evidence::{Seal, SealOutcome};
use minds_core::{
    ChangeId, ContentHash, EvidenceMark, EvidenceSource, Session, SessionId, Trailer,
};
use minds_git::{CommitId, Repo};
use minds_metrics::Coverage;
use minds_store::{ContextStore, StoreError};

use crate::error::Result;
use crate::text::sanitize;

/// Warum eine im Store gelistete Session nicht gezeigt werden kann.
///
/// **Vergessen** ist ein gewollter Zustand (DSGVO, `minds forget`), alles
/// andere ein Defekt, dem `minds fsck` nachgeht — dieselbe Trennung wie in
/// [`StoreError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// Getilgt per `minds forget`; `reason` ist der hinterlegte Grund.
    Forgotten {
        /// Der beim Vergessen hinterlegte Grund, entschärft.
        reason: String,
    },
    /// Der Inhalt hasht nicht auf die Id, unter der er liegt.
    Corrupt,
    /// Der Inhalt ist kein gültiges Session-JSON.
    Malformed,
    /// Der Inhalt ist nicht als redigiert markiert.
    Unredacted,
    /// Gelistet, aber nicht auflösbar.
    Missing,
    /// Ein anderer Fehler beim Holen dieser einen Session, entschärft.
    Failed {
        /// Die Fehlermeldung, entschärft.
        message: String,
    },
}

impl Degradation {
    /// Ein kurzes Wort für die Anzeige: `vergessen` oder `unlesbar`.
    pub fn word(&self) -> &'static str {
        match self {
            Degradation::Forgotten { .. } => "vergessen",
            _ => "unlesbar",
        }
    }

    /// `true` für den gewollten Zustand — den Tombstone.
    pub fn is_forgotten(&self) -> bool {
        matches!(self, Degradation::Forgotten { .. })
    }

    fn of(err: &StoreError) -> Self {
        match err {
            StoreError::Forgotten { reason, .. } => Degradation::Forgotten {
                reason: sanitize(reason),
            },
            StoreError::Corrupt { .. } => Degradation::Corrupt,
            StoreError::Malformed { .. } => Degradation::Malformed,
            StoreError::Unredacted { .. } => Degradation::Unredacted,
            other => Degradation::Failed {
                message: sanitize(&other.to_string()),
            },
        }
    }
}

/// Eine Session, die im Store gelistet ist, aber nicht gezeigt werden kann.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degraded {
    /// Die betroffene Session.
    pub id: SessionId,
    /// Warum.
    pub cause: Degradation,
}

/// Eine Content-Übergabe zwischen zwei Sessions: `to` las exakt die Bytes,
/// die `from` schrieb — festgestellt über identische [`ContentHash`]e am
/// selben Pfad, nicht beobachtet und nicht behauptet. Die **Richtung** ist
/// aus der Effekt-Art abgeleitet (Write → Read), nicht zeitlich belegt —
/// belegt ist allein die Byte-Gleichheit. Genau dafür gibt es
/// `(ContentDerived, Verified)`: eine **nachgerechnete** Beziehung
/// (ADR-0011; die erste Stelle, die diesen Mark produziert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLink {
    /// Die schreibende Session.
    pub from: SessionId,
    /// Die lesende Session.
    pub to: SessionId,
    /// Der gemeinsame Pfad, entschärft.
    pub path: String,
    /// Der übereinstimmende Inhalts-Hash.
    pub hash: ContentHash,
}

impl ContentLink {
    /// Der Beleg dieser Kante — per Konstruktion nachgerechnet.
    pub fn mark(&self) -> EvidenceMark {
        EvidenceMark {
            source: EvidenceSource::ContentDerived,
            status: minds_core::EvidenceStatus::Verified,
        }
    }
}

/// Das gesammelte Bild aus Store und Historie.
///
/// Zwei Quellen verknüpfen Commits mit Sessions: die **Trailer** (verbindlich)
/// und der **Store-Index** (heuristisch, für importierte Sessions). Der Index
/// führt beide zusammen und merkt sich in [`Index::observed`], welche Sessions
/// über mindestens einen Trailer belegt sind — der Rest ist „vermutet" und wird
/// im Reader als solcher gezeigt.
#[derive(Debug, Default, Clone)]
pub struct Index {
    sessions: BTreeMap<SessionId, Session>,
    commits: BTreeMap<CommitId, Vec<SessionId>>,
    /// Die Gegenrichtung zu `commits`, damit `commits_of` nicht die ganze
    /// Historie durchsucht.
    by_session: BTreeMap<SessionId, Vec<CommitId>>,
    /// Woher jede Kante bekannt ist; bei mehreren Belegen gewinnt der beste
    /// (Merge-Regel aus ADR-0011: erst Quelle, dann Status).
    evidence: BTreeMap<(CommitId, SessionId), EvidenceMark>,
    /// Die Seals je Session: `(seal_id, Seal, signiert?)`, in
    /// Rückverweis-Reihenfolge.
    seals: BTreeMap<SessionId, Vec<(ContentHash, Seal, bool)>>,
    /// Sessions, deren Seal-Material beim Lesen als manipuliert auffiel.
    seal_tampered: BTreeSet<SessionId>,
    /// Sessionlose Block-Seals: zurückgehaltene Sessions, deren einziger
    /// Beleg der Seal ist (`outcome=storage_policy_rejected_payload`).
    rejected: Vec<(ContentHash, Seal)>,
    /// Alle lesbaren Seals des Namensraums, für die `previous`-Auflösung und
    /// die Epochen-Zählung: `seal_id → (stored?, root, previous)`.
    all_seals: BTreeMap<ContentHash, (bool, ContentHash, Option<ContentHash>)>,
    /// Der Evidence-DAG als **Projektion** (Phase 6): Content-Übergaben
    /// zwischen Sessions, zur Ladezeit aus den gespeicherten Inhalts-Hashes
    /// abgeleitet — nie gespeichert, jederzeit neu berechenbar. Die Chain
    /// bleibt die temporale Wahrheit; das hier sind semantische Beziehungen
    /// darüber.
    content_links: Vec<ContentLink>,
    changes: BTreeMap<CommitId, ChangeId>,
    subjects: BTreeMap<CommitId, String>,
    /// `(agent, lineage.local_id)` → Id — für die symbolischen Endpunkte der
    /// Kanten (`Endpoint::Session`).
    locals: BTreeMap<(String, String), SessionId>,
    observed: BTreeSet<SessionId>,
    degraded: Vec<Degraded>,
    commits_total: u64,
    covered: u64,
}

impl Index {
    /// Zieht Sessions, Trailer und den Store-Index aus Store und Repository.
    pub fn build(repo: &Repo, store: &dyn ContextStore) -> Result<Self> {
        let mut sessions = BTreeMap::new();
        let mut degraded = Vec::new();

        for id in store.list()? {
            match store.get(id) {
                // Sessions ohne erfasste Absicht tragen nichts bei und werden
                // gar nicht erst aufgenommen — dann verschwinden sie überall:
                // Übersicht, Datei-Panels und die Zeilen-Zuordnung. Das ist der
                // frühere „(kein Prompt erfasst)"-Ballast, an einer Stelle
                // ausgesiebt.
                Ok(Some(session)) if session.intent.request.trim().is_empty() => {}
                Ok(Some(session)) => {
                    sessions.insert(id, session);
                }
                // Im Store gelistet, aber nicht auflösbar — vermerken, nicht
                // sterben.
                Ok(None) => degraded.push(Degraded {
                    id,
                    cause: Degradation::Missing,
                }),
                Err(err) => degraded.push(Degraded {
                    id,
                    cause: Degradation::of(&err),
                }),
            }
        }
        let known: BTreeSet<SessionId> = sessions
            .keys()
            .copied()
            .chain(degraded.iter().map(|d| d.id))
            .collect();

        let mut index = Self {
            sessions,
            degraded,
            ..Self::default()
        };

        // 1. Die Trailer — die verbindliche Richtung. Nur Kanten zu Sessions,
        //    die wir behalten haben (keine ausgesiebten). Die Message wird je
        //    Commit einmal gelesen; Session-Ids, Change-Id und Betreff kommen
        //    aus demselben Text.
        if let Some(head) = repo.head()?.commit() {
            for commit in repo.revwalk(head)? {
                let commit = commit?;
                index.commits_total += 1;
                let message = repo.message_of(commit)?;
                let mut covered = false;
                for id in Trailer::session_ids(&message) {
                    covered |= known.contains(&id);
                    if index.sessions.contains_key(&id) {
                        index.link(commit, id, EvidenceMark::of(EvidenceSource::Observed));
                    }
                }
                if let Some(change) = Trailer::change_id(&message) {
                    index.changes.insert(commit, change);
                }
                if let Some(subject) = message.lines().map(str::trim).find(|l| !l.is_empty()) {
                    index.subjects.insert(commit, sanitize(subject));
                }
                // Für die Abdeckung zählt die Verknüpfung, nicht die
                // Lesbarkeit: Ein Tombstone ist eine erfasste, bewusst getilgte
                // Session — derselbe Vertrag wie bei `minds metrics`.
                index.covered += u64::from(covered);
            }
        }

        // 2. Der Store-Index — die vermuteten Kanten (z. B. importiert). Was
        //    schon über einen Trailer da ist, behält seinen besseren Beleg.
        for (hex, links) in store.index()?.iter() {
            let Ok(commit) = hex.parse::<CommitId>() else {
                continue;
            };
            for link in links {
                if index.sessions.contains_key(&link.session) {
                    index.link(commit, link.session, link.evidence);
                }
            }
        }

        // 3. Die Evidence-Chain (ADR-0011): Seals je Session und die
        //    sessionlosen Block-Seals. Fail-soft — ein unlesbarer Seal wird
        //    vermerkt, nie ein Absturz.
        index.load_seals(store, &known);

        index.finish();
        Ok(index)
    }

    /// Baut einen Index aus fertigen Teilen — für Tests und für Aufrufer, die
    /// ihre Daten schon haben. Alle Verknüpfungen gelten dabei als beobachtet.
    ///
    /// Auch hier fliegen absichtslose Sessions und Kanten auf sie raus, damit
    /// der Test denselben Vertrag prüft wie [`Index::build`].
    pub fn from_parts(
        sessions: BTreeMap<SessionId, Session>,
        commits: BTreeMap<CommitId, Vec<SessionId>>,
    ) -> Self {
        let sessions: BTreeMap<SessionId, Session> = sessions
            .into_iter()
            .filter(|(_, s)| !s.intent.request.trim().is_empty())
            .collect();
        let mut index = Self {
            sessions,
            ..Self::default()
        };
        for (commit, ids) in commits {
            let mut any = false;
            for id in ids {
                if index.sessions.contains_key(&id) {
                    index.link(commit, id, EvidenceMark::of(EvidenceSource::Observed));
                    any = true;
                }
            }
            if any {
                index.commits_total += 1;
                index.covered += 1;
            }
        }
        index.finish();
        index
    }

    /// Ergänzt Change-Ids je Commit — für Tests, die den Strang
    /// Session → Commit → Change prüfen.
    pub fn with_changes(mut self, changes: BTreeMap<CommitId, ChangeId>) -> Self {
        self.changes.extend(changes);
        self
    }

    /// Ergänzt degradierte Einträge — für Tests, die den Leerlauf einer
    /// kaputten oder vergessenen Session prüfen.
    pub fn with_degraded(mut self, degraded: Vec<Degraded>) -> Self {
        self.degraded.extend(degraded);
        self
    }

    /// Ergänzt Seals je Session — für Tests, die das Evidence-Verdikt ohne
    /// Store prüfen. `(seal_id, Seal, signiert?)`, wie beim Laden.
    pub fn with_seals(mut self, id: SessionId, seals: Vec<(ContentHash, Seal, bool)>) -> Self {
        for (seal_id, seal, _) in &seals {
            let stored = matches!(seal.outcome, SealOutcome::Stored { .. });
            self.all_seals.insert(
                seal_id.clone(),
                (stored, seal.root.clone(), seal.previous.clone()),
            );
        }
        self.seals.entry(id).or_default().extend(seals);
        self
    }

    /// Trägt eine Kante ein; ein besserer Beleg ersetzt einen schwächeren,
    /// ein schwächerer ändert nichts ([`EvidenceMark::merge`]).
    fn link(&mut self, commit: CommitId, id: SessionId, evidence: EvidenceMark) {
        let slot = self.evidence.entry((commit, id)).or_insert(evidence);
        *slot = slot.merge(evidence);
        if evidence.source == EvidenceSource::Observed {
            self.observed.insert(id);
        }
        push_unique(self.commits.entry(commit).or_default(), id);
        push_unique(self.by_session.entry(id).or_default(), commit);
    }

    /// Lädt die Seals aller bekannten Sessions und die sessionlosen
    /// Block-Seals. Jeder Lesefehler degradiert sichtbar statt zu stürzen.
    fn load_seals(&mut self, store: &dyn ContextStore, known: &BTreeSet<SessionId>) {
        for id in known {
            let Ok(seal_ids) = store.seals_of(*id) else {
                continue;
            };
            for seal_id in seal_ids {
                match store.seal_text(&seal_id) {
                    Ok(Some(text)) => match Seal::parse(&text) {
                        Ok(seal) => {
                            let signed = matches!(store.seal_signature(&seal_id), Ok(Some(_)));
                            self.seals
                                .entry(*id)
                                .or_default()
                                .push((seal_id, seal, signed));
                        }
                        Err(_) => {
                            self.seal_tampered.insert(*id);
                        }
                    },
                    Ok(None) => {
                        // Baumelnder Rückverweis: kein Beweis, keine
                        // Manipulation — die Session gilt schlicht als (noch)
                        // unversiegelt an dieser Stelle.
                    }
                    Err(StoreError::SealMismatch { .. }) => {
                        self.seal_tampered.insert(*id);
                    }
                    Err(_) => {}
                }
            }
        }
        // Einmal über den ganzen Namensraum: die Block-Seals (per
        // Konstruktion sessionlos) und die Übersicht für die
        // `previous`-Auflösung über Epochen-/Session-Grenzen hinweg.
        if let Ok(all) = store.list_seals() {
            for seal_id in all {
                let Ok(Some(text)) = store.seal_text(&seal_id) else {
                    continue;
                };
                let Ok(seal) = Seal::parse(&text) else {
                    continue;
                };
                let stored = matches!(seal.outcome, SealOutcome::Stored { .. });
                self.all_seals.insert(
                    seal_id.clone(),
                    (stored, seal.root.clone(), seal.previous.clone()),
                );
                if !stored {
                    self.rejected.push((seal_id, seal));
                }
            }
        }
    }

    /// Die sessionlosen Block-Seals — zurückgehaltene Sessions, deren
    /// einziger Beleg der Seal ist.
    pub fn rejected_seals(&self) -> &[(ContentHash, Seal)] {
        &self.rejected
    }

    /// Das Evidence-Verdikt einer Session aus ihren Seals — `None`, wenn es
    /// keinerlei Seal-Material gibt (vor Evidence-Chain erfasst).
    ///
    /// Eine Projektion auf [`evidence_report`](Self::evidence_report) — es
    /// gibt genau **eine** Verdikt-Rechnung, keine zwei Semantiken.
    pub fn evidence_state(&self, id: SessionId) -> Option<crate::model::EvidenceState> {
        self.evidence_report(id).map(|report| report.state)
    }

    /// Der volle Evidence-Report einer Session: Verdikt, Epochen mit
    /// Coverage, Scope, Signatur-Lage und die Grenzen des Proof-Modells —
    /// **ein** Read-Model für CLI, TUI und Audit. `None` heißt Legacy
    /// (vor Evidence-Chain erfasst), nie „Bug".
    pub fn evidence_report(&self, id: SessionId) -> Option<crate::model::EvidenceReport> {
        use crate::model::{
            EpochLink, EpochReport, EvidenceReport, EvidenceState, EvidenceVerdict,
        };
        use crate::text::sanitize;

        let tampered = self.seal_tampered.contains(&id);
        let seals = self.seals.get(&id);
        if !tampered && seals.is_none_or(|s| s.is_empty()) {
            return None;
        }
        let seals = seals.map(Vec::as_slice).unwrap_or(&[]);

        let mut events = 0u64;
        let mut gaps = 0u64;
        let mut pre_chain = 0u64;
        let mut signed = 0usize;
        let mut rejected = false;
        // Dieselbe Epochen-Logik wie `minds verify` (ADR-0011): Epochen sind
        // eigene Sessions — ein `previous` auf einen auflösbaren
        // `stored`-Seal SCHLIESST die Kette; ein Block-Seal als Vorgänger
        // ist nur dann Geschichte, wenn sein Root identisch ist
        // (Policy-Fix), sonst eine zurückgewiesene Epoche.
        let in_set: BTreeSet<&ContentHash> = seals.iter().map(|(id, _, _)| id).collect();
        let mut entry_points = 0usize;
        let mut internal_targets: BTreeSet<&ContentHash> = BTreeSet::new();
        let mut chain_closed = true;
        let mut epochs = Vec::with_capacity(seals.len());
        for (seal_id, seal, is_signed) in seals {
            events += seal.events;
            gaps += seal.gaps;
            pre_chain += seal.pre_chain;
            signed += usize::from(*is_signed);
            let link = match &seal.previous {
                None => {
                    entry_points += 1;
                    EpochLink::Start
                }
                Some(prev) if in_set.contains(prev) => {
                    if !internal_targets.insert(prev) {
                        chain_closed = false; // Fork
                    }
                    EpochLink::Chained
                }
                Some(prev) => match self.all_seals.get(prev) {
                    Some((true, _, _)) => {
                        entry_points += 1;
                        EpochLink::Chained
                    }
                    Some((false, prev_root, _)) if prev_root == &seal.root => {
                        entry_points += 1;
                        EpochLink::Chained
                    }
                    Some((false, _, _)) => {
                        rejected = true;
                        chain_closed = false;
                        EpochLink::RejectedBefore
                    }
                    None => {
                        chain_closed = false;
                        EpochLink::Unresolved
                    }
                },
            };
            epochs.push(EpochReport {
                seal_id: seal_id.clone(),
                root: seal.root.clone(),
                scope: sanitize(&seal.scope),
                first_seq: seal.first_seq,
                last_seq: seal.last_seq,
                events: seal.events,
                gaps: seal.gaps,
                pre_chain: seal.pre_chain,
                stored: matches!(seal.outcome, SealOutcome::Stored { .. }),
                signed: *is_signed,
                link,
                last_event_at: sanitize(&seal.last_event_at),
            });
        }
        if entry_points != 1 {
            chain_closed = false;
        }
        if seals.is_empty() {
            chain_closed = false;
        }

        let verdict = if tampered {
            EvidenceVerdict::Tampered
        } else if gaps == 0 && pre_chain == 0 && !rejected && chain_closed {
            EvidenceVerdict::Verified
        } else {
            EvidenceVerdict::Incomplete
        };
        let scope = epochs
            .first()
            .map(|epoch: &EpochReport| epoch.scope.clone());
        Some(EvidenceReport {
            state: EvidenceState {
                verdict,
                seals: seals.len(),
                events,
                gaps,
                pre_chain,
                rejected,
                chain_closed,
                signed,
            },
            scope,
            epochs,
            limitations: minds_core::evidence::DOES_NOT_PROVE,
        })
    }

    /// Leitet die abgeleiteten Tabellen ab, sobald alle Kanten stehen.
    fn finish(&mut self) {
        self.locals = self
            .sessions
            .iter()
            .filter_map(|(id, session)| {
                let lineage = session.lineage.as_ref()?;
                Some(((session.agent.name.clone(), lineage.local_id.clone()), *id))
            })
            .collect();
        self.content_links = self.project_content_links();
    }

    /// Die DAG-Projektion: Write-Hash der einen Session == Read-Hash der
    /// anderen, am selben Pfad. Deterministisch sortiert; Duplikate (mehrere
    /// Aufrufe derselben Datei) fallen zusammen.
    fn project_content_links(&self) -> Vec<ContentLink> {
        use minds_core::EffectKind;

        // (pfad, hash) → Schreiber bzw. Leser.
        let mut writes: BTreeMap<(String, ContentHash), BTreeSet<SessionId>> = BTreeMap::new();
        let mut reads: BTreeMap<(String, ContentHash), BTreeSet<SessionId>> = BTreeMap::new();
        for (id, session) in &self.sessions {
            for call in session.turns.iter().flat_map(|t| &t.tool_calls) {
                let Some(effect) = &call.effect else { continue };
                let (Some(path), Some(hash)) = (&effect.path, &effect.content) else {
                    continue;
                };
                let key = (path.clone(), hash.clone());
                match effect.kind {
                    EffectKind::Write => {
                        writes.entry(key).or_default().insert(*id);
                    }
                    EffectKind::Read => {
                        reads.entry(key).or_default().insert(*id);
                    }
                    _ => {}
                }
            }
        }

        let mut links = Vec::new();
        for (key, writers) in &writes {
            let Some(readers) = reads.get(key) else {
                continue;
            };
            for from in writers {
                for to in readers {
                    if from == to {
                        continue; // die eigene Datei zu lesen ist keine Übergabe
                    }
                    links.push(ContentLink {
                        from: *from,
                        to: *to,
                        path: sanitize(&key.0),
                        hash: key.1.clone(),
                    });
                }
            }
        }
        links.sort_by(|a, b| {
            (a.from, a.to, &a.path)
                .cmp(&(b.from, b.to, &b.path))
                .then_with(|| a.hash.cmp(&b.hash))
        });
        links.dedup();
        links
    }

    /// Alle Content-Übergaben des Repos, deterministisch sortiert.
    pub fn content_links(&self) -> &[ContentLink] {
        &self.content_links
    }

    /// Die Content-Übergaben, an denen eine Session beteiligt ist.
    pub fn content_links_of(&self, id: SessionId) -> Vec<&ContentLink> {
        self.content_links
            .iter()
            .filter(|l| l.from == id || l.to == id)
            .collect()
    }

    /// Die Epochen-Position einer Session: `(k, n)` — ihr letzter Seal hat
    /// k−1 auflösbare Vorfahren und n−k transitive Nachfahren. Bei einer
    /// linearen Kette ist das „Epoche k von n"; bei einem Fork zählt n
    /// **alle** Zweige — eine Anzeige, keine Ketten-Garantie (die macht
    /// `coverage_complete`). `None` ohne Seals oder bei trivialer Kette.
    ///
    /// Rein aus den Seals gerechnet (Projektion): Vorfahren über `previous`,
    /// Nachfahren über die Umkehrung — mit Zyklus-Schutz, denn `previous`
    /// ist Repo-Inhalt, kein Vertrauensanker.
    pub fn epoch_position(&self, id: SessionId) -> Option<(usize, usize)> {
        let (last_id, _, _) = self.seals.get(&id)?.last()?;

        // Vorfahren zaehlen — nur AUFLOESBARE Glieder (ein baumelndes
        // `previous` ist eine offene Kette, kein Vorfahr), mit Zyklus-Schutz
        // inklusive des eigenen Seals: `previous` ist Repo-Inhalt, kein
        // Vertrauensanker.
        let mut seen: BTreeSet<&ContentHash> = BTreeSet::new();
        seen.insert(last_id);
        let mut ancestors = 0usize;
        let mut cursor = self.all_seals.get(last_id).and_then(|(_, _, p)| p.as_ref());
        while let Some(prev) = cursor {
            if !self.all_seals.contains_key(prev) || !seen.insert(prev) {
                break; // baumelnd oder Zyklus — nicht weiterzaehlen
            }
            ancestors += 1;
            cursor = self.all_seals.get(prev).and_then(|(_, _, p)| p.as_ref());
        }

        // Nachfahren: wer setzt (transitiv) auf uns auf?
        let mut children: BTreeMap<&ContentHash, Vec<&ContentHash>> = BTreeMap::new();
        for (seal_id, (_, _, previous)) in &self.all_seals {
            if let Some(prev) = previous {
                children.entry(prev).or_default().push(seal_id);
            }
        }
        let mut descendants = 0usize;
        let mut frontier = vec![last_id];
        // Gemeinsame Sicht mit dem Vorfahren-Walk: In einem Zyklus darf ein
        // Glied nie doppelt zählen (als Vorfahr UND Nachfahr).
        let mut visited: BTreeSet<&ContentHash> = seen;
        visited.insert(last_id);
        while let Some(node) = frontier.pop() {
            for child in children.get(node).into_iter().flatten() {
                if visited.insert(child) {
                    descendants += 1;
                    frontier.push(child);
                }
            }
        }

        let total = ancestors + 1 + descendants;
        (total > 1).then_some((ancestors + 1, total))
    }

    /// `true`, wenn diese Session über mindestens einen Trailer belegt ist (im
    /// Gegensatz zu nur heuristisch über den Store-Index verknüpft).
    pub fn is_observed(&self, id: SessionId) -> bool {
        self.observed.contains(&id)
    }

    /// Woher die Kante Commit → Session bekannt ist; `None`, wenn es sie
    /// nicht gibt.
    pub fn evidence_of(&self, commit: CommitId, id: SessionId) -> Option<EvidenceMark> {
        self.evidence.get(&(commit, id)).copied()
    }

    /// Der beste Beleg, mit dem diese Session an irgendeinem Commit hängt —
    /// `None`, wenn sie mit keinem Code verbunden ist.
    pub fn evidence_for_session(&self, id: SessionId) -> Option<EvidenceMark> {
        self.by_session
            .get(&id)?
            .iter()
            .filter_map(|commit| self.evidence_of(*commit, id))
            .max()
    }

    /// Die Change-Id aus dem Trailer dieses Commits.
    pub fn change_of(&self, commit: CommitId) -> Option<&ChangeId> {
        self.changes.get(&commit)
    }

    /// Die Change-Ids aller Commits, die diese Session tragen — dedupliziert,
    /// in Commit-Reihenfolge.
    pub fn changes_of(&self, id: SessionId) -> Vec<ChangeId> {
        let mut out: Vec<ChangeId> = Vec::new();
        for commit in self.commits_of(id) {
            if let Some(change) = self.changes.get(&commit)
                && !out.contains(change)
            {
                out.push(change.clone());
            }
        }
        out
    }

    /// Der Betreff (erste Zeile der Message) eines Commits, entschärft.
    pub fn subject_of(&self, commit: CommitId) -> Option<&str> {
        self.subjects
            .get(&commit)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// Löst den symbolischen Endpunkt einer Kante (`agent` + `local_id`) zu
    /// einer Session-Id auf, sofern diese Session im Index ist.
    pub fn resolve_endpoint(&self, agent: &str, local_id: &str) -> Option<SessionId> {
        self.locals
            .get(&(agent.to_string(), local_id.to_string()))
            .copied()
    }

    /// Die Session zu einer Id.
    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id)
    }

    /// Alle Sessions, nach Id sortiert (die Ordnung von [`SessionId`]).
    pub fn sessions(&self) -> impl Iterator<Item = (&SessionId, &Session)> {
        self.sessions.iter()
    }

    /// Die Sessions, deren Trailer an diesem Commit stehen.
    pub fn sessions_of(&self, commit: CommitId) -> &[SessionId] {
        self.commits.get(&commit).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Die Commits, die diese Session tragen — die Gegenrichtung zu
    /// [`Index::sessions_of`]. Reihenfolge ist die von [`CommitId`] (Hash),
    /// also deterministisch, aber nicht chronologisch.
    pub fn commits_of(&self, id: SessionId) -> Vec<CommitId> {
        let mut commits = self.by_session.get(&id).cloned().unwrap_or_default();
        commits.sort();
        commits
    }

    /// Wie viele Sessions insgesamt bekannt sind.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// `true`, wenn keine Session bekannt ist — der Empty-State der Seite.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Wie viele im Store gelistete Sessions sich nicht zeigen ließen —
    /// Tombstones eingeschlossen. Die Ursachen stehen in [`Index::degraded`].
    pub fn unreadable(&self) -> usize {
        self.degraded.len()
    }

    /// Die gelisteten, aber nicht zeigbaren Sessions, je mit Ursache.
    pub fn degraded(&self) -> &[Degraded] {
        &self.degraded
    }

    /// Wie viele Commits mindestens eine Session tragen.
    pub fn attributed_commits(&self) -> usize {
        self.commits.len()
    }

    /// Die Abdeckung der Historie: Wie viele Commits ab HEAD eine über
    /// Trailer verknüpfte, im Store bekannte Session tragen.
    pub fn coverage(&self) -> Coverage {
        Coverage {
            commits_total: self.commits_total,
            commits_with_context: self.covered,
        }
    }
}

/// Fügt `id` an, wenn sie nicht schon in `ids` steht.
fn push_unique<T: PartialEq>(ids: &mut Vec<T>, id: T) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal_for(
        id: SessionId,
        gaps: u64,
        previous: Option<ContentHash>,
    ) -> (ContentHash, Seal, bool) {
        let seal = Seal {
            root: ContentHash::from_bytes([7u8; 32]),
            agent: "claude-code".into(),
            scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
            first_seq: 0,
            last_seq: 3,
            events: 4,
            gaps,
            pre_chain: 0,
            outcome: SealOutcome::Stored {
                session: id.to_string(),
            },
            previous,
            last_event_at: "2026-08-24T10:00:00Z".into(),
        };
        let seal_id = Seal::id_of_text(&seal.to_text().unwrap());
        (seal_id, seal, false)
    }

    #[test]
    fn content_links_are_a_projection_with_verified_content_marks() {
        use minds_core::{Effect, EffectKind, ToolCall, Turn};

        // A schreibt foo.rs mit Hash H, B liest foo.rs mit demselben H —
        // die Uebergabe ist NACHGERECHNET, nicht beobachtet: der erste Ort,
        // der (ContentDerived, Verified) produziert (ADR-0011, Phase 6).
        let hash = ContentHash::from_bytes([5u8; 32]);
        let mk = |req: &str, kind: EffectKind, h: &ContentHash| {
            let mut s = minds_core::Session::new(
                minds_core::Agent {
                    name: "x".into(),
                    version: "1".into(),
                },
                minds_core::Model {
                    provider: "p".into(),
                    id: "m".into(),
                },
                minds_core::Intent {
                    request: req.into(),
                    ..Default::default()
                },
            );
            s.redaction.applied = true;
            s.turns.push(Turn {
                role: minds_core::Role::Assistant,
                text: String::new(),
                tool_calls: vec![ToolCall {
                    name: "T".into(),
                    arguments: String::new(),
                    capture: None,
                    effect: Some(Effect {
                        kind,
                        path: Some("foo.rs".into()),
                        content: Some(h.clone()),
                    }),
                }],
                parent: None,
                at: None,
            });
            s
        };
        let a: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let b: SessionId = format!("b3-{}", "b".repeat(64)).parse().unwrap();
        let c: SessionId = format!("b3-{}", "c".repeat(64)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(a, mk("schreibt", EffectKind::Write, &hash));
        sessions.insert(b, mk("liest", EffectKind::Read, &hash));
        // C liest denselben Pfad mit ANDEREM Hash: keine Uebergabe.
        sessions.insert(
            c,
            mk(
                "liest anderes",
                EffectKind::Read,
                &ContentHash::from_bytes([6u8; 32]),
            ),
        );

        let index = Index::from_parts(sessions, BTreeMap::new());
        let links = index.content_links();
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].from, a);
        assert_eq!(links[0].to, b);
        assert_eq!(links[0].path, "foo.rs");
        assert_eq!(
            links[0].mark(),
            EvidenceMark {
                source: EvidenceSource::ContentDerived,
                status: minds_core::EvidenceStatus::Verified,
            }
        );
        assert_eq!(index.content_links_of(c).len(), 0);

        // Projektion: zweimal gebaut, identisch — nichts wird gespeichert.
        let again = Index::from_parts(
            index.sessions().map(|(id, s)| (*id, s.clone())).collect(),
            BTreeMap::new(),
        );
        assert_eq!(again.content_links(), links);
    }

    #[test]
    fn epoch_position_counts_a_linear_chain_and_survives_adversarial_previous() {
        use minds_core::evidence::{Seal, SealOutcome};

        let sid_a: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let sid_b: SessionId = format!("b3-{}", "b".repeat(64)).parse().unwrap();
        let mk_session = |req: &str| {
            let mut s = minds_core::Session::new(
                minds_core::Agent {
                    name: "claude-code".into(),
                    version: "1".into(),
                },
                minds_core::Model {
                    provider: "p".into(),
                    id: "m".into(),
                },
                minds_core::Intent {
                    request: req.into(),
                    ..Default::default()
                },
            );
            s.redaction.applied = true;
            s
        };
        let mk_seal = |session: SessionId, root: u8, previous: Option<ContentHash>| {
            let seal = Seal {
                root: ContentHash::from_bytes([root; 32]),
                agent: "claude-code".into(),
                scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
                first_seq: 0,
                last_seq: 0,
                events: 1,
                gaps: 0,
                pre_chain: 0,
                outcome: SealOutcome::Stored {
                    session: session.to_string(),
                },
                previous,
                last_event_at: "2026-08-24T10:00:00Z".into(),
            };
            let id = Seal::id_of_text(&seal.to_text().unwrap());
            (id, seal, false)
        };

        // Lineare Kette: Epoche 1 (a) ← Epoche 2 (b).
        let (id_a, seal_a, s) = mk_seal(sid_a, 1, None);
        let (_, seal_b, s2) = mk_seal(sid_b, 2, Some(id_a.clone()));
        let mut sessions = BTreeMap::new();
        sessions.insert(sid_a, mk_session("erste"));
        sessions.insert(sid_b, mk_session("zweite"));
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid_a, vec![(id_a.clone(), seal_a.clone(), s)])
            .with_seals(
                sid_b,
                vec![(
                    Seal::id_of_text(&seal_b.to_text().unwrap()),
                    seal_b.clone(),
                    s2,
                )],
            );
        assert_eq!(index.epoch_position(sid_a), Some((1, 2)));
        assert_eq!(index.epoch_position(sid_b), Some((2, 2)));

        // Adversarial: previous zeigt ins Leere — kein Vorfahr, keine Panik.
        let dangling = ContentHash::from_bytes([9u8; 32]);
        let (id_c, seal_c, s3) = mk_seal(sid_a, 3, Some(dangling));
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid_a, vec![(id_c, seal_c, s3)]);
        assert_eq!(
            index.epoch_position(sid_a),
            None,
            "baumelnd = triviale Kette"
        );

        // Adversarial: 2-Zyklus A↔B — der eigene Seal wird nie mitgezaehlt,
        // der Walk terminiert.
        let (id_x, mut seal_x, _) = mk_seal(sid_a, 4, None);
        let (id_y, seal_y, _) = mk_seal(sid_b, 5, Some(id_x.clone()));
        seal_x.previous = Some(id_y.clone());
        // seal_x wurde nach dem Setzen von previous nicht neu gehasht —
        // fuer den Walk zaehlt nur die Struktur in all_seals.
        let index = Index::from_parts(sessions, BTreeMap::new())
            .with_seals(sid_a, vec![(id_x.clone(), seal_x, false)])
            .with_seals(sid_b, vec![(id_y.clone(), seal_y, false)]);
        let (k, n) = index
            .epoch_position(sid_a)
            .expect("Zyklus liefert etwas Endliches");
        assert!(k <= n && n <= 2, "Zyklus zaehlt sich nicht selbst: {k}/{n}");
    }

    #[test]
    fn invariant_legacy_stays_legacy() {
        // Invariante 7 (ADR-0011): Eine Session ohne Seal-Material ist
        // LEGACY — explizit, nicht ein leeres None. Und sie bekommt nie
        // nachträglich eine Chain angedichtet: Auch beim zweiten Laden
        // bleibt der Zustand derselbe.
        let sid: SessionId = format!("b3-{}", "e".repeat(64)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid, {
            let mut s = minds_core::Session::new(
                minds_core::Agent {
                    name: "claude-code".into(),
                    version: "1".into(),
                },
                minds_core::Model {
                    provider: "p".into(),
                    id: "m".into(),
                },
                minds_core::Intent {
                    request: "alt".into(),
                    ..Default::default()
                },
            );
            s.redaction.applied = true;
            s
        });
        let index = Index::from_parts(sessions, BTreeMap::new());
        assert!(
            index.evidence_state(sid).is_none(),
            "Legacy hat kein Verdikt"
        );
        assert!(index.epoch_position(sid).is_none());
    }

    #[test]
    fn evidence_state_judges_seals_like_verify_does() {
        let sid: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid, {
            let mut s = minds_core::Session::new(
                minds_core::Agent {
                    name: "claude-code".into(),
                    version: "1".into(),
                },
                minds_core::Model {
                    provider: "anthropic".into(),
                    id: "opus".into(),
                },
                minds_core::Intent {
                    request: "x".into(),
                    ..Default::default()
                },
            );
            s.redaction.applied = true;
            s
        });

        // Sauber versiegelt ⇒ VERIFIZIERT.
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid, vec![seal_for(sid, 0, None)]);
        let state = index.evidence_state(sid).expect("Seal-Material");
        assert_eq!(state.verdict, crate::model::EvidenceVerdict::Verified);
        assert!(state.chain_closed);

        // Mit versiegelter Luecke ⇒ UNVOLLSTAENDIG.
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid, vec![seal_for(sid, 2, None)]);
        let state = index.evidence_state(sid).unwrap();
        assert_eq!(state.verdict, crate::model::EvidenceVerdict::Incomplete);
        assert_eq!(state.gaps, 2);

        // Offene Epochenkette (previous zeigt ins Leere) ⇒ UNVOLLSTAENDIG.
        let dangling = ContentHash::from_bytes([1u8; 32]);
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid, vec![seal_for(sid, 0, Some(dangling))]);
        let state = index.evidence_state(sid).unwrap();
        assert_eq!(state.verdict, crate::model::EvidenceVerdict::Incomplete);
        assert!(!state.chain_closed);

        // Und ganz ohne Seals: kein Verdikt — vor Evidence-Chain erfasst.
        let index = Index::from_parts(sessions, BTreeMap::new());
        assert!(index.evidence_state(sid).is_none());
    }

    #[test]
    fn the_report_carries_epochs_scope_and_limitations() {
        let sid: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid, session("x"));

        let index = Index::from_parts(sessions, BTreeMap::new())
            .with_seals(sid, vec![seal_for(sid, 0, None)]);
        let report = index.evidence_report(sid).expect("Seal-Material");

        assert_eq!(
            report.state.verdict,
            crate::model::EvidenceVerdict::Verified
        );
        assert_eq!(
            report.scope.as_deref(),
            Some(minds_core::evidence::SCOPE_AGENT_HOOKS_V1)
        );
        assert_eq!(report.epochs.len(), 1);
        let epoch = &report.epochs[0];
        assert_eq!(epoch.link, crate::model::EpochLink::Start);
        assert!(epoch.stored);
        assert!(!epoch.signed);
        assert_eq!((epoch.first_seq, epoch.last_seq, epoch.events), (0, 3, 4));
        // Die Grenzen sind Teil des Reports — dasselbe Vokabular wie das
        // Audit-Bundle, nie eine eigene Liste.
        assert_eq!(report.limitations, minds_core::evidence::DOES_NOT_PROVE);
        assert!(report.sentence().contains("innerhalb der aufgezeichneten"));
        // Und das Verdikt ist DASSELBE Objekt wie evidence_state.
        assert_eq!(index.evidence_state(sid), Some(report.state));
    }

    #[test]
    fn the_report_classifies_how_each_epoch_links() {
        let sid: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid, session("x"));

        // Zwei Epochen: B haengt an A ⇒ [Start, Chained], Kette geschlossen.
        let (a_id, a_seal, a_signed) = seal_for(sid, 0, None);
        let b = seal_for(sid, 0, Some(a_id.clone()));
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid, vec![(a_id, a_seal, a_signed), b]);
        let report = index.evidence_report(sid).unwrap();
        assert_eq!(
            report.epochs.iter().map(|e| e.link).collect::<Vec<_>>(),
            vec![
                crate::model::EpochLink::Start,
                crate::model::EpochLink::Chained
            ]
        );
        assert!(report.state.chain_closed);

        // Ein baumelndes previous ⇒ Unresolved, unvollstaendig — und der
        // Leitsatz behauptet nichts.
        let dangling = ContentHash::from_bytes([1u8; 32]);
        let index = Index::from_parts(sessions.clone(), BTreeMap::new())
            .with_seals(sid, vec![seal_for(sid, 0, Some(dangling))]);
        let report = index.evidence_report(sid).unwrap();
        assert_eq!(report.epochs[0].link, crate::model::EpochLink::Unresolved);
        assert!(report.sentence().contains("unvollständig"));

        // Ein Block-Seal mit anderem Root als Vorgaenger ⇒ RejectedBefore.
        let other: SessionId = format!("b3-{}", "b".repeat(64)).parse().unwrap();
        let block = Seal {
            root: ContentHash::from_bytes([9u8; 32]),
            outcome: SealOutcome::Rejected,
            ..seal_for(other, 0, None).1
        };
        let block_id = Seal::id_of_text(&block.to_text().unwrap());
        let index = Index::from_parts(sessions, BTreeMap::new())
            .with_seals(other, vec![(block_id.clone(), block, false)])
            .with_seals(sid, vec![seal_for(sid, 0, Some(block_id))]);
        let report = index.evidence_report(sid).unwrap();
        assert_eq!(
            report.epochs[0].link,
            crate::model::EpochLink::RejectedBefore
        );
        assert!(report.state.rejected);
    }
    use minds_core::{Agent, Intent, Model};

    fn id(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn commit(hex: char) -> CommitId {
        hex.to_string().repeat(40).parse().unwrap()
    }

    fn session(request: &str) -> Session {
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.0.0".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: request.into(),
                ..Intent::default()
            },
        )
    }

    fn sample() -> Index {
        let mut sessions = BTreeMap::new();
        sessions.insert(id('a'), session("erste Absicht"));
        sessions.insert(id('b'), session("zweite Absicht"));

        let mut commits = BTreeMap::new();
        commits.insert(commit('1'), vec![id('a')]);
        commits.insert(commit('2'), vec![id('a'), id('b')]);

        Index::from_parts(sessions, commits)
    }

    #[test]
    fn resolves_a_session_by_id() {
        let index = sample();
        assert_eq!(
            index.session(id('a')).unwrap().intent.request,
            "erste Absicht"
        );
        assert!(index.session(id('f')).is_none());
    }

    #[test]
    fn a_commit_can_carry_several_sessions() {
        let index = sample();
        assert_eq!(index.sessions_of(commit('2')), &[id('a'), id('b')]);
        assert_eq!(index.sessions_of(commit('1')), &[id('a')]);
    }

    #[test]
    fn commits_of_is_the_reverse_direction() {
        let index = sample();
        // 'a' steckt in Commit '1' und '2', 'b' nur in '2'.
        assert_eq!(index.commits_of(id('a')), vec![commit('1'), commit('2')]);
        assert_eq!(index.commits_of(id('b')), vec![commit('2')]);
        assert!(index.commits_of(id('f')).is_empty());
    }

    #[test]
    fn a_commit_without_a_trailer_yields_nothing_not_a_panic() {
        assert!(sample().sessions_of(commit('9')).is_empty());
    }

    #[test]
    fn an_empty_index_is_the_empty_state() {
        let empty = Index::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.attributed_commits(), 0);
        assert_eq!(empty.unreadable(), 0);
    }

    #[test]
    fn sessions_come_out_in_id_order() {
        let index = sample();
        let ids: Vec<_> = index.sessions().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![id('a'), id('b')]);
    }

    #[test]
    fn every_edge_from_parts_is_observed_with_its_evidence() {
        let index = sample();
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(EvidenceMark::of(EvidenceSource::Observed))
        );
        assert_eq!(index.evidence_of(commit('1'), id('b')), None);
        assert_eq!(
            index.evidence_for_session(id('a')),
            Some(EvidenceMark::of(EvidenceSource::Observed))
        );
        // Ohne Kante: mit keinem Code verbunden, nicht „vermutet".
        let mut lonely = BTreeMap::new();
        lonely.insert(id('c'), session("dritte Absicht"));
        let index = Index::from_parts(lonely, BTreeMap::new());
        assert_eq!(index.evidence_for_session(id('c')), None);
    }

    #[test]
    fn a_better_proof_replaces_a_weaker_one_never_the_reverse() {
        let mut index = Index::default();
        index.sessions.insert(id('a'), session("x"));
        index.link(
            commit('1'),
            id('a'),
            EvidenceMark::of(EvidenceSource::Heuristic),
        );
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(EvidenceMark::of(EvidenceSource::Heuristic))
        );
        assert!(!index.is_observed(id('a')));
        index.link(
            commit('1'),
            id('a'),
            EvidenceMark::of(EvidenceSource::Observed),
        );
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(EvidenceMark::of(EvidenceSource::Observed))
        );
        assert!(index.is_observed(id('a')));
        index.link(
            commit('1'),
            id('a'),
            EvidenceMark::of(EvidenceSource::HumanDeclared),
        );
        assert_eq!(
            index.evidence_of(commit('1'), id('a')),
            Some(EvidenceMark::of(EvidenceSource::Observed))
        );
        // Die Kante steht nur einmal in beiden Richtungen.
        assert_eq!(index.sessions_of(commit('1')).len(), 1);
        assert_eq!(index.commits_of(id('a')).len(), 1);
    }

    #[test]
    fn changes_follow_the_commits_and_are_deduplicated() {
        let change: ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(commit('1'), change.clone());
        changes.insert(commit('2'), change.clone());
        let index = sample().with_changes(changes);
        assert_eq!(index.change_of(commit('1')), Some(&change));
        assert_eq!(index.change_of(commit('9')), None);
        // 'a' hängt an '1' und '2' — dieselbe Change-Id, einmal genannt.
        assert_eq!(index.changes_of(id('a')), vec![change.clone()]);
        assert_eq!(index.changes_of(id('b')), vec![change]);
        assert!(index.changes_of(id('f')).is_empty());
    }

    #[test]
    fn degraded_entries_keep_their_cause_and_count_as_unreadable() {
        let index = sample().with_degraded(vec![
            Degraded {
                id: id('d'),
                cause: Degradation::Forgotten {
                    reason: "DSGVO".into(),
                },
            },
            Degraded {
                id: id('e'),
                cause: Degradation::Corrupt,
            },
        ]);
        assert_eq!(index.unreadable(), 2);
        assert_eq!(index.degraded()[0].cause.word(), "vergessen");
        assert!(index.degraded()[0].cause.is_forgotten());
        assert_eq!(index.degraded()[1].cause.word(), "unlesbar");
        // Degradierte sind keine Sessions.
        assert_eq!(index.len(), 2);
        assert!(index.session(id('d')).is_none());
    }

    #[test]
    fn a_forgotten_store_error_carries_its_reason_sanitized() {
        let err = StoreError::Forgotten {
            id: id('a'),
            reason: "Kunde\u{1b}[2Kweg".into(),
        };
        let cause = Degradation::of(&err);
        assert_eq!(
            cause,
            Degradation::Forgotten {
                reason: "Kunde\\u{1b}[2Kweg".into()
            }
        );
        assert_eq!(
            Degradation::of(&StoreError::Unredacted { id: id('a') }),
            Degradation::Unredacted
        );
    }

    #[test]
    fn coverage_counts_linked_commits_over_all_commits() {
        let index = sample();
        let coverage = index.coverage();
        assert_eq!(coverage.commits_total, 2);
        assert_eq!(coverage.commits_with_context, 2);
        assert_eq!(Index::default().coverage().commits_total, 0);
    }

    #[test]
    fn a_symbolic_endpoint_resolves_over_agent_and_local_id() {
        let mut with_lineage = session("mit Herkunft");
        with_lineage.lineage = Some(minds_core::Lineage::new("sess-42"));
        let mut sessions = BTreeMap::new();
        sessions.insert(id('a'), with_lineage);
        sessions.insert(id('b'), session("ohne Herkunft"));
        let index = Index::from_parts(sessions, BTreeMap::new());
        assert_eq!(
            index.resolve_endpoint("claude-code", "sess-42"),
            Some(id('a'))
        );
        assert_eq!(index.resolve_endpoint("codex", "sess-42"), None);
        assert_eq!(index.resolve_endpoint("claude-code", "sess-43"), None);
    }

    #[test]
    fn an_unknown_commit_has_no_subject() {
        assert_eq!(sample().subject_of(commit('1')), None);
    }
}
