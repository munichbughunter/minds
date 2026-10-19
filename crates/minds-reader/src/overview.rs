//! Die Übersicht: die Historie des Repos als Commit-Graph — und an jedem
//! Commit, was Minds über ihn weiß (Sessions, Agent, Review, Seal, Intent).
//! Darüber die Sessions, die an keinem Commit hängen.
//!
//! Strikt lesend. Die Historie liest [`minds_git::Repo::graph`] (Kinder vor
//! Eltern, hash-geprüft, begrenzt); die Lanes legt [`layout`] — wie `git log
//! --graph`, eine Spalte je offenem Zweig. Alle Texte sind entschärft und
//! begrenzt: Commit-Nachrichten, Autoren, Branch- und Tag-Namen schreibt,
//! wer das Repo schreibt.

use std::collections::{BTreeMap, HashSet};

use minds_core::{ChangeId, SessionId};
use minds_git::{CommitId, GraphCommit, Repo};

use crate::model::{EvidenceVerdict, Verdict};
use crate::text::{sanitize, sanitize_path};

/// So viele Commits zeigt die Übersicht höchstens.
pub const MAX_COMMITS: usize = 500;

/// So viele Lanes zeichnet der Graph höchstens; weitere enden in `…`.
pub const MAX_LANES: usize = 10;

/// So viele Branches bzw. Tags liest die Übersicht höchstens.
pub const MAX_REFS: usize = 4096;

/// So viele Ref-Etiketten trägt ein Commit höchstens; der Rest als `+n`.
pub const MAX_LABELS: usize = 4;

/// So viele Sessions ohne Commit stehen oben höchstens (die neuesten).
pub const MAX_WIP: usize = 20;

/// So viele Branches, die einen Commit enthalten, nennt eine Zeile; der Rest
/// als Zahl.
pub const MAX_CONTAINING: usize = 3;

/// Über so viele Sessions eines Commits rechnet die Zeile Seal und Intent.
pub const MAX_SESSIONS_PER_ROW: usize = 64;

/// Die Übersicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    /// Sessions ohne Commit, neueste zuerst, höchstens [`MAX_WIP`].
    pub wip: Vec<WipSession>,
    /// Wie viele Sessions ohne Commit nicht gezeigt sind.
    pub wip_more: usize,
    /// Die Commits, Kinder vor Eltern.
    pub rows: Vec<CommitRow>,
    /// Wie viele Lanes der Graph breit ist (höchstens [`MAX_LANES`]).
    pub lanes: usize,
    /// Ob die Historie bei [`MAX_COMMITS`] gekappt wurde.
    pub truncated: bool,
    /// Ob das Lesebudget je Durchgang die Kappung auslöste.
    pub budget_hit: bool,
    /// Wie viele Branch-Spitzen bzw. Refs nicht in den Graphen kamen (kein
    /// lesbarer Commit, zu groß, über [`minds_git::MAX_TIPS`]) — nur solche,
    /// die auch sonst nicht im Graphen stehen.
    pub skipped_refs: usize,
    /// Ob es mehr als [`MAX_REFS`] Branches oder Tags gibt bzw. die Tags
    /// nicht lesbar waren — dann sind nicht alle angesehen.
    pub refs_capped: bool,
    /// Ob ein Commit fehlte oder nicht zu seiner Id hashte (ersetzt?) — der
    /// Graph ist dann an dieser Stelle offen.
    pub incomplete: bool,
}

/// Eine Session, die an keinem Commit hängt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WipSession {
    /// Die Session.
    pub id: SessionId,
    /// Der Agent, entschärft.
    pub agent: String,
    /// Der Auftrag, einzeilig, entschärft.
    pub request: String,
    /// Der Start, wie erfasst, entschärft.
    pub started: Option<String>,
    /// Was über ihr Ende bekannt ist — nur das beobachtete `SessionEnd`
    /// zählt (`closed`); `ended_at` ist bloß das letzte erfasste Event.
    pub state: WipState,
    /// Der Start als Epoch-Sekunden, für das Alter in Listen (C3.7) —
    /// aus `started`, nie aus der Uhr des Readers; `None` ohne Start.
    pub started_epoch: Option<i64>,
    /// Woher die Session stammt (K2) — aus dem Record abgeleitet, siehe
    /// [`Origin`].
    pub origin: Origin,
}

/// Woher eine Session ohne Commit stammt (K2). Eine Session im Store dieses
/// Repositories, die an keinem Commit hängt, ist entweder noch offen — oder
/// sie gehört gar nicht hierher: `minds enable` importiert im Hintergrund
/// die alten Transkripte unter `~/.claude/projects/<slug>/`, und der Slug ist
/// der Verzeichnispfad. Ein am selben Pfad neu angelegtes Repository erbt so
/// fremde Sessions. Der Reader liest die Herkunft aus dem, was der Record
/// trägt, und rät nicht:
///
/// - Ein Seal mit Scope `witness/v1` oder `witness-fs/v1`: ein Witness hat
///   die Session bezeugt — `witnessed`.
/// - Sonst ein Seal mit Scope `agent-hooks/v1`: die Hooks des Harness haben
///   sie erfasst — `captured`.
/// - Kein Seal-Material, aber ein gelesenes Transkript
///   ([`TranscriptStatus::Read`](minds_core::TranscriptStatus::Read)): So
///   baut nur `minds import` (der Import *ist* das Transkript; er versiegelt
///   kein Journal) — `imported`.
/// - Alles andere (kein Seal, kein oder anderer Transkript-Status): vor der
///   Evidence-Chain erfasst oder von einem Binary ohne das Feld — `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Aus dem Transkript-Store des Agenten importiert, nicht von den Hooks
    /// dieses Repositories erfasst.
    Imported,
    /// Von den Hooks des Harness erfasst und versiegelt.
    Captured,
    /// Von einem Witness bezeugt.
    Witnessed,
    /// Der Record sagt es nicht.
    Unknown,
}

impl Origin {
    /// Das Wort in Listen — Glyph kommt aus dem Theme, das Wort von hier.
    pub fn word(self) -> &'static str {
        match self {
            Origin::Imported => "imported",
            Origin::Captured => "captured",
            Origin::Witnessed => "witnessed",
            Origin::Unknown => "origin unknown",
        }
    }

    /// Der Satz fürs Detail: woher, und was das bedeutet.
    pub fn sentence(self) -> &'static str {
        match self {
            Origin::Imported => {
                "not from this repository's hooks — imported from the agent's transcript store"
            }
            Origin::Captured => "captured by the agent's hooks in this repository",
            Origin::Witnessed => "witnessed by a separate witness in this repository",
            Origin::Unknown => "the record does not say where it came from",
        }
    }
}

/// Der Befehl, der eine Session aus dem Store dieses Repositories entfernt —
/// **ein** Wortlaut für TUI und CLI. Der Reader führt ihn nicht aus.
pub fn forget_command(id: &SessionId) -> String {
    format!("minds forget {id}")
}

/// Was über das Ende einer Session ohne Commit bekannt ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WipState {
    /// Kein `SessionEnd` gesehen — sie kann noch laufen.
    Open,
    /// `SessionEnd` gesehen.
    Ended,
    /// Keine Herkunftsdaten (Altbestand, Adapter ohne Lineage).
    Unknown,
}

/// Ein Ref, der auf einen Commit zeigt. Namen entschärft und gekürzt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefLabel {
    /// Der ausgecheckte Branch (HEAD).
    Head(String),
    /// Ein anderer lokaler Branch.
    Branch(String),
    /// Ein Tag.
    Tag(String),
    /// HEAD ohne Branch.
    Detached,
    /// So viele weitere Refs.
    More(usize),
}

/// Wie die Sessions eines Commits versiegelt sind — der Reader-Befund, ohne
/// Signaturprüfung (die macht `minds verify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealMark {
    /// Alle Sessions: Integrität intakt, Coverage vollständig, jedes Seal
    /// trägt eine Signatur (ungeprüft — das tut Verify).
    Sealed,
    /// Intakt und vollständig, aber mindestens ein Seal ohne Signatur — wer
    /// `refs/minds/` schreibt, kann solches Material erzeugen.
    Unsigned,
    /// Mehr Sessions, als die Zeile ansieht ([`MAX_SESSIONS_PER_ROW`]); die
    /// angesehenen sind intakt — über den Rest sagt sie nichts.
    Partial,
    /// Alle mit Seal, mindestens eine mit Lücken.
    Incomplete,
    /// Mindestens eine Session ohne Seal.
    Unsealed,
    /// Seal-Material verändert.
    Tampered,
}

/// Eine Zelle des Graphen: ein Zeichen und die Lane, deren Farbe es trägt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// `●`, `│`, `╮`, `╯`, `╭`, `╰`, `├`, `┤`, `─`, `┼`, ` ` oder `…`.
    pub glyph: char,
    /// Die Lane (Farbe).
    pub lane: usize,
    /// Ob zwischen dieser und der nächsten Zelle eine waagrechte Linie läuft.
    pub joint: bool,
}

/// Ein Commit in der Übersicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    /// Der Commit.
    pub id: CommitId,
    /// Die Eltern.
    pub parents: Vec<CommitId>,
    /// Der Betreff, entschärft.
    pub subject: String,
    /// Der Autor, entschärft.
    pub author: String,
    /// Die Autor-Zeit (Sekunden seit Epoch).
    pub time: i64,
    /// Die Autor-Zeit zur Anzeige, `JJJJ-MM-TT HH:MMZ` (UTC).
    pub date: String,
    /// Die Refs auf diesem Commit, höchstens [`MAX_LABELS`] und `More`.
    pub refs: Vec<RefLabel>,
    /// Die Sessions an diesem Commit (Trailer, sonst Store-Index).
    pub sessions: Vec<SessionId>,
    /// Der Agent der ersten Session, entschärft.
    pub agent: Option<String>,
    /// Der Auftrag der ersten Session, einzeilig, entschärft.
    pub request: Option<String>,
    /// Die Change-Id.
    pub change: Option<ChangeId>,
    /// Das Review-Urteil.
    pub review: Verdict,
    /// Wie die Sessions versiegelt sind — `None` ohne Session.
    pub seal: Option<SealMark>,
    /// Ob eine Session des Commits einen Intent-Anker nennt (Record).
    pub intent: bool,
    /// Die Anker-Ids, die seine Sessions nennen — je Anker einmal, in der
    /// Reihenfolge der Sessions (C3.4). Genannt, nicht geprüft.
    pub anchors: Vec<minds_core::ContentHash>,
    /// Die Branches, die den Commit enthalten — aus dem gelesenen Graphen,
    /// HEAD zuerst, höchstens [`MAX_CONTAINING`] (C3.4).
    pub branches: Vec<String>,
    /// Wie viele weitere Branches ihn enthalten.
    pub branches_more: usize,
    /// Die Zeile des Graphen.
    pub graph: Vec<Cell>,
}

/// Die Zahlen der Übersicht (C3.1) — gezählt über genau die gelesenen
/// Zeilen ([`Overview::rows`], [`Overview::wip`]), nie über eine andere
/// Bezugsmenge. Die Teile ergeben immer das Ganze.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverviewCounts {
    /// Die gelesenen Commits.
    pub commits: usize,
    /// Davon Commits mit einer Agent-Session ([`CommitRow::has_agent_session`]).
    pub with_agent_session: usize,
    /// Davon Commits ohne Agent-Session — der Rest.
    pub without_agent_session: usize,
    /// Sessions, die an keinem Commit hängen — gezeigte und weitere.
    pub sessions_without_commit: usize,
    /// Ob die Historie gekappt oder lückenhaft gelesen wurde — die Zahlen
    /// zählen dann ausdrücklich nur das Gelesene.
    pub limited: bool,
}

impl Overview {
    /// Die Zahlen über die gelesenen Zeilen.
    pub fn counts(&self) -> OverviewCounts {
        let commits = self.rows.len();
        let with_agent_session = self
            .rows
            .iter()
            .filter(|row| row.has_agent_session())
            .count();
        OverviewCounts {
            commits,
            with_agent_session,
            without_agent_session: commits - with_agent_session,
            sessions_without_commit: self.wip.len() + self.wip_more,
            limited: self.truncated || self.budget_hit || self.incomplete,
        }
    }
}

impl CommitRow {
    /// Ob an diesem Commit eine lesbare Agent-Session hängt — dieselbe
    /// Frage für Badge, Zahl und Filter (C3.1, C3.3).
    pub fn has_agent_session(&self) -> bool {
        self.agent.is_some()
    }
}

/// Ein Text einzeilig und begrenzt — vor dem Entschärfen gekürzt.
pub fn one_line(text: &str, limit: usize) -> String {
    let head: String = text.chars().take(limit.saturating_mul(4)).collect();
    let flat = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = flat.chars().take(limit).collect();
    sanitize(&clipped)
}

/// Ein Ref-Name, entschärft und gekürzt.
fn ref_name(name: &str) -> String {
    let clipped: String = name.chars().take(40).collect();
    let shown = sanitize_path(&clipped);
    if name.chars().count() > 40 {
        format!("{shown}…")
    } else {
        shown
    }
}

impl crate::Inspection {
    /// Die Übersicht: die Historie ab HEAD und den lokalen Branches,
    /// höchstens [`MAX_COMMITS`] Commits.
    pub fn overview(&self, repo: &Repo) -> minds_git::Result<Overview> {
        self.overview_with(repo, MAX_COMMITS)
    }

    /// [`Inspection::overview`] mit ausdrücklicher Grenze.
    pub fn overview_with(&self, repo: &Repo, limit: usize) -> minds_git::Result<Overview> {
        self.overview_limits(repo, limit, MAX_REFS)
    }

    /// [`Inspection::overview`] mit ausdrücklichen Grenzen für Commits und
    /// Refs je Namensraum.
    pub fn overview_limits(
        &self,
        repo: &Repo,
        limit: usize,
        max_refs: usize,
    ) -> minds_git::Result<Overview> {
        let index = self.index();
        // Ein begrenzter Lesezugriff für alles hier — HEAD, Refs, Historie.
        let reader = repo.bounded_reader()?;
        let head = reader.head();
        let (head_branch, head_commit) = (head.branch.clone(), head.commit);
        let heads = reader.refs_under_max("refs/heads/", max_refs)?;
        // Ein Fehler beim Lesen der Tags kostet nur die Tags — und wird
        // gesagt, nicht verschwiegen.
        let (tags, tags_failed) = match reader.refs_under_max("refs/tags/", max_refs) {
            Ok(tags) => (tags, false),
            Err(_) => (minds_git::RefsUnder::default(), true),
        };
        let branches = &heads.refs;
        // HEAD zuerst — es kommt immer in den Graphen.
        let mut tips: Vec<CommitId> = head_commit.into_iter().collect();
        tips.extend(branches.iter().map(|(_, id)| *id));
        let graph = reader.graph(&tips, limit);
        let shown: HashSet<CommitId> = graph.commits.iter().map(|c| c.id).collect();

        // Etiketten nur für gezeigte Commits.
        let mut labels: BTreeMap<CommitId, Vec<RefLabel>> = BTreeMap::new();
        let current = head_branch;
        // HEAD aus HEAD selbst — nicht aus der (begrenzten) Branch-Liste.
        if let Some(commit) = head_commit.filter(|c| shown.contains(c)) {
            let label = match &current {
                Some(name) => RefLabel::Head(ref_name(name)),
                None => RefLabel::Detached,
            };
            labels.entry(commit).or_default().push(label);
        }
        for (name, id) in branches {
            let short = name.strip_prefix("refs/heads/").unwrap_or(name);
            if !shown.contains(id) || current.as_deref() == Some(short) {
                continue;
            }
            labels
                .entry(*id)
                .or_default()
                .push(RefLabel::Branch(ref_name(short)));
        }
        for (name, id) in &tags.refs {
            if !shown.contains(id) {
                continue;
            }
            let short = name.strip_prefix("refs/tags/").unwrap_or(name);
            labels
                .entry(*id)
                .or_default()
                .push(RefLabel::Tag(ref_name(short)));
        }
        for list in labels.values_mut() {
            // HEAD zuerst, dann Branches, dann Tags; höchstens MAX_LABELS.
            list.sort_by_key(|label| match label {
                RefLabel::Head(_) | RefLabel::Detached => 0,
                RefLabel::Branch(_) => 1,
                RefLabel::Tag(_) | RefLabel::More(_) => 2,
            });
            if list.len() > MAX_LABELS {
                let more = list.len() - MAX_LABELS;
                list.truncate(MAX_LABELS);
                list.push(RefLabel::More(more));
            }
        }

        // Welche Branches welchen Commit enthalten — über den gelesenen
        // Graphen (Kinder vor Eltern), ohne weiteren Git-Zugriff: Jede Spitze
        // setzt ihr Bit, jeder Commit reicht seine Bits an die Eltern weiter.
        let mut names: Vec<String> = Vec::new();
        let mut tip_bits: Vec<(CommitId, usize)> = Vec::new();
        if let (Some(name), Some(commit)) = (&current, head_commit) {
            names.push(ref_name(name));
            tip_bits.push((commit, 0));
        }
        for (name, id) in branches {
            let short = name.strip_prefix("refs/heads/").unwrap_or(name);
            if current.as_deref() == Some(short) {
                continue;
            }
            names.push(ref_name(short));
            tip_bits.push((*id, names.len() - 1));
        }
        let words = names.len().div_ceil(64);
        let position: std::collections::HashMap<CommitId, usize> = graph
            .commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        let mut bits = vec![vec![0u64; words]; graph.commits.len()];
        for (tip, bit) in tip_bits {
            if let Some(&i) = position.get(&tip) {
                bits[i][bit / 64] |= 1 << (bit % 64);
            }
        }
        for i in 0..graph.commits.len() {
            let mine = bits[i].clone();
            for parent in &graph.commits[i].parents {
                if let Some(&j) = position.get(parent).filter(|&&j| j > i) {
                    for (word, set) in bits[j].iter_mut().zip(&mine) {
                        *word |= set;
                    }
                }
            }
        }
        let mut containing = bits.into_iter().map(|set| {
            let all: Vec<usize> = (0..names.len())
                .filter(|b| set[b / 64] & (1 << (b % 64)) != 0)
                .collect();
            let shown: Vec<String> = all
                .iter()
                .take(MAX_CONTAINING)
                .map(|b| names[*b].clone())
                .collect();
            (shown, all.len().saturating_sub(MAX_CONTAINING))
        });

        let (graphs, lanes) = layout(&graph.commits);
        let rows = graph
            .commits
            .into_iter()
            .zip(graphs)
            .map(|(commit, cells)| {
                let sessions: Vec<SessionId> = index.sessions_of(commit.id).to_vec();
                // Seal und Intent über höchstens so viele Sessions — ein
                // Commit kann beliebig viele tragen.
                let considered = &sessions[..sessions.len().min(MAX_SESSIONS_PER_ROW)];
                let first = sessions.first().and_then(|id| index.session(*id));
                let seal = (!sessions.is_empty()).then(|| {
                    let states: Vec<_> = considered
                        .iter()
                        .map(|id| index.evidence_state(*id))
                        .collect();
                    let verdict = |v| {
                        states
                            .iter()
                            .any(|s| s.as_ref().is_some_and(|s| s.verdict == v))
                    };
                    if verdict(EvidenceVerdict::Tampered) {
                        SealMark::Tampered
                    } else if states.iter().any(Option::is_none) {
                        SealMark::Unsealed
                    } else if verdict(EvidenceVerdict::Incomplete) {
                        SealMark::Incomplete
                    } else if sessions.len() > considered.len() {
                        SealMark::Partial
                    } else if states.iter().flatten().any(|s| s.signed < s.seals) {
                        SealMark::Unsigned
                    } else {
                        SealMark::Sealed
                    }
                });
                let mut anchors: Vec<minds_core::ContentHash> = Vec::new();
                for id in considered {
                    if let Some(anchor) = index
                        .session(*id)
                        .and_then(crate::intent::bound_anchor)
                        .map(|a| a.id)
                        && !anchors.contains(&anchor)
                    {
                        anchors.push(anchor);
                    }
                }
                let intent = !anchors.is_empty();
                let (branches, branches_more) = containing.next().unwrap_or_default();
                CommitRow {
                    refs: labels.remove(&commit.id).unwrap_or_default(),
                    subject: one_line(&commit.summary, 200),
                    author: one_line(&commit.author, 60),
                    time: commit.time,
                    date: jiff::Timestamp::from_second(commit.time)
                        .map(|t| t.strftime("%Y-%m-%d %H:%MZ").to_string())
                        .unwrap_or_default(),
                    agent: first.map(|s| one_line(&s.agent.name, 40)),
                    request: first.map(|s| one_line(&s.intent.request, 200)),
                    change: index.change_of(commit.id).cloned(),
                    review: self.review_state_of_commit(commit.id).verdict,
                    parents: commit.parents,
                    seal,
                    intent,
                    anchors,
                    branches,
                    branches_more,
                    sessions,
                    graph: cells,
                    id: commit.id,
                }
            })
            .collect();

        // Sessions ohne Commit, nach erfasstem Start (geparst), neueste
        // zuerst; ohne Start zuletzt. Erst wählen, dann aufbereiten.
        let mut open: Vec<(Option<jiff::Timestamp>, SessionId)> = index
            .sessions()
            .filter(|(id, _)| index.commits_of(**id).is_empty())
            .map(|(id, session)| {
                let at = session
                    .lineage
                    .as_ref()
                    .and_then(|l| l.started_at.as_deref())
                    // Ein RFC-3339-Zeitpunkt ist nie länger als 64 Bytes.
                    .filter(|s| s.len() <= 64)
                    .and_then(|s| s.parse::<jiff::Timestamp>().ok());
                (at, *id)
            })
            .collect();
        open.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.cmp(&x).then(a.1.cmp(&b.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.cmp(&b.1),
        });
        let wip_more = open.len().saturating_sub(MAX_WIP);
        let wip: Vec<WipSession> = open
            .into_iter()
            .take(MAX_WIP)
            .filter_map(|(_, id)| {
                let session = index.session(id)?;
                let lineage = session.lineage.as_ref();
                Some(WipSession {
                    id,
                    agent: one_line(&session.agent.name, 40),
                    request: one_line(&session.intent.request, 200),
                    started: lineage
                        .and_then(|l| l.started_at.as_deref())
                        .map(|s| one_line(s, 40)),
                    state: match lineage {
                        None => WipState::Unknown,
                        Some(l) if l.closed => WipState::Ended,
                        Some(_) => WipState::Open,
                    },
                    started_epoch: lineage
                        .and_then(|l| l.started_at.as_deref())
                        .and_then(minds_metrics::epoch_seconds),
                    origin: origin_of(index, id, lineage),
                })
            })
            .collect();
        // Nicht gefolgte Spitzen, die auch sonst nicht im Graphen stehen —
        // gezählt als Refs (Branches, dazu ein HEAD ohne Branch).
        let lost: HashSet<CommitId> = graph
            .skipped
            .iter()
            .filter(|t| !shown.contains(t))
            .copied()
            .collect();
        let skipped = branches.iter().filter(|(_, id)| lost.contains(id)).count()
            + usize::from(
                current.is_none()
                    && (head.refused || head_commit.is_some_and(|c| lost.contains(&c))),
            );
        Ok(Overview {
            wip,
            wip_more,
            rows,
            lanes,
            truncated: graph.truncated,
            budget_hit: graph.budget_hit,
            skipped_refs: skipped + heads.skipped + tags.skipped,
            refs_capped: heads.more || tags.more || tags_failed,
            incomplete: graph.incomplete,
        })
    }
}

/// Die Lanes des Graphen, Zeile für Zeile — wie `git log --graph`: Jeder
/// offene Zweig hat eine Spalte; ein Commit sitzt in der Spalte, die ihn
/// erwartet, Merges öffnen eine Spalte für den weiteren Elter, Zweige, die
/// in einem Commit zusammenlaufen, schließen ihre. Höchstens [`MAX_LANES`]
/// Zellen je Zeile; der Knoten bleibt immer sichtbar.
pub fn layout(commits: &[GraphCommit]) -> (Vec<Vec<Cell>>, usize) {
    let mut active: Vec<Option<CommitId>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    let mut width = 0usize;
    for commit in commits {
        let col = match active.iter().position(|a| *a == Some(commit.id)) {
            Some(col) => col,
            None => match active.iter().position(Option::is_none) {
                Some(free) => {
                    active[free] = Some(commit.id);
                    free
                }
                None => {
                    active.push(Some(commit.id));
                    active.len() - 1
                }
            },
        };
        // Weitere Lanes, die diesen Commit erwarten, laufen hier zusammen.
        let joining: Vec<usize> = active
            .iter()
            .enumerate()
            .filter(|(i, a)| *i != col && **a == Some(commit.id))
            .map(|(i, _)| i)
            .collect();
        let before = active.clone();
        for j in &joining {
            active[*j] = None;
        }
        active[col] = commit.parents.first().copied();
        // Weitere Eltern: eine bestehende Lane oder eine neue — nie eine,
        // die in dieser Zeile gerade zusammenlief (sonst verschwände die
        // Merge-Kante unter einem `╯`).
        let mut forks = Vec::new();
        for parent in commit.parents.iter().skip(1) {
            let at = match active.iter().position(|a| *a == Some(*parent)) {
                Some(at) => at,
                None => match active
                    .iter()
                    .enumerate()
                    .position(|(i, a)| a.is_none() && i != col && !joining.contains(&i))
                {
                    Some(free) => {
                        active[free] = Some(*parent);
                        free
                    }
                    None => {
                        active.push(Some(*parent));
                        active.len() - 1
                    }
                },
            };
            forks.push(at);
        }
        let span = before.len().max(active.len());
        let targets: Vec<usize> = joining.iter().chain(forks.iter()).copied().collect();
        let lo = targets.iter().copied().chain([col]).min().unwrap_or(col);
        let hi = targets.iter().copied().chain([col]).max().unwrap_or(col);
        let mut cells = Vec::with_capacity(span.min(MAX_LANES));
        for i in 0..span.min(MAX_LANES) {
            let passing = before.get(i).is_some_and(Option::is_some);
            let glyph = if i == col {
                '●'
            } else if joining.contains(&i) {
                if i > col { '╯' } else { '╰' }
            } else if forks.contains(&i) && !passing {
                if i > col { '╮' } else { '╭' }
            } else if forks.contains(&i) {
                if i > col { '┤' } else { '├' }
            } else if i > lo && i < hi {
                if passing { '┼' } else { '─' }
            } else if passing {
                '│'
            } else {
                ' '
            };
            cells.push(Cell {
                glyph,
                lane: i,
                joint: i >= lo && i < hi,
            });
        }
        if span > MAX_LANES
            && let Some(last) = cells.last_mut()
        {
            // Der Knoten jenseits der Grenze bleibt sichtbar, sonst `…`.
            if col >= MAX_LANES - 1 {
                *last = Cell {
                    glyph: '●',
                    lane: col,
                    joint: false,
                };
            } else {
                last.glyph = '…';
                last.joint = false;
            }
        }
        // Leere Lanes am Ende fallen weg.
        while active.last().is_some_and(Option::is_none) {
            active.pop();
        }
        width = width.max(cells.len());
        rows.push(cells);
    }
    (rows, width)
}

/// Die Herkunft einer Session aus ihrem Seal-Material und ihrer Lineage —
/// die Regel steht an [`Origin`].
fn origin_of(index: &crate::Index, id: SessionId, lineage: Option<&minds_core::Lineage>) -> Origin {
    use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1};
    match index.evidence_report(id) {
        Some(report) => {
            let has = |scope: &str| report.epochs.iter().any(|e| e.scope == scope);
            if has(SCOPE_WITNESS_V1) || has(SCOPE_WITNESS_FS_V1) {
                Origin::Witnessed
            } else if has(SCOPE_AGENT_HOOKS_V1) {
                Origin::Captured
            } else {
                Origin::Unknown
            }
        }
        None => match lineage {
            Some(l) if l.transcript == Some(minds_core::TranscriptStatus::Read) => Origin::Imported,
            _ => Origin::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> CommitId {
        format!("{:040x}", n).parse().unwrap()
    }

    fn commit(n: u8, parents: &[u8]) -> GraphCommit {
        GraphCommit {
            id: id(n),
            parents: parents.iter().map(|p| id(*p)).collect(),
            author: "A".into(),
            time: 0,
            summary: format!("c{n}"),
        }
    }

    fn glyphs(rows: &[Vec<Cell>]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|c| c.glyph)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn a_linear_history_is_one_lane() {
        let (rows, width) = layout(&[commit(3, &[2]), commit(2, &[1]), commit(1, &[])]);
        assert_eq!(glyphs(&rows), ["●", "●", "●"]);
        assert_eq!(width, 1);
    }

    /// Ein Merge öffnet eine Lane für den zweiten Elter; wo der Zweig vom
    /// Hauptstrang abging, läuft sie wieder zusammen.
    #[test]
    fn a_merge_opens_and_closes_a_lane() {
        // 5 = merge(4, 3); 4 und 3 haben beide 2; 2 hat 1.
        let (rows, width) = layout(&[
            commit(5, &[4, 3]),
            commit(4, &[2]),
            commit(3, &[2]),
            commit(2, &[1]),
            commit(1, &[]),
        ]);
        assert_eq!(glyphs(&rows), ["●╮", "●│", "│●", "●╯", "●"]);
        assert_eq!(width, 2);
        assert!(rows[0][0].joint, "the merge line runs right");
        assert!(rows[3][0].joint, "the join line runs right");
    }

    /// GitHub-Flow: Ein Branch geht von einem Merge ab — die Merge-Kante
    /// bleibt sichtbar (`╮`), statt unter dem `╯` des Branches zu
    /// verschwinden.
    #[test]
    fn a_branch_off_a_merge_keeps_the_merge_edge() {
        let (rows, _) = layout(&[
            commit(9, &[5]),
            commit(8, &[5]),
            commit(5, &[3, 4]),
            commit(4, &[2]),
            commit(3, &[2]),
            commit(2, &[]),
        ]);
        assert_eq!(
            glyphs(&rows),
            ["●", "│●", "●╯╮", "│ ●", "● │", "●─╯"],
            "the second parent's lane opens beside the closing branch"
        );
    }

    /// Ein Merge in eine Lane links vom Commit zeigt nach links (`├`).
    #[test]
    fn a_fork_into_an_existing_lane_on_the_left_points_left() {
        // 9 → 4, 8 → 7, 7 = merge(3, 4): die Lane von 4 liegt links von 7.
        let (rows, _) = layout(&[
            commit(9, &[4]),
            commit(8, &[7]),
            commit(7, &[3, 4]),
            commit(4, &[1]),
            commit(3, &[1]),
            commit(1, &[]),
        ]);
        assert!(
            rows[2].iter().any(|c| c.glyph == '├'),
            "{:?}",
            glyphs(&rows)
        );
    }

    #[test]
    fn two_tips_get_two_lanes() {
        let (rows, _) = layout(&[commit(3, &[1]), commit(2, &[1]), commit(1, &[])]);
        assert_eq!(glyphs(&rows), ["●", "│●", "●╯"]);
    }

    /// Zu viele Lanes enden in `…` — aber jede Zeile behält ihren Knoten.
    #[test]
    fn too_many_lanes_keep_every_node() {
        let mut commits: Vec<GraphCommit> = (10..30).map(|n| commit(n, &[1])).collect();
        commits.push(commit(1, &[]));
        let (rows, width) = layout(&commits);
        assert_eq!(width, MAX_LANES);
        for row in &rows {
            assert!(row.len() <= MAX_LANES);
            assert_eq!(row.iter().filter(|c| c.glyph == '●').count(), 1, "{row:?}");
        }
    }

    /// Ein Octopus mit vielen Eltern bleibt begrenzt.
    #[test]
    fn a_wide_octopus_stays_bounded() {
        let parents: Vec<u8> = (2..18).collect();
        let mut commits = vec![commit(1, &parents)];
        commits.extend(parents.iter().map(|p| commit(*p, &[])));
        let (rows, width) = layout(&commits);
        assert!(width <= MAX_LANES);
        assert!(rows.iter().all(|r| r.len() <= MAX_LANES));
    }

    /// Der Beweis für `overview_limits`: HEAD trägt sein Etikett, auch wenn
    /// sein Branch hinter der Ref-Grenze liegt.
    #[test]
    fn the_head_label_survives_the_ref_cap() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=A", "-c", "user.email=a@x"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "zzz"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["branch", "aaa"]);
        git(&["branch", "bbb"]);
        let repo = Repo::open(dir.path()).unwrap();
        let inspection = crate::Inspection::from_index(
            crate::Index::from_parts(Default::default(), Default::default()),
            Vec::new(),
            "t",
        );
        let overview = inspection.overview_limits(&repo, 10, 1).unwrap();
        assert!(overview.refs_capped);
        assert!(
            overview.rows[0]
                .refs
                .iter()
                .any(|r| matches!(r, RefLabel::Head(name) if name == "zzz")),
            "{:?}",
            overview.rows[0].refs
        );
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=A", "-c", "user.email=a@x"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn agent_session(request: &str) -> minds_core::Session {
        use minds_core::{Agent, Intent, Model, Session};
        Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "t".into(),
                id: "t".into(),
            },
            Intent {
                request: request.into(),
                ..Intent::default()
            },
        )
    }

    /// K2: Eine Session ohne Commit sagt, woher sie stammt und wie alt sie
    /// ist — aus dem Record, nie geraten. Ein Hook-Seal heißt `captured`,
    /// ein Witness-Seal `witnessed`; ohne Seal-Material baut nur der Import
    /// eine Session mit gelesenem Transkript (`imported`); alles andere
    /// bleibt `unknown`. Der `minds forget`-Befehl kommt fertig mit.
    #[test]
    fn a_session_without_a_commit_names_its_origin_and_age() {
        use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome};
        use minds_core::{Lineage, TranscriptStatus};

        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "one"]);
        let repo = Repo::open(dir.path()).unwrap();

        let with_lineage = |request: &str, transcript: Option<TranscriptStatus>| {
            let mut s = agent_session(request);
            let mut l = Lineage::new("local");
            l.started_at = Some("2026-07-20T10:00:00Z".into());
            l.transcript = transcript;
            s.lineage = Some(l);
            s
        };
        let seal = |id: SessionId, scope: &str| {
            let seal = Seal {
                root: minds_core::ContentHash::from_bytes([7u8; 32]),
                agent: "claude-code".into(),
                scope: scope.into(),
                first_seq: 0,
                last_seq: 3,
                events: 4,
                gaps: 0,
                pre_chain: 0,
                outcome: SealOutcome::Stored {
                    session: id.to_string(),
                },
                previous: None,
                last_event_at: "2026-07-20T10:05:00Z".into(),
            };
            let seal_id = Seal::id_of_text(&seal.to_text().unwrap());
            (seal_id, seal, true)
        };

        let imported = with_lineage("imported", Some(TranscriptStatus::Read));
        let captured = with_lineage("captured", Some(TranscriptStatus::Read));
        let witnessed = with_lineage("witnessed", Some(TranscriptStatus::NotReadByWitness));
        let legacy = agent_session("legacy");
        let ids: Vec<SessionId> = [&imported, &captured, &witnessed, &legacy]
            .iter()
            .map(|s| SessionId::of(s).unwrap())
            .collect();
        let mut sessions = BTreeMap::new();
        for (id, s) in ids.iter().zip([imported, captured, witnessed, legacy]) {
            sessions.insert(*id, s);
        }
        let index = crate::Index::from_parts(sessions, BTreeMap::new())
            .with_seals(ids[1], vec![seal(ids[1], SCOPE_AGENT_HOOKS_V1)])
            .with_seals(ids[2], vec![seal(ids[2], SCOPE_WITNESS_V1)]);
        let inspection = crate::Inspection::from_index(index, Vec::new(), "t");
        let overview = inspection.overview(&repo).unwrap();
        let wip = |id: SessionId| overview.wip.iter().find(|w| w.id == id).unwrap();

        assert_eq!(wip(ids[0]).origin, Origin::Imported);
        assert_eq!(wip(ids[1]).origin, Origin::Captured);
        assert_eq!(wip(ids[2]).origin, Origin::Witnessed);
        assert_eq!(wip(ids[3]).origin, Origin::Unknown);
        // Das Alter als Epoch-Sekunden aus dem erfassten Start — nicht aus
        // der Uhr des Readers; ohne Start keins.
        assert_eq!(
            wip(ids[0]).started_epoch,
            minds_metrics::epoch_seconds("2026-07-20T10:00:00Z")
        );
        assert_eq!(wip(ids[3]).started_epoch, None);
        // Ein Wort je Herkunft, und der Befehl, der sie entfernt.
        assert_eq!(Origin::Imported.word(), "imported");
        assert_eq!(Origin::Unknown.word(), "origin unknown");
        assert_eq!(forget_command(&ids[0]), format!("minds forget {}", ids[0]));
    }

    /// C3.1: Die Zahlen der Übersicht kommen aus genau den gelesenen Zeilen
    /// — „with an agent session" ist die Zahl der Zeilen mit Agent-Badge,
    /// über 0, 1 und n Agent-Commits, zwei Branches und Sessions ohne
    /// Commit; die Teile ergeben immer das Ganze.
    #[test]
    fn the_counts_are_the_rows_read() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        let mut ids: Vec<CommitId> = Vec::new();
        for m in ["one", "two", "three"] {
            git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", m]);
            ids.push(git_in(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap());
        }
        git_in(dir.path(), &["switch", "-q", "-c", "feat", "HEAD~2"]);
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "side"]);
        ids.push(git_in(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap());
        let repo = Repo::open(dir.path()).unwrap();
        for agent_commits in [0usize, 1, 3] {
            let mut sessions = BTreeMap::new();
            let mut links = BTreeMap::new();
            for (n, commit) in ids.iter().enumerate().take(agent_commits) {
                let s = agent_session(&format!("task {n}"));
                let id = SessionId::of(&s).unwrap();
                sessions.insert(id, s);
                links.insert(*commit, vec![id]);
            }
            // Zwei Sessions ohne Commit.
            for n in 0..2 {
                let s = agent_session(&format!("wip {n}"));
                sessions.insert(SessionId::of(&s).unwrap(), s);
            }
            let inspection = crate::Inspection::from_index(
                crate::Index::from_parts(sessions, links),
                Vec::new(),
                "t",
            );
            let overview = inspection.overview(&repo).unwrap();
            let counts = overview.counts();
            assert_eq!(counts.commits, overview.rows.len());
            assert_eq!(counts.commits, 4, "both branches are read");
            let badged = overview
                .rows
                .iter()
                .filter(|r| r.has_agent_session())
                .count();
            assert_eq!(counts.with_agent_session, badged);
            assert_eq!(counts.with_agent_session, agent_commits);
            assert_eq!(
                counts.with_agent_session + counts.without_agent_session,
                counts.commits
            );
            assert_eq!(counts.sessions_without_commit, 2);
            assert!(!counts.limited);
        }
    }

    /// C3.1: Eine gekappte Liste sagt, dass die Zahlen nur das Gelesene
    /// zählen.
    #[test]
    fn capped_counts_say_they_count_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        for m in ["one", "two", "three"] {
            git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", m]);
        }
        let repo = Repo::open(dir.path()).unwrap();
        let inspection = crate::Inspection::from_index(
            crate::Index::from_parts(Default::default(), Default::default()),
            Vec::new(),
            "t",
        );
        let counts = inspection.overview_with(&repo, 2).unwrap().counts();
        assert_eq!(counts.commits, 2);
        assert!(counts.limited);
    }

    /// C3.4: Jeder Commit kennt die Branches, die ihn enthalten — aus dem
    /// schon gelesenen Graphen, HEAD zuerst, höchstens drei und der Rest als
    /// Zahl — und die Anker-Ids, die seine Sessions nennen.
    #[test]
    fn rows_know_their_branches_and_anchors() {
        let dir = tempfile::tempdir().unwrap();
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base: CommitId = git_in(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "top"]);
        for b in ["b1", "b2", "b3", "b4"] {
            git_in(dir.path(), &["branch", b, &base.to_string()]);
        }
        git_in(
            dir.path(),
            &["switch", "-q", "-c", "feat", &base.to_string()],
        );
        git_in(dir.path(), &["commit", "-q", "--allow-empty", "-m", "side"]);
        let side: CommitId = git_in(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
        git_in(dir.path(), &["switch", "-q", "main"]);
        let repo = Repo::open(dir.path()).unwrap();
        let mut s = agent_session("with an anchor");
        s.intent_events
            .push(minds_core::intent_anchor::IntentEvent {
                seq: 1,
                anchor_id: minds_core::ContentHash::from_bytes([7; 32]),
                opens_session: true,
            });
        let id = SessionId::of(&s).unwrap();
        let mut sessions = BTreeMap::new();
        sessions.insert(id, s);
        let mut links = BTreeMap::new();
        links.insert(side, vec![id]);
        let inspection = crate::Inspection::from_index(
            crate::Index::from_parts(sessions, links),
            Vec::new(),
            "t",
        );
        let overview = inspection.overview(&repo).unwrap();
        let row = |id: CommitId| overview.rows.iter().find(|r| r.id == id).unwrap();
        // Der Seitenzweig liegt nur auf `feat`.
        assert_eq!(row(side).branches, ["feat"]);
        assert_eq!(row(side).branches_more, 0);
        assert_eq!(
            row(side).anchors,
            [minds_core::ContentHash::from_bytes([7; 32])]
        );
        // Die Basis liegt auf allen sechs: HEAD (main) zuerst, drei Namen,
        // drei weitere als Zahl.
        let b = row(base);
        assert_eq!(b.branches.len(), 3, "{:?}", b.branches);
        assert_eq!(b.branches[0], "main");
        assert_eq!(b.branches_more, 3);
        assert!(b.anchors.is_empty());
    }

    /// Mehr Sessions, als die Zeile ansieht: nie „sealed".
    #[test]
    fn many_sessions_are_never_called_sealed() {
        use minds_core::{Agent, Intent, Model, Session};
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=A", "-c", "user.email=a@x"])
            .args(["init", "-q"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=A", "-c", "user.email=a@x"])
            .args(["commit", "-q", "--allow-empty", "-m", "one"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let repo = Repo::open(dir.path()).unwrap();
        let head = repo.head().unwrap().commit().unwrap();
        let mut sessions = BTreeMap::new();
        let mut ids = Vec::new();
        for n in 0..(MAX_SESSIONS_PER_ROW + 1) {
            let s = Session::new(
                Agent {
                    name: "claude-code".into(),
                    version: "1".into(),
                },
                Model {
                    provider: "t".into(),
                    id: "t".into(),
                },
                Intent {
                    request: format!("task {n}"),
                    ..Intent::default()
                },
            );
            let id = SessionId::of(&s).unwrap();
            sessions.insert(id, s);
            ids.push(id);
        }
        let mut links = BTreeMap::new();
        links.insert(head, ids.clone());
        // Jede Session sauber und signiert versiegelt — nur die Zahl zählt.
        let mut index = crate::Index::from_parts(sessions, links);
        for id in &ids {
            let seal = minds_core::evidence::Seal {
                root: minds_core::ContentHash::from_bytes([9u8; 32]),
                agent: "claude-code".into(),
                scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
                first_seq: 0,
                last_seq: 3,
                events: 4,
                gaps: 0,
                pre_chain: 0,
                outcome: minds_core::evidence::SealOutcome::Stored {
                    session: id.to_string(),
                },
                previous: None,
                last_event_at: "2026-07-25T14:10:00Z".into(),
            };
            let seal_id = minds_core::evidence::Seal::id_of_text(&seal.to_text().unwrap());
            index = index.with_seals(*id, vec![(seal_id, seal, true)]);
        }
        let inspection = crate::Inspection::from_index(index, Vec::new(), "t");
        let overview = inspection.overview(&repo).unwrap();
        assert_eq!(overview.rows[0].seal, Some(SealMark::Partial));
    }
}
