//! Der Checkpoint: aus Journal und Transkript werden [`Session`]s.
//!
//! Hier laufen die beiden Hälften zusammen, die der ganze Entwurf getrennt
//! hält. Das **Journal** ist die Wahrheit über *Ordnung* und *Tool-Calls* — ein
//! Beobachter mit einer Uhr hat jedes Event gesehen. Das **Transkript** ist die
//! Wahrheit über *Inhalt* — Antworttext, Token-Zähler, Modell-ID, die im
//! Hook-Payload nicht stehen. Der Adapter baut die Struktur aus dem Journal und
//! füllt den Inhalt aus dem Transkript.
//!
//! # Deterministisch, mit Absicht
//!
//! Keine Uhr, kein Zufall: Alle Zeitangaben stammen aus den Events, die
//! Token-Zähler aus dem Transkript. Derselbe Journal-Inhalt und dasselbe
//! Transkript ergeben Byte für Byte dieselbe [`Session`] und damit dieselbe
//! `SessionId`. Ohne diese Zusage wären die Fixture-Tests aus M5.9 nicht
//! schreibbar und die Content-Adressierung eine Behauptung.
//!
//! # Was der Journal-Adapter aus den Events macht
//!
//! - Ein **Prompt** öffnet einen User-Zug.
//! - Ein oder mehrere **PreToolUse** sammeln sich in *einem* Assistant-Zug; sein
//!   `at` ist der Zeitpunkt des ersten Tool-Calls.
//! - Ein **Stop** schließt den Assistant-Zug. Hatte der Zug keine Tools (das
//!   Modell hat nur geredet), entsteht trotzdem ein leerer Assistant-Zug — sonst
//!   ginge eine reine Text-Antwort verloren, die nur das Transkript kennt.
//!
//! `Turn::parent` bleibt `None`: Das Journal zeigt einen linearen Verlauf, und
//! bei einem linearen Verlauf ist der Elternindex schlicht `i-1` und damit
//! redundant (siehe [`Turn`]). Verzweigungen aus `/resume` oder Rewind sieht der
//! Hook nicht; sie kämen additiv aus dem Transkript.
//!
//! # Was hier noch *nicht* passiert
//!
//! Kanten (Sub-Agent, Übergabe, Commit) und der Inhalts-Hash am [`Effect`] sind
//! M5.7. Redaction ist der Schritt *danach*: Dieser Adapter liefert die rohe,
//! un-redigierte [`Session`]; erst `minds capture` schickt sie durch die
//! Pipeline und in den Store. Der Store nimmt nur redigierte Sessions an — das
//! ist ein Typ, kein Vorsatz.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use minds_core::{
    Agent, CaptureNote, ContentHash, EffectKind, Intent, Lineage, Model, Produced, Redaction, Role,
    Session, ToolCall, Turn, WrittenUnavailable,
};

use crate::edges;
use crate::exec_outcome;
use crate::journal::{EventKind, Journal, JournalEvent, SessionKey};
use crate::normalize;
use crate::transcript::{self, Transcript};

/// Der Kontext eines Checkpoints — was über die Events hinaus bekannt ist.
///
/// Beides ist optional, weil ein Checkpoint auch ohne beides sinnvoll ist: Ohne
/// `root` bleiben die Artefakt-Hashes leer, ohne `commit` fehlt die
/// Produced-Kante. Der Adapter erzwingt nichts, was die Aufrufstelle nicht hat.
#[derive(Default, Clone, Copy)]
pub struct Checkpoint<'a> {
    /// Wurzel für die Auflösung relativer Effekt-Pfade beim Artefakt-Hash.
    /// Üblicherweise die Repo-Wurzel.
    pub root: Option<&'a Path>,

    /// Der Commit, den dieser Checkpoint begleitet (post-commit-Hook).
    pub commit: Option<&'a str>,

    /// Die von git **getrackten**, repo-relativen Pfade — die Grenze für
    /// Read-Hashes (Phase 6). Getrackter Inhalt ist für jeden Leser des
    /// Repos ohnehin sichtbar; sein Hash verrät nichts Neues. Alles andere
    /// (untracked, absolut, außerhalb des Worktrees) bekommt beim bloßen
    /// **Lesen** nie einen Hash — ein ungesalzener Inhalts-Hash über eine
    /// kurze, private Datei wäre ein Bestätigungsorakel; dieselbe
    /// Bedrohungsklasse, gegen die der Chain-Root gesalzen ist. `None`
    /// heißt: Grenze unbekannt ⇒ keine Read-Hashes (fail-closed in Richtung
    /// „weniger Fingerabdruck").
    pub tracked: Option<&'a std::collections::BTreeSet<String>>,

    /// Die Redaction-Policy des Repos — der Prüfstein für jeden Hash über
    /// geschriebene Bytes (`content` wie `written` eines Schreib-Effekts).
    /// Ein ungesalzener Hash über Bytes, in denen ein Secret steht, wäre ein
    /// Wörterbuch-Orakel; deshalb werden genau die Bytes gescannt, die
    /// gehasht würden, und bei jedem Fund entsteht kein Hash. `None` heißt:
    /// nichts prüfbar ⇒ keine Schreib-Hashes (fail-closed; `written` trägt
    /// dann [`WrittenUnavailable::Unscanned`]).
    pub redaction: Option<&'a minds_redact::RedactionPipeline>,
}

impl std::fmt::Debug for Checkpoint<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checkpoint")
            .field("root", &self.root)
            .field("commit", &self.commit)
            .field("tracked", &self.tracked.map(|set| set.len()))
            .field("redaction", &self.redaction.map(|p| p.len()))
            .finish()
    }
}

/// Baut aus allen Sessions eines Journals ihre [`Session`]-Records.
///
/// Reihenfolge ist die von [`Journal::sessions`] (Agent, dann `local_id`) — so
/// ist auch der Rückgabewert deterministisch.
///
/// Unauflösbare Verzeichnisse ([`SessionsOutcome::unresolved`]) meldet dieser
/// Pfad nicht — wer sie sehen muss, geht über [`Journal::sessions`] selbst,
/// wie `minds checkpoint` und `minds fsck` es tun.
///
/// [`SessionsOutcome::unresolved`]: crate::SessionsOutcome
pub fn build(journal: &Journal) -> crate::Result<Vec<Session>> {
    let mut out = Vec::new();
    for key in journal.sessions()?.keys {
        let read = journal.read(&key)?;
        if read.events.is_empty() {
            continue;
        }
        // Default-Kontext: keine Repo-Wurzel, kein Commit. Damit unterbleiben
        // Artefakt-Hashes (kein I/O) und die Commit-Kante — aber die
        // event-abgeleiteten Sub-Agent-Kanten kommen mit, weil sie nichts
        // Externes brauchen. `minds capture` (M6) ruft `checkpoint` mit vollem
        // Kontext, wenn Wurzel und Commit feststehen.
        let mut session = checkpoint(&key, &read.events, &Checkpoint::default());
        settle_closed(&mut session, read.is_complete());
        out.push(session);
    }
    Ok(out)
}

/// Baut die [`Session`] einer einzelnen Journal-Session.
///
/// Öffentlich, damit `minds capture` (M6) und die Fixture-Tests eine Session
/// gezielt bauen können, ohne den Umweg über das ganze Journal.
pub fn build_one(key: &SessionKey, events: &[JournalEvent]) -> Session {
    build_indexed(key, events).0
}

/// Wie [`build_one`], liefert zusätzlich je Tool-Call (in Envelope-Reihenfolge,
/// über alle Züge flach) den Index des Journal-Events, aus dem er entstand.
/// Der Checkpoint braucht diese Brücke zurück zu den Events, um den
/// PostToolUse-Payload eines Aufrufs zu finden ([`written_hashes`]).
fn build_indexed(key: &SessionKey, events: &[JournalEvent]) -> (Session, Vec<usize>) {
    build_with_transcript(key, events, read_transcript(events))
}

fn build_with_transcript(
    key: &SessionKey,
    events: &[JournalEvent],
    transcript: Transcript,
) -> (Session, Vec<usize>) {
    let agent = key.agent();

    let (turns, call_events) = build_turns(agent, events, &transcript);
    let intent = Intent {
        request: first_prompt(agent, events).unwrap_or_default(),
        discarded: discarded_files(&turns),
        ..Intent::default()
    };
    let produced = Produced {
        commit_hint: None,
        files: produced_files(&turns),
    };

    let session = Session {
        schema_version: minds_core::SCHEMA_VERSION,
        agent: Agent {
            name: agent.to_string(),
            version: transcript.agent_version.clone().unwrap_or_else(unknown),
        },
        model: transcript.model.clone().unwrap_or_else(|| Model {
            provider: unknown(),
            id: unknown(),
        }),
        intent,
        turns,
        usage: transcript.usage,
        produced,
        redaction: Redaction::default(),
        lineage: Some(lineage(key, events)),
        edges: Vec::new(),
        intent_anchor: None,
        intent_events: Vec::new(),
    };
    (session, call_events)
}

/// Baut die [`Session`] und reichert sie mit dem an, was nur der Checkpoint
/// weiß: Artefakt-Hashes an den Schreib-Effekten und die Kanten (Sub-Agent,
/// Commit).
///
/// Der teure Teil — Dateien lesen und hashen — passiert nur hier, nicht in
/// [`build_one`]: Wer nur die Struktur will (der Reader, ein Test), zahlt den
/// I/O nicht.
pub fn checkpoint(key: &SessionKey, events: &[JournalEvent], ctx: &Checkpoint) -> Session {
    let (mut session, call_events) = build_indexed(key, events);

    written_hashes(&mut session, events, &call_events, key.agent(), ctx);
    exec_outcomes(&mut session, events, &call_events, key.agent());
    hash_artifacts(&mut session, ctx, &[]);

    session.edges.extend(edges::subagent(key.agent(), events));
    if let Some(commit) = ctx.commit {
        session.edges.push(edges::commit(commit));
    }

    session
}

/// Der Host darf aus fremden Hook-Pfaden keine Transkripte nachladen.
/// Nur die Artefakt-Lesezugriffe werden gemappt; Evidence und Effektpfade
/// bleiben im ursprünglichen Namensraum.
pub fn checkpoint_witness(
    key: &SessionKey,
    events: &[JournalEvent],
    ctx: &Checkpoint,
    path_map: &[(PathBuf, PathBuf)],
) -> Session {
    let (mut session, call_events) = build_with_transcript(key, events, Transcript::default());
    let agent_root = path_map
        .iter()
        .find(|(_, host)| Some(host.as_path()) == ctx.root)
        .map(|(agent, _)| agent.as_path())
        .or(ctx.root);
    let written_ctx = Checkpoint {
        root: agent_root,
        ..*ctx
    };
    written_hashes(
        &mut session,
        events,
        &call_events,
        key.agent(),
        &written_ctx,
    );
    exec_outcomes(&mut session, events, &call_events, key.agent());
    hash_artifacts(&mut session, ctx, path_map);
    session.edges.extend(edges::subagent(key.agent(), events));
    if let Some(commit) = ctx.commit {
        session.edges.push(edges::commit(commit));
    }
    session.intent_events = intent_events(events);
    session
}

/// Die vom Witness verketteten Intent-Events eines Bereichs (EA-14) — eine
/// Beobachtung wie [`lineage`]`.closed`: Nummer, Anker-Id und ob der Witness
/// das Event als erstes der Session geschrieben hat (`opens_session`, aus dem
/// Payload — nie aus der Position im Bereich). Bewertet wird nichts (W2).
///
/// Nur der Witness-Pfad ruft das: Dort nimmt der Witness ein Hook-Event mit
/// dem reservierten Namen nie von der Agent-Seite an, jedes solche Event hat
/// er selbst geschrieben. Das lokale Journal liegt dagegen im Zugriff des
/// Agenten — dort wäre ein `minds.intent`-Event eine Behauptung, keine
/// Beobachtung, und bleibt draußen. Ein Payload, dessen Anker nicht parst
/// oder dessen Id nicht zum Text passt, wird nicht übernommen (fail-closed:
/// lieber „nicht gebunden" als ein falscher Anker).
pub fn intent_events(events: &[JournalEvent]) -> Vec<minds_core::intent_anchor::IntentEvent> {
    use minds_core::intent_anchor::{INTENT_EVENT_KIND, IntentEvent, IntentEventPayload};

    events
        .iter()
        .filter(|e| {
            e.raw_kind == INTENT_EVENT_KIND
                && e.kind == crate::EventKind::Other
                // Synthetische Witness-Events tragen weder `cwd` noch ein
                // Transkript — Hook-Events der Agent-Seite meist schon.
                && e.cwd.is_none()
                && e.transcript_path.is_none()
        })
        .filter_map(|e| {
            let payload: IntentEventPayload = serde_json::from_str(e.payload.get()).ok()?;
            payload.check().ok()?;
            Some(IntentEvent {
                seq: e.seq,
                anchor_id: payload.anchor_id,
                opens_session: payload.opens_session,
            })
        })
        .collect()
}

/// Füllt die Inhalts-Hashes der Effekte, deren Datei sich lesen lässt.
///
/// Vier Regeln, alle fail-closed:
/// - **Schreib**-Effekte: Der Hash ist der Fingerabdruck des vom Agenten
///   *erzeugten* Artefakts — aber nur, wenn die Redaction in den Bytes nichts
///   findet ([`Checkpoint::redaction`], [`scanned_hash`]).
/// - **Lese**-Effekte nur innerhalb der Read-Grenze ([`Checkpoint::tracked`]):
///   getrackte, repo-relative Pfade — als Beweismittel für Content-Übergaben
///   (Phase 6). Bloßes Lesen einer privaten oder repo-fremden Datei erzeugt
///   nie einen Fingerabdruck.
/// - **Nie** für eine Zugangsdaten-Datei: Bei einer kurzen, ratbaren Datei wäre
///   ein Hash ein Orakel. Die Secretfile-Mauer gilt auch für Fingerabdrücke.
/// - Lese-Effekte werden **nicht** gescannt: Sie liegen innerhalb der
///   Read-Grenze, ihr Inhalt ist für jeden Repo-Leser ohnehin sichtbar.
fn hash_artifacts(session: &mut Session, ctx: &Checkpoint, path_map: &[(PathBuf, PathBuf)]) {
    let Checkpoint {
        root,
        tracked,
        redaction,
        ..
    } = *ctx;
    // Wurzel roh und kanonisch, wie bei `written` (siehe dort).
    let canonical_root = root.and_then(|r| fs::canonicalize(r).ok());
    let roots: Vec<&Path> = root.into_iter().chain(canonical_root.as_deref()).collect();
    for call in session.turns.iter_mut().flat_map(|t| &mut t.tool_calls) {
        let Some(effect) = call.effect.as_mut() else {
            continue;
        };
        // Seit Phase 6 (Evidence-DAG) auch Read-Effekte: Der Hash ist das
        // Beweismittel für Content-Übergaben zwischen Agents — „B las exakt
        // die Bytes, die A schrieb" braucht beide Seiten. Der Hash entsteht
        // zum Checkpoint-Zeitpunkt: Hat sich die Datei seit dem Lesen
        // geändert, entsteht schlicht kein Match (falsch-negativ, nie
        // falsch-positiv — ein Match heißt immer „dieselben Bytes").
        if effect.content.is_some() || !matches!(effect.kind, EffectKind::Write | EffectKind::Read)
        {
            continue;
        }
        let Some(path) = effect.path.as_deref() else {
            continue;
        };
        if minds_redact::is_secret_file(path) {
            continue;
        }
        let mapped = path_map
            .iter()
            .filter_map(|(agent, host)| {
                Path::new(path)
                    .strip_prefix(agent)
                    .ok()
                    .map(|rest| (agent.components().count(), host.join(rest)))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, path)| path);
        let path = mapped.as_deref().and_then(Path::to_str).unwrap_or(path);
        if minds_redact::is_secret_file(path) {
            continue;
        }
        // Boundary gegen Pfad-Flucht: `path` kommt aus Text, den der Agent
        // selbst gewählt hat (ein Tool-Argument, bei `apply_patch` sogar eine
        // frei formulierte Diff-Kopfzeile) — nie aus vertrauenswürdiger
        // Eingabe. Ein absoluter oder über `..` aus dem Repo führender Pfad
        // würde sonst beliebige Dateien der Maschine hashen: ein
        // Existenz-/Inhalts-Orakel. Gilt für Schreib- **und** Lese-Effekte
        // gleichermaßen — mit derselben Grenze wie `written`: Claude Code
        // nennt Pfade absolut, ein absoluter Pfad unter der Wurzel ist innen.
        if !inside_repo_resolved(path, &roots) {
            continue;
        }
        // Read-Grenze (siehe [`Checkpoint::tracked`]): zusätzlich nur
        // getrackte Pfade — bloßes Lesen einer privaten Datei erzeugt keinen
        // Fingerabdruck. Schreib-Effekte brauchen das nicht: Was der Agent
        // innerhalb des Repos erzeugt hat, ist sein Artefakt.
        let relative = root
            .and_then(|root| Path::new(path).strip_prefix(root).ok())
            .and_then(Path::to_str)
            .unwrap_or(path);
        if effect.kind == EffectKind::Read && !tracked.is_some_and(|set| set.contains(relative)) {
            continue;
        }
        let Some(bytes) = read_artifact(root, path) else {
            continue;
        };
        effect.content = match effect.kind {
            // Nicht-UTF-8 (etwa UTF-16) lässt sich nicht verlässlich scannen:
            // verlustbehaftet gelesen, fände kein Detektor `g\0h\0p\0_…`.
            // Ungeprüft gibt es keinen Hash.
            EffectKind::Write => std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| scanned_hash(redaction, text, &bytes).ok()),
            _ => Some(ContentHash::from_bytes(*blake3::hash(&bytes).as_bytes())),
        };
    }
}

/// Füllt den **Schreibzeit**-Hash ([`Effect::written`](minds_core::Effect::written))
/// der Schreib-Effekte — aus dem PostToolUse-Payload, nie von der Platte.
///
/// Die Reihenfolge ist die Mauer, nicht bloß eine Optimierung:
///
/// 1. **Secretfile-Mauer zuerst.** Für eine Zugangsdaten-Datei wird der
///    Payload gar nicht angesehen — kein Hash, kein Orakel, dieselbe Regel
///    wie bei [`hash_artifacts`]. Die Hot-Path-Wall hat den Payload meist
///    schon ersetzt; hier gilt sie unabhängig davon noch einmal.
/// 2. **Repo-Grenze.** Nur Pfade innerhalb der Wurzel ([`inside_repo`]).
///    Ein Schreib-Hash für `~/.ssh/config` wäre ein Fingerabdruck über eine
///    private Datei; ohne bekannte Wurzel ist jeder absolute Pfad draußen.
/// 3. **Korrelation.** Pre- und Post-Event desselben Aufrufs verbindet die
///    Kennung des Adapters ([`ToolAdapter::call_id`](crate::ToolAdapter::call_id));
///    fehlt sie, ist sie doppelt oder fehlt das Post-Event, gibt es keinen Hash
///    und der Grund steht daneben. Über die Reihenfolge wird nicht geraten.
/// 4. Erst dann deutet der Adapter den Payload
///    ([`ToolAdapter::written_bytes`](crate::ToolAdapter::written_bytes)).
/// 5. **Redaction-Scan** über genau die Bytes, die gehasht würden
///    ([`scanned_hash`]). Das fängt, was die zweite Linie in der Redaction
///    (`arguments` getroffen ⇒ Hash weg) nicht sehen kann: das Original eines
///    `Edit`, das Notebook nach `NotebookEdit`, eine Antwort, die vom
///    `tool_input` abweicht.
///
/// Ein Agent ohne Adapter bekommt weder Hash noch Grund: Seine Effekte sind
/// ohnehin ungedeutet.
fn written_hashes(
    session: &mut Session,
    events: &[JournalEvent],
    call_events: &[usize],
    agent: &str,
    ctx: &Checkpoint,
) {
    let root = ctx.root;
    let Some(adapter) = normalize::adapter_for(agent) else {
        return;
    };
    // Die Wurzel auch in kanonischer Form: Claude Code bildet `file_path` aus
    // seinem `cwd`, gix liefert die Wurzel — liegt ein Symlink dazwischen
    // (macOS `/tmp` → `/private/tmp`, verlinktes Home), waere sonst jede
    // Schreibung der Session still „draussen". Die Gegenrichtung (der Agent
    // nennt den Symlink-Pfad, die Wurzel ist aufgeloest) deckt
    // [`inside_repo_resolved`] ab. I/O nur fuer Verzeichnisse, nie fuer die
    // Datei.
    let canonical_root = root.and_then(|r| fs::canonicalize(r).ok());
    let roots: Vec<&Path> = root.into_iter().chain(canonical_root.as_deref()).collect();
    let posts = PostIndex::new(adapter, events);

    let calls = session.turns.iter_mut().flat_map(|t| &mut t.tool_calls);
    for (call, &pre_index) in calls.zip(call_events) {
        let Some(effect) = call.effect.as_mut() else {
            continue;
        };
        if effect.kind != EffectKind::Write {
            continue;
        }
        let outcome = match effect.path.as_deref() {
            None => Err(WrittenUnavailable::PayloadWithoutContent),
            Some(path) if minds_redact::is_secret_file(path) => Err(WrittenUnavailable::SecretFile),
            Some(path) if !inside_repo_resolved(path, &roots) => {
                Err(WrittenUnavailable::OutsideRepo)
            }
            Some(path) => match posts.post_for(events.get(pre_index)) {
                Some(post) => adapter
                    .written_bytes(post, path)
                    .and_then(|bytes| scanned_hash(ctx.redaction, &bytes, bytes.as_bytes())),
                None => Err(WrittenUnavailable::PayloadWithoutContent),
            },
        };
        match outcome {
            Ok(hash) => effect.written = Some(hash),
            Err(reason) => effect.written_unavailable = Some(reason),
        }
    }
}

/// Die Brücke von einem Pre-Event zu seinem Post-Event über die Kennung des
/// Adapters ([`ToolAdapter::call_id`](crate::ToolAdapter::call_id)) —
/// geteilt von Schreibzeit-Hash und Runner-Ergebnis, damit beide nach
/// derselben Regel zuordnen.
///
/// Fail-closed: Eine Kennung, die zwei Pre-Events tragen, ordnet keinem von
/// beiden ein Post-Event zu; eine Kennung an zwei Post-Events ist keine
/// Zuordnung, sondern zwei — dann keine. Über die Reihenfolge wird nicht
/// geraten.
struct PostIndex<'e> {
    adapter: &'static dyn normalize::ToolAdapter,
    pre_ids: BTreeMap<String, usize>,
    posts: BTreeMap<String, Option<&'e JournalEvent>>,
}

impl<'e> PostIndex<'e> {
    fn new(adapter: &'static dyn normalize::ToolAdapter, events: &'e [JournalEvent]) -> Self {
        let mut pre_ids: BTreeMap<String, usize> = BTreeMap::new();
        for event in events.iter().filter(|e| e.kind == EventKind::ToolPre) {
            if let Some(id) = adapter.call_id(event) {
                *pre_ids.entry(id).or_default() += 1;
            }
        }
        let mut posts: BTreeMap<String, Option<&JournalEvent>> = BTreeMap::new();
        for event in events.iter().filter(|e| e.kind == EventKind::ToolPost) {
            if let Some(id) = adapter.call_id(event) {
                posts
                    .entry(id)
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(event));
            }
        }
        Self {
            adapter,
            pre_ids,
            posts,
        }
    }

    /// Das eindeutige Post-Event zu diesem Pre-Event, falls es eines gibt.
    fn post_for(&self, pre: Option<&JournalEvent>) -> Option<&'e JournalEvent> {
        pre.and_then(|pre| self.adapter.call_id(pre))
            .filter(|id| self.pre_ids.get(id) == Some(&1))
            .and_then(|id| self.posts.get(&id).copied().flatten())
    }
}

/// Füllt [`ToolCall::outcome`] der Shell-Aufrufe bekannter Test- und
/// Benchmark-Runner — aus dem Post-Payload, nie aus einer Datei (EA-18a).
///
/// Die Reihenfolge:
///
/// 1. Nur Adapter, deren Payload-Form aufgezeichnet ist
///    ([`ToolAdapter::exec_outcomes`](crate::ToolAdapter::exec_outcomes)),
///    und nur `Exec`-Effekte.
/// 2. **Einfaches Kommando** ([`exec_outcome::simple_argv`]). Sonst kein
///    Ergebnis — und, wenn das Kommando einen bekannten Runner nennt, der
///    Hinweis [`CaptureNote::CompoundCommandNotInterpreted`]. Ein Replay
///    soll nie eine Shell brauchen.
/// 3. **Bekannter Runner** ([`exec_outcome::recognize`]).
/// 4. **Korrelation** mit dem Post-Event ([`PostIndex`]), Gegenprobe des
///    Kommandos im Post-Payload durch den Adapter.
/// 5. **Zusammenfassung parsen** ([`exec_outcome::interpret`]): Gespeichert
///    werden nur Zahlen und Bench-Namen. Die Bench-Namen und das argv
///    scannt die Redaction danach wie jedes Textfeld.
///
/// Eine Ableitung im Sinne der Invariante? Nein — eine **Deutung** des
/// Adapters, versioniert über [`Capture::adapter_version`](minds_core::Capture),
/// wie `effect` und `written`. Sie kann nur hier entstehen: Der Payload liegt
/// nach dem Checkpoint nicht mehr vor. Ob die Behauptung stimmt, rechnet
/// erst ein Replay im Reader (W5).
fn exec_outcomes(
    session: &mut Session,
    events: &[JournalEvent],
    call_events: &[usize],
    agent: &str,
) {
    let Some(adapter) = normalize::adapter_for(agent).filter(|a| a.exec_outcomes()) else {
        return;
    };
    let posts = PostIndex::new(adapter, events);

    let calls = session.turns.iter_mut().flat_map(|t| &mut t.tool_calls);
    for (call, &pre_index) in calls.zip(call_events) {
        if call.effect.as_ref().map(|e| e.kind) != Some(EffectKind::Exec) {
            continue;
        }
        let Some(command) = command_str(&call.arguments) else {
            continue;
        };
        let argv = match exec_outcome::simple_argv(&command) {
            Ok(argv) => argv,
            Err(_) => {
                if exec_outcome::mentions_runner(&command)
                    && let Some(capture) = call.capture.as_mut()
                {
                    capture.note = Some(CaptureNote::CompoundCommandNotInterpreted);
                }
                continue;
            }
        };
        let Some(runner) = exec_outcome::recognize(&argv) else {
            // Ein einfaches Kommando, das einen Runner nur umhüllt
            // (`timeout 600 cargo test`, `env X=1 pytest`): Gedeutet wird
            // nur ein Runner an `argv[0]` — der Grund steht dabei.
            if exec_outcome::wraps_runner(&argv)
                && let Some(capture) = call.capture.as_mut()
            {
                capture.note = Some(CaptureNote::ResultNotCaptured);
            }
            continue;
        };
        call.outcome = posts
            .post_for(events.get(pre_index))
            .and_then(|post| adapter.exec_report(post, &command))
            .and_then(|report| exec_outcome::interpret(runner, argv, &report));
        // Erkannt, aber ohne Zusammenfassung — kein Ergebnis, oder nur ein
        // Exit-Code (Build-Fehler, unlesbare Zusammenfassung): Das steht
        // dabei, damit „kein `outcome`" nicht als „nicht versucht" und ein
        // nackter Exit-Code nicht als „gelesen" erscheint.
        let summarized = call
            .outcome
            .as_ref()
            .is_some_and(|o| o.tests.is_some() || !o.benches.is_empty());
        if !summarized && let Some(capture) = call.capture.as_mut() {
            capture.note = Some(CaptureNote::ResultNotCaptured);
        }
    }
}

/// blake3 über `bytes` — nur, wenn die Redaction in `text` (denselben Bytes
/// als Text) nichts findet und keinen Detektor-Fehler meldet.
///
/// Das ist die erste Linie gegen das Wörterbuch-Orakel: Ein ungesalzener
/// Hash über eine kurze Datei mit einem Passwort ließe sich gegen Kandidaten
/// durchprobieren. Ohne Pipeline gibt es nichts zu prüfen und deshalb keinen
/// Hash. `text` und `bytes` sind dieselben Bytes — Aufrufer mit
/// Nicht-UTF-8-Bytes hashen gar nicht erst (siehe [`hash_artifacts`]).
fn scanned_hash(
    redaction: Option<&minds_redact::RedactionPipeline>,
    text: &str,
    bytes: &[u8],
) -> Result<ContentHash, WrittenUnavailable> {
    let pipeline = redaction.ok_or(WrittenUnavailable::Unscanned)?;
    let scan = pipeline.redact(text);
    if scan.counts != minds_core::RedactionCounts::default() || scan.invalid_findings > 0 {
        return Err(WrittenUnavailable::RedactedContent);
    }
    Ok(ContentHash::from_bytes(*blake3::hash(bytes).as_bytes()))
}

/// Wie [`inside_repo`], mit einer zweiten Chance für einen absoluten Pfad,
/// der lexikalisch draußen liegt: Sein **Elternverzeichnis** wird aufgelöst
/// und der Pfad erneut geprüft. Das fängt den Fall, dass der Agent den
/// Symlink-Pfad nennt (`/var/folders/…`, ein verlinktes `~/dev`), die Wurzel
/// aber aufgelöst vorliegt (`/private/var/folders/…`).
///
/// Die lexikalischen Regeln gelten vorher am rohen Pfad: `..`, `~`, `$` und
/// Backslash bleiben draußen, auch wenn die Auflösung „drinnen" ergäbe.
/// Aufgelöst wird nur ein Verzeichnis, nie die Datei — ihr Inhalt wird hier
/// nicht gelesen. Existiert das Verzeichnis nicht, bleibt es bei „draußen".
///
/// Bewusst **nicht** geprüft: ein Pfad, der lexikalisch innen liegt, aber
/// über einen Symlink im Repo nach draußen zeigt (`/repo/link → ~/.aws`).
/// Er gilt als innen. Für `Write` stehen dieselben Bytes ohnehin in den
/// `arguments`; für `Edit` fingert `written` dann eine Datei außerhalb —
/// eine benannte Grenze dieser Prüfung (siehe ADR-0011, Entscheidung 9),
/// die der Redaction-Scan für Secret-förmigen Inhalt abdeckt, für anderen
/// nicht.
fn inside_repo_resolved(path: &str, roots: &[&Path]) -> bool {
    if inside_repo(path, roots) {
        return true;
    }
    let candidate = Path::new(path);
    if !candidate.is_absolute() || !lexically_plain(path) {
        return false;
    }
    let (Some(parent), Some(name)) = (candidate.parent(), candidate.file_name()) else {
        return false;
    };
    let Ok(parent) = fs::canonicalize(parent) else {
        return false;
    };
    parent
        .join(name)
        .to_str()
        .is_some_and(|resolved| inside_repo(resolved, roots))
}

/// Liegt `path` innerhalb einer der Repo-Wurzeln (roh und kanonisch)?
///
/// Relativ ohne `..` gilt als innen (so wie es die Read-Grenze in
/// [`hash_artifacts`] hält) — außer der Pfad ist nur *technisch* relativ:
/// `~/…` und `$HOME/…` meinen eine Shell-Expansion, ein Backslash einen
/// Windows-Pfad; beides zeigt nicht ins Repo. Absolut nur, wenn eine Wurzel
/// bekannt ist und der Pfad lexikalisch unter ihr liegt — Claude Code nennt
/// Pfade absolut, sonst bliebe `written` in jeder echten Session leer. Ohne
/// Wurzel ist jeder absolute Pfad draußen (fail-closed in Richtung „kein
/// Fingerabdruck"). Rein lexikalisch: Die Platte wird hier nicht angefasst
/// (die Auflösung von Symlinks macht [`inside_repo_resolved`]).
fn inside_repo(path: &str, roots: &[&Path]) -> bool {
    if !lexically_plain(path) {
        return false;
    }
    let candidate = Path::new(path);
    if candidate.is_relative() {
        return true;
    }
    roots
        .iter()
        .any(|root| candidate.strip_prefix(root).is_ok())
}

/// Kein `..`, keine Shell-Expansion (`~`, `$`), kein Windows-Pfad — die
/// Regeln, die ein Pfad vor jeder Grenzprüfung erfüllen muss.
fn lexically_plain(path: &str) -> bool {
    !(path.starts_with('~') || path.starts_with('$') || path.contains('\\'))
        && !Path::new(path)
            .components()
            .any(|c| c == Component::ParentDir)
}

/// Liest die Artefakt-Datei. Absolute Pfade wie sie sind, relative gegen `root`
/// (fehlt `root`, gegen das Arbeitsverzeichnis). Fehlt die Datei, ist das kein
/// Fehler — der Hash bleibt dann `None`.
fn read_artifact(root: Option<&Path>, path: &str) -> Option<Vec<u8>> {
    let candidate = PathBuf::from(path);
    let full = if candidate.is_absolute() {
        candidate
    } else {
        match root {
            Some(root) => root.join(candidate),
            None => candidate,
        }
    };
    // Der Pfad liegt lexikalisch im Repo; ein Symlink darin kann trotzdem
    // hinaus zeigen. Beim Witness (EA-06d) liegt „hinaus" auf dem Host, in
    // einer anderen Vertrauensdomäne — der Hash einer Datei, die der Agent
    // selbst nicht lesen kann, wäre ein Orakel. Also zählt der echte Ort.
    let real = full.canonicalize().ok()?;
    // Die Mauer prüft den Namen im Event; ein Symlink `notes.rs -> .env`
    // hieße dort harmlos. Maßgeblich ist auch hier der echte Ort.
    if real.to_str().is_none_or(minds_redact::is_secret_file) {
        return None;
    }
    let beneath = match root {
        Some(root) => {
            let root = root.canonicalize().ok()?;
            if !real.starts_with(&root) {
                return None;
            }
            Some(root)
        }
        None => None,
    };
    read_regular_file(&real, MAX_ARTIFACT_BYTES, beneath.as_deref())
}

/// Obergrenze für eine Artefakt-Datei. Quelltext liegt Größenordnungen
/// darunter; eine größere Datei bekommt keinen Hash, statt den Speicher zu
/// füllen und — bei Schreib-Effekten — die ganze Redaction-Pipeline über
/// sich laufen zu lassen. Eng, weil der Agent beliebig viele Events auf
/// dieselbe große Datei zeigen lassen kann und der Witness jede davon im
/// Checkpoint liest.
const MAX_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;

/// Liest `path` nur, wenn es eine gewöhnliche Datei bis `max` Bytes ist.
///
/// Der Pfad stammt aus dem Event, also vom Agenten — und seit EA-06d kann der
/// Agent den Checkpoint des Witness jederzeit auslösen. Ein FIFO an dieser
/// Stelle hielte den einzigen Schreiber an, ein Gerät wie `/dev/zero` füllte
/// seinen Speicher. Deshalb: nichtblockierend öffnen, am offenen Deskriptor
/// prüfen (kein Zeitfenster zwischen Prüfen und Lesen), begrenzt lesen.
///
/// Mit `beneath` zählt außerdem, wo die **geöffnete** Datei tatsächlich liegt
/// (vom Deskriptor erfragt): Wer zwischen Prüfung und Öffnen einen Teil des
/// Pfads gegen einen Symlink tauscht, landet außerhalb — und bekommt nichts.
fn read_regular_file(path: &Path, max: u64, beneath: Option<&Path>) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    // Ein harter Link `notes.rs` ⇄ `.env` hätte einen harmlosen Namen und
    // läge im Repo. Ein Checkout legt nie harte Links an; Werkzeuge wie pnpm
    // tun es — solche Dateien bekommen bewusst keinen Hash (fail-closed).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return None;
        }
    }
    if let Some(beneath) = beneath {
        if let Some(opened) = opened_path(&file) {
            // Am geöffneten Deskriptor noch einmal: Wer den Symlink zwischen
            // Auflösen und Öffnen umbiegt, landet sonst doch bei `.env`.
            if !opened.starts_with(beneath)
                || opened.to_str().is_none_or(minds_redact::is_secret_file)
            {
                return None;
            }
        }
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max).then_some(bytes)
}

/// Der Pfad, unter dem der Kern eine geöffnete Datei kennt — `None`, wo
/// sich das nicht erfragen lässt (dann gilt allein die Prüfung vor dem
/// Öffnen).
#[cfg(target_os = "linux")]
fn opened_path(file: &fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(target_os = "macos")]
fn opened_path(file: &fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: `F_GETPATH` schreibt höchstens `MAXPATHLEN` Bytes samt NUL in
    // einen Puffer dieser Größe; der Deskriptor gehört `file`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn opened_path(_: &fs::File) -> Option<PathBuf> {
    None
}

fn unknown() -> String {
    "unknown".to_string()
}

/// Liest das Transkript, auf das die Events zeigen — best effort.
///
/// Genommen wird der `transcript_path` des letzten Events, das einen trägt: Am
/// Ende der Session ist das Transkript am vollständigsten. Fehlt die Datei oder
/// lässt sie sich nicht lesen, ist das kein Fehler, sondern ein leeres
/// Transkript — das Journal allein trägt die Session.
fn read_transcript(events: &[JournalEvent]) -> Transcript {
    let Some(path) = events
        .iter()
        .rev()
        .find_map(|e| e.transcript_path.as_deref())
    else {
        return Transcript::default();
    };

    match fs::read(Path::new(path)) {
        Ok(bytes) => transcript::parse(&bytes),
        Err(_) => Transcript::default(),
    }
}

/// Der Text des ersten Prompts — die deterministisch extrahierte Absicht.
fn first_prompt(agent: &str, events: &[JournalEvent]) -> Option<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::Prompt)
        .find_map(|e| normalize::facts(agent, e).prompt)
}

/// Baut die Züge aus den Events und färbt die Assistant-Züge der Reihe nach mit
/// den Texten aus dem Transkript ein.
///
/// Der zweite Rückgabewert ist je gebautem Tool-Call der Index seines
/// Pre-Events — flach über alle Züge, in derselben Reihenfolge, in der die
/// Aufrufe im Envelope stehen (Züge werden nur angehängt, nie umsortiert).
fn build_turns(
    agent: &str,
    events: &[JournalEvent],
    transcript: &Transcript,
) -> (Vec<Turn>, Vec<usize>) {
    let mut turns: Vec<Turn> = Vec::new();
    let mut open: Option<Turn> = None;
    let mut call_events: Vec<usize> = Vec::new();

    for (index, event) in events.iter().enumerate() {
        let facts = normalize::facts(agent, event);
        match event.kind {
            EventKind::Prompt => {
                flush(&mut open, &mut turns);
                turns.push(Turn {
                    role: Role::User,
                    text: facts.prompt.unwrap_or_default(),
                    tool_calls: Vec::new(),
                    parent: None,
                    at: Some(event.at.clone()),
                });
            }
            EventKind::ToolPre => {
                let turn = open.get_or_insert_with(|| assistant_turn(&event.at));
                if let Some(tool) = facts.tool {
                    turn.tool_calls.push(ToolCall {
                        outcome: None,
                        name: tool.name,
                        arguments: tool.arguments,
                        effect: tool.effect,
                        capture: Some(tool.capture),
                    });
                    call_events.push(index);
                }
            }
            EventKind::TurnEnd => {
                if open.is_some() {
                    flush(&mut open, &mut turns);
                } else {
                    // Reine Text-Antwort ohne Tools: trotzdem ein Zug, sonst
                    // verschwindet, was nur das Transkript kennt.
                    turns.push(assistant_turn(&event.at));
                }
            }
            _ => {}
        }
    }
    flush(&mut open, &mut turns);

    paint_assistant_text(&mut turns, &transcript.assistant_texts);
    (turns, call_events)
}

fn assistant_turn(at: &str) -> Turn {
    Turn {
        role: Role::Assistant,
        text: String::new(),
        tool_calls: Vec::new(),
        parent: None,
        at: Some(at.to_string()),
    }
}

fn flush(open: &mut Option<Turn>, turns: &mut Vec<Turn>) {
    if let Some(turn) = open.take() {
        turns.push(turn);
    }
}

/// Weist die Transkript-Texte den Assistant-Zügen der Reihe nach zu. Gibt es
/// mehr Züge als Texte (oder umgekehrt), bleibt der Rest, wie er war — best
/// effort, nie ein Absturz.
fn paint_assistant_text(turns: &mut [Turn], texts: &[String]) {
    let mut texts = texts.iter();
    for turn in turns.iter_mut().filter(|t| t.role == Role::Assistant) {
        if let Some(text) = texts.next() {
            turn.text = text.clone();
        }
    }
}

/// Die von der Session geänderten Dateien: die Pfade der Schreib-Effekte,
/// sortiert und ohne Duplikate. Lesungen zählen nicht als „produziert".
fn produced_files(turns: &[Turn]) -> Vec<String> {
    let mut files: Vec<String> = turns
        .iter()
        .flat_map(|t| &t.tool_calls)
        .filter_map(|c| c.effect.as_ref())
        .filter(|e| {
            matches!(
                e.kind,
                minds_core::EffectKind::Write | minds_core::EffectKind::Delete
            )
        })
        .filter_map(|e| e.path.clone())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Verworfene Ansätze, deterministisch aus den Effekten: Dateien, die in
/// derselben Session **angelegt und wieder entfernt** wurden.
///
/// Das ist der eine Sackgassen-Beleg, der hart im Effekt-Muster steht — anders
/// als eine Korrektur im Freitext, die nur eine Heuristik wäre. Weil der
/// Claude-Adapter kein `Delete`-Effekt kennt (Löschen läuft über `Bash rm`),
/// wird die Entfernung auch aus `rm`/`git rm`-Kommandos gelesen; ein künftiger
/// Adapter mit echtem `Delete`-Effekt fällt automatisch mit hinein.
///
/// `constraints` bleibt bewusst leer: dafür gibt es kein verlässliches
/// deterministisches Signal (das bräuchte ein Modell — Summary-Pfad M8), und ein
/// geratener Constraint wäre schlechter als keiner.
fn discarded_files(turns: &[Turn]) -> Vec<String> {
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();

    for call in turns.iter().flat_map(|t| &t.tool_calls) {
        let Some(effect) = &call.effect else { continue };
        match effect.kind {
            EffectKind::Write => {
                if let Some(path) = &effect.path {
                    written.insert(path.clone());
                }
            }
            EffectKind::Delete => {
                if let Some(path) = &effect.path {
                    removed.insert(path.clone());
                }
            }
            EffectKind::Exec => {
                if let Some(command) = command_str(&call.arguments) {
                    for target in rm_targets(&command) {
                        removed.insert(target);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<String> = written
        .iter()
        .filter(|w| removed.iter().any(|r| same_file(w, r)))
        .map(|w| format!("{w} — angelegt und wieder entfernt"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Der Kommando-String aus dem rohen Bash-`arguments`-JSON, falls vorhanden.
fn command_str(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value
        .get("command")
        .or_else(|| value.get("cmd"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// Die von einem `rm`/`git rm`-Kommando entfernten Pfade (Nicht-Flag-Tokens).
/// Leer, wenn das Kommando gar nichts löscht.
fn rm_targets(command: &str) -> Vec<String> {
    let cmd = command.trim();
    let rest = cmd
        .strip_prefix("rm ")
        .or_else(|| cmd.strip_prefix("git rm "));
    match rest {
        Some(rest) => rest
            .split_whitespace()
            .filter(|token| !token.starts_with('-'))
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// Ob zwei Pfade dieselbe Datei meinen — exakt oder über den Basename, damit
/// `rm scratch.rs` auch die geschriebene `src/scratch.rs` trifft.
fn same_file(a: &str, b: &str) -> bool {
    a == b || basename(a) == basename(b)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Die Herkunft: Kennung, Zeitfenster aus dem ersten und letzten Event, `cwd`
/// aus dem ersten Event, das eines nennt, und ob der Bereich mit dem
/// `SessionEnd` des Harness schließt — nur wenn es das **letzte** Event ist:
/// Ging die Session danach weiter (`--resume`), ist sie hier nicht zu Ende.
/// Maßgeblich ist der von der Kette gebundene `raw_kind` (der Event-Hash
/// deckt ihn, das gespeicherte `kind` nicht); `kind` muss dazu passen.
///
/// Ob der Bereich **vollständig** gelesen wurde, weiß dieser Pfad nicht —
/// wer nur Events übergibt, muss [`settle_closed`] mit dem Lesestand
/// aufrufen.
fn lineage(key: &SessionKey, events: &[JournalEvent]) -> Lineage {
    Lineage {
        local_id: key.local_id().to_string(),
        started_at: events.first().map(|e| e.at.clone()),
        ended_at: events.last().map(|e| e.at.clone()),
        cwd: events.iter().find_map(|e| e.cwd.clone()),
        closed: events
            .last()
            .is_some_and(|e| e.raw_kind == "SessionEnd" && e.kind == crate::EventKind::SessionEnd),
    }
}

/// Zieht das Ende der Session (`lineage.closed`) zurück, wenn der Bereich
/// nicht vollständig gelesen wurde ([`crate::ReadOutcome::is_complete`]):
/// Ein abgestürzter Schreibvorgang oder eine Lücke kann hinter dem
/// `SessionEnd` liegen — dann ist nicht belegt, dass es das letzte Event war.
/// Jeder Pfad, der aus einem Journal-Lesestand eine Session baut, ruft das.
pub fn settle_closed(session: &mut Session, read_complete: bool) {
    if !read_complete && let Some(lineage) = session.lineage.as_mut() {
        lineage.closed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein FIFO oder Gerät unter einem Artefakt-Pfad: kein Hash, kein Hängen,
    /// kein voller Speicher (EA-06d — der Agent löst den Checkpoint aus).
    #[cfg(unix)]
    #[test]
    fn an_artifact_that_is_not_a_regular_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: nul-terminierter Pfad, keine weiteren Vorbedingungen.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert_eq!(read_regular_file(&fifo, 1024, None), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(read_regular_file(Path::new("/dev/zero"), 1024, None), None);

        let file = dir.path().join("f.rs");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(read_regular_file(&file, 3, None), Some(b"abc".to_vec()));
        assert_eq!(read_regular_file(&file, 2, None), None, "über der Grenze");
    }

    /// Ein Symlink im Repo, der hinaus zeigt, liefert keinen Hash — beim
    /// Witness läge „hinaus" auf dem Host (Hash-Orakel). Einer, der im Repo
    /// bleibt, schon.
    #[cfg(unix)]
    #[test]
    fn an_artifact_symlink_out_of_the_repo_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        let outside = dir.path().join("id_ed25519");
        fs::write(&outside, b"host secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("leak.rs")).unwrap();
        fs::write(root.join("real.rs"), b"fn main() {}").unwrap();
        std::os::unix::fs::symlink(root.join("real.rs"), root.join("alias.rs")).unwrap();

        assert_eq!(read_artifact(Some(&root), "leak.rs"), None);
        // Ein harmloser Name vor einer Zugangsdatei im Repo: kein Fingerabdruck.
        fs::write(root.join(".env"), b"DB_PASSWORD=hunter2").unwrap();
        std::os::unix::fs::symlink(root.join(".env"), root.join("notes.rs")).unwrap();
        assert_eq!(read_artifact(Some(&root), "notes.rs"), None);
        fs::hard_link(root.join(".env"), root.join("linked.rs")).unwrap();
        assert_eq!(read_artifact(Some(&root), "linked.rs"), None);
        assert_eq!(
            read_artifact(Some(&root), "alias.rs"),
            Some(b"fn main() {}".to_vec())
        );
    }

    /// Die strenge Default-Policy — dieselbe, die `minds checkpoint` ohne
    /// `.minds/redact.json` verwendet.
    fn policy() -> &'static minds_redact::RedactionPipeline {
        static POLICY: std::sync::OnceLock<minds_redact::RedactionPipeline> =
            std::sync::OnceLock::new();
        POLICY.get_or_init(|| {
            minds_redact::RedactionConfig::default()
                .pipeline()
                .expect("Default-Policy muss bauen")
        })
    }

    /// Ein Checkpoint ohne Wurzel und Commit, aber mit Redaction-Policy.
    fn scanned() -> Checkpoint<'static> {
        Checkpoint {
            redaction: Some(policy()),
            ..Checkpoint::default()
        }
    }
    use minds_core::EffectKind;
    use serde_json::value::RawValue;

    fn ev(seq: u64, kind: EventKind, raw_kind: &str, at: &str, payload: &str) -> JournalEvent {
        JournalEvent {
            seq,
            at: at.into(),
            at_nanos: seq,
            kind,
            raw_kind: raw_kind.into(),
            cwd: Some("/home/anna/projects/minds".into()),
            transcript_path: None,
            payload: RawValue::from_string(payload.to_string()).unwrap(),
            payload_hash: None,
            event_hash: None,
        }
    }

    fn key() -> SessionKey {
        SessionKey::new("claude-code", "31f3f224").unwrap()
    }

    /// EA-14: Projiziert wird nur genau die Form, die der Witness schreibt
    /// — kein Hook-Event der Agent-Seite, auch nicht eines, das ein älterer
    /// Witness mit dem reservierten Namen noch angenommen hätte.
    #[test]
    fn only_witness_shaped_intent_events_are_projected() {
        use minds_core::intent_anchor::{
            INTENT_EVENT_KIND, IntentAnchor, IntentEventPayload, IntentSource, content_hash,
        };
        let anchor = IntentAnchor {
            source: IntentSource::Prompt,
            content: content_hash(b"x"),
            scope: Vec::new(),
        }
        .to_text()
        .unwrap();
        let mut payload = IntentEventPayload::new(&anchor, None).unwrap();
        payload.opens_session = true;
        let genuine = serde_json::to_string(&payload).unwrap();
        let mut hook_shaped: serde_json::Value = serde_json::from_str(&genuine).unwrap();
        hook_shaped["session_id"] = "31f3f224".into();
        hook_shaped["hook_event_name"] = INTENT_EVENT_KIND.into();
        let synthetic = |seq, payload: &str| JournalEvent {
            cwd: None,
            ..ev(seq, EventKind::Other, INTENT_EVENT_KIND, "t", payload)
        };
        let events = vec![
            // Ein Hook-Payload: trägt `session_id` — kein Witness-Event.
            synthetic(0, &hook_shaped.to_string()),
            // Richtige Form, aber mit `cwd` wie ein Hook-Event.
            ev(1, EventKind::Other, INTENT_EVENT_KIND, "t", &genuine),
            // Richtige Form, falsche Klassifikation.
            JournalEvent {
                cwd: None,
                ..ev(2, EventKind::Prompt, INTENT_EVENT_KIND, "t", &genuine)
            },
            // Id passt nicht zum Text.
            synthetic(
                3,
                &genuine.replace(
                    IntentAnchor::id_of_text(&anchor).as_str(),
                    content_hash(b"y").as_str(),
                ),
            ),
            // Die echte Form.
            synthetic(4, &genuine),
        ];
        let projected = intent_events(&events);
        assert_eq!(projected.len(), 1, "{projected:?}");
        assert_eq!(projected[0].seq, 4);
        assert!(projected[0].opens_session);
        assert_eq!(projected[0].anchor_id, IntentAnchor::id_of_text(&anchor));
    }

    #[test]
    fn a_prompt_and_a_tool_burst_become_two_turns() {
        let events = vec![
            ev(
                0,
                EventKind::SessionStart,
                "SessionStart",
                "t0",
                r#"{"session_id":"x"}"#,
            ),
            ev(
                1,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t1",
                r#"{"prompt":"fix retry"}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Read","tool_input":{"file_path":"src/retry.rs"}}"#,
            ),
            ev(
                3,
                EventKind::ToolPre,
                "PreToolUse",
                "t3",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/retry.rs"}}"#,
            ),
            ev(4, EventKind::TurnEnd, "Stop", "t4", r#"{}"#),
        ];

        let s = build_one(&key(), &events);

        assert_eq!(s.intent.request, "fix retry");
        assert_eq!(s.turns.len(), 2);
        assert_eq!(s.turns[0].role, Role::User);
        assert_eq!(s.turns[0].at.as_deref(), Some("t1"));
        assert_eq!(s.turns[1].role, Role::Assistant);
        assert_eq!(s.turns[1].at.as_deref(), Some("t2"), "erster Tool-Call");
        assert_eq!(s.turns[1].tool_calls.len(), 2);
        assert_eq!(
            s.turns[1].tool_calls[0].effect.as_ref().unwrap().kind,
            EffectKind::Read
        );
        // Nur die Schreibung zählt als produziert.
        assert_eq!(s.produced.files, vec!["src/retry.rs"]);
        // Herkunft aus den Events, ohne Uhr.
        let lin = s.lineage.unwrap();
        assert_eq!(lin.local_id, "31f3f224");
        assert_eq!(lin.started_at.as_deref(), Some("t0"));
        assert_eq!(lin.ended_at.as_deref(), Some("t4"));
        assert_eq!(lin.cwd.as_deref(), Some("/home/anna/projects/minds"));
        // Kein `SessionEnd`: Die Session läuft über diesen Bereich hinaus.
        assert!(!lin.closed);
    }

    /// `closed` heißt: Das letzte Event des Bereichs ist das `SessionEnd`
    /// des Harness. Folgt danach noch etwas (`--resume`), ist die Session
    /// hier nicht zu Ende.
    #[test]
    fn a_range_is_closed_only_when_session_end_is_its_last_event() {
        let prompt = || {
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"x"}"#,
            )
        };
        let end = |seq| ev(seq, EventKind::SessionEnd, "SessionEnd", "t1", r#"{}"#);
        let closed = |events: &[JournalEvent]| build_one(&key(), events).lineage.unwrap().closed;

        assert!(closed(&[prompt(), end(1)]));
        assert!(!closed(&[prompt()]));
        assert!(!closed(&[
            prompt(),
            end(1),
            ev(2, EventKind::SessionStart, "SessionStart", "t2", r#"{}"#),
        ]));
        // Auch ein unbekanntes Event danach hält den Bereich offen.
        assert!(!closed(&[
            prompt(),
            end(1),
            ev(2, EventKind::Other, "Notification", "t2", r#"{}"#),
        ]));
        // Maßgeblich ist der kettengebundene `raw_kind`: Ein umgeschriebenes
        // `kind` allein schließt nichts.
        assert!(!closed(&[
            prompt(),
            ev(1, EventKind::SessionEnd, "Notification", "t1", r#"{}"#),
        ]));

        // Ohne vollständigen Lesestand kein Ende.
        let mut session = build_one(&key(), &[prompt(), end(1)]);
        settle_closed(&mut session, true);
        assert!(session.lineage.as_ref().unwrap().closed);
        settle_closed(&mut session, false);
        assert!(!session.lineage.unwrap().closed);
    }

    #[test]
    fn a_stop_without_tools_still_makes_an_assistant_turn() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"hi"}"#,
            ),
            ev(1, EventKind::TurnEnd, "Stop", "t1", r#"{}"#),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(s.turns.len(), 2);
        assert_eq!(s.turns[1].role, Role::Assistant);
        assert!(s.turns[1].tool_calls.is_empty());
    }

    #[test]
    fn build_is_deterministic() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"x"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
            ),
        ];
        let a = build_one(&key(), &events);
        let b = build_one(&key(), &events);
        assert_eq!(
            minds_core::to_canonical_string(&a).unwrap(),
            minds_core::to_canonical_string(&b).unwrap()
        );
    }

    #[test]
    fn a_trailing_tool_burst_without_stop_is_still_flushed() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"x"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#,
            ),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(
            s.turns.len(),
            2,
            "der offene Assistant-Zug wird am Ende geschlossen"
        );
        assert_eq!(s.turns[1].tool_calls.len(), 1);
    }

    #[test]
    fn checkpoint_hashes_written_artifacts_but_not_secrets() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("out.rs"), b"fn main() {}").unwrap();
        fs::write(dir.path().join(".env"), b"DB_PASSWORD=hunter2").unwrap();

        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"schreib"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Write","tool_input":{"file_path":"out.rs"}}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Write","tool_input":{"file_path":".env"}}"#,
            ),
        ];

        let ctx = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        let s = checkpoint(&key(), &events, &ctx);

        let calls = &s.turns[1].tool_calls;
        let out = calls[0].effect.as_ref().unwrap();
        let env = calls[1].effect.as_ref().unwrap();
        assert!(out.content.is_some(), "das Artefakt wird gehasht");
        assert!(
            env.content.is_none(),
            "eine Zugangsdaten-Datei wird nie gehasht — sonst ein Orakel"
        );
    }

    #[test]
    fn checkpoint_adds_a_commit_edge() {
        let events = vec![ev(
            0,
            EventKind::Prompt,
            "UserPromptSubmit",
            "t0",
            r#"{"prompt":"x"}"#,
        )];
        let ctx = Checkpoint {
            root: None,
            commit: Some("deadbeefcafe"),
            tracked: None,
            redaction: Some(policy()),
        };
        let s = checkpoint(&key(), &events, &ctx);
        assert_eq!(s.edges.len(), 1);
        assert_eq!(
            s.edges[0].to,
            minds_core::Endpoint::Commit {
                id: "deadbeefcafe".into()
            }
        );
    }

    #[test]
    fn a_file_written_then_removed_via_bash_is_a_discarded_approach() {
        let events = vec![
            ev(
                0,
                EventKind::Prompt,
                "UserPromptSubmit",
                "t0",
                r#"{"prompt":"probier einen Ansatz"}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/scratch.rs"}}"#,
            ),
            ev(
                2,
                EventKind::ToolPre,
                "PreToolUse",
                "t2",
                r#"{"tool_name":"Bash","tool_input":{"command":"rm src/scratch.rs"}}"#,
            ),
        ];
        let s = build_one(&key(), &events);
        assert_eq!(
            s.intent.discarded,
            vec!["src/scratch.rs — angelegt und wieder entfernt"]
        );
        // Kein geratener Constraint.
        assert!(s.intent.constraints.is_empty());
    }

    #[test]
    fn rm_by_basename_still_matches_a_written_path() {
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Write","tool_input":{"file_path":"src/scratch.rs"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Bash","tool_input":{"command":"rm -f scratch.rs"}}"#,
            ),
        ];
        assert_eq!(build_one(&key(), &events).intent.discarded.len(), 1);
    }

    #[test]
    fn a_written_file_that_survives_is_not_discarded() {
        let events = vec![ev(
            0,
            EventKind::ToolPre,
            "PreToolUse",
            "t0",
            r#"{"tool_name":"Write","tool_input":{"file_path":"keep.rs"}}"#,
        )];
        assert!(build_one(&key(), &events).intent.discarded.is_empty());
    }

    fn write_pre(path: &str, id: &str) -> String {
        format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{path}","content":"agent\n"}},"tool_use_id":"{id}"}}"#
        )
    }

    fn write_post(path: &str, id: &str) -> String {
        format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"{path}","content":"agent\n"}},"tool_response":{{"type":"create","content":"agent\n"}},"tool_use_id":"{id}"}}"#
        )
    }

    fn first_write(session: &Session) -> &minds_core::Effect {
        session
            .turns
            .iter()
            .flat_map(|t| &t.tool_calls)
            .filter_map(|c| c.effect.as_ref())
            .find(|e| e.kind == EffectKind::Write)
            .unwrap()
    }

    #[test]
    fn written_comes_from_the_post_event_correlated_by_call_id() {
        // Zwei parallele Schreibungen, die Post-Events in umgekehrter
        // Reihenfolge: Die Kennung ordnet richtig zu, nicht die Position.
        let post_b = r#"{"tool_name":"Write","tool_input":{"file_path":"b.rs","content":"BBB"},"tool_response":{"type":"create","content":"BBB"},"tool_use_id":"tb"}"#;
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "ta"),
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Write","tool_input":{"file_path":"b.rs","content":"BBB"},"tool_use_id":"tb"}"#,
            ),
            ev(2, EventKind::ToolPost, "PostToolUse", "t2", post_b),
            ev(
                3,
                EventKind::ToolPost,
                "PostToolUse",
                "t3",
                &write_post("a.rs", "ta"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        let calls: Vec<_> = s.turns.iter().flat_map(|t| &t.tool_calls).collect();
        let a = calls[0].effect.as_ref().unwrap();
        let b = calls[1].effect.as_ref().unwrap();
        let hash = |bytes: &[u8]| ContentHash::from_bytes(*blake3::hash(bytes).as_bytes());
        assert_eq!(a.written, Some(hash(b"agent\n")));
        assert_eq!(b.written, Some(hash(b"BBB")));
        // `content` bleibt leer: Es gibt keine Platte hinter diesen Pfaden —
        // und `written` hat sie auch nie gebraucht.
        assert_eq!(a.content, None);
    }

    #[test]
    fn without_a_post_event_or_with_an_ambiguous_id_there_is_no_written() {
        // Kein Post-Event.
        let events = vec![ev(
            0,
            EventKind::ToolPre,
            "PreToolUse",
            "t0",
            &write_pre("a.rs", "ta"),
        )];
        let s = checkpoint(&key(), &events, &scanned());
        let e = first_write(&s);
        assert_eq!(e.written, None);
        assert_eq!(
            e.written_unavailable,
            Some(WrittenUnavailable::PayloadWithoutContent)
        );

        // Zwei Post-Events mit derselben Kennung: keine Zuordnung statt einer
        // geratenen.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "dup"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post("a.rs", "dup"),
            ),
            ev(
                2,
                EventKind::ToolPost,
                "PostToolUse",
                "t2",
                &write_post("a.rs", "dup"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        assert_eq!(
            first_write(&s).written_unavailable,
            Some(WrittenUnavailable::PayloadWithoutContent)
        );

        // Ohne Kennung im Pre-Event ebenso — die Reihenfolge zaehlt nicht.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs","content":"x"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post("a.rs", "ta"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        assert_eq!(first_write(&s).written, None);
    }

    #[test]
    fn written_is_only_derived_for_write_effects_and_only_by_an_adapter() {
        // Ein Read traegt weder Hash noch Grund.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"},"tool_use_id":"r"}"#,
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"},"tool_response":{"file":{"content":"x"}},"tool_use_id":"r"}"#,
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        let e = s.turns[0].tool_calls[0].effect.as_ref().unwrap();
        assert_eq!((e.written.as_ref(), e.written_unavailable), (None, None));

        // Ein Agent ohne Adapter: ungedeutet, also auch kein `written`.
        let other = SessionKey::new("some-future-agent", "x1").unwrap();
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "ta"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post("a.rs", "ta"),
            ),
        ];
        let s = checkpoint(&other, &events, &scanned());
        assert!(s.turns[0].tool_calls[0].effect.is_none());
    }

    #[test]
    fn the_secret_wall_and_the_repo_boundary_come_before_the_payload() {
        // Fuer eine Secret-Datei wird der Payload nicht angesehen — selbst
        // wenn er (hier ungewallt konstruiert) den Inhalt traegt.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre(".env", "s"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post(".env", "s"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        let e = first_write(&s);
        assert_eq!(e.written, None);
        assert_eq!(e.written_unavailable, Some(WrittenUnavailable::SecretFile));

        // Absolut ohne Wurzel: draussen. Mit passender Wurzel: drinnen.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("/repo/a.rs", "o"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post("/repo/a.rs", "o"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        assert_eq!(
            first_write(&s).written_unavailable,
            Some(WrittenUnavailable::OutsideRepo)
        );
        let ctx = Checkpoint {
            root: Some(Path::new("/repo")),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        assert!(
            first_write(&checkpoint(&key(), &events, &ctx))
                .written
                .is_some()
        );
        // Eine andere Wurzel: draussen. Auch `/repo2` ist nicht `/repo`.
        let ctx = Checkpoint {
            root: Some(Path::new("/repo2")),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        assert_eq!(
            first_write(&checkpoint(&key(), &events, &ctx)).written_unavailable,
            Some(WrittenUnavailable::OutsideRepo)
        );
    }

    #[test]
    fn inside_repo_is_lexical_and_fail_closed() {
        let root: &[&Path] = &[Path::new("/repo")];
        assert!(inside_repo("src/a.rs", &[]));
        assert!(inside_repo("src/a.rs", root));
        assert!(!inside_repo("../a.rs", root));
        assert!(!inside_repo("src/../../a.rs", root));
        assert!(inside_repo("/repo/src/a.rs", root));
        assert!(!inside_repo("/repo/../a.rs", root));
        assert!(!inside_repo("/repo2/a.rs", root));
        assert!(!inside_repo("/elsewhere/a.rs", root));
        assert!(!inside_repo("/repo/src/a.rs", &[]));
        // Nur technisch relativ: Shell-Expansion und Windows-Pfade.
        assert!(!inside_repo("~/notes.txt", root));
        assert!(!inside_repo("$HOME/notes.txt", root));
        assert!(!inside_repo("C:\\Users\\anna\\privat.txt", root));
    }

    #[test]
    fn a_symlinked_root_still_counts_as_inside() {
        // Die Wurzel kommt roh (z. B. ueber einen Symlink), der Agent nennt
        // den aufgeloesten Pfad. Das ist drinnen.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        #[cfg(unix)]
        let link = {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            link
        };
        #[cfg(not(unix))]
        let link = real.clone();
        let file = fs::canonicalize(&real).unwrap().join("a.rs");
        let path = file.to_str().unwrap();

        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre(path, "s"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post(path, "s"),
            ),
        ];
        let ctx = Checkpoint {
            root: Some(&link),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        let s = checkpoint(&key(), &events, &ctx);
        assert!(
            first_write(&s).written.is_some(),
            "{:?}",
            first_write(&s).written_unavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_path_under_a_resolved_root_still_counts_as_inside() {
        // Die Gegenrichtung: Der Agent nennt den Pfad ueber den Symlink
        // (`cwd` aus der Shell, macOS `/var/folders/…`), die Wurzel liegt
        // aufgeloest vor (`/private/var/folders/…`). Auch das ist drinnen.
        let dir = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(dir.path()).unwrap().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let path = link.join("a.rs");
        let path = path.to_str().unwrap();

        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre(path, "s"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post(path, "s"),
            ),
        ];
        let ctx = Checkpoint {
            root: Some(&real),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        let s = checkpoint(&key(), &events, &ctx);
        assert!(
            first_write(&s).written.is_some(),
            "{:?}",
            first_write(&s).written_unavailable
        );

        // Ein Symlink-Verzeichnis, das nach draussen zeigt, bleibt draussen;
        // ebenso ein Verzeichnis, das es nicht gibt, und `..` im Rohpfad.
        let outside = tempfile::tempdir().unwrap();
        let escape = real.join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape).unwrap();
        let roots: &[&Path] = &[&real];
        let escaped = link.join("escape").join("x.rs");
        assert!(!inside_repo_resolved(escaped.to_str().unwrap(), roots));
        let missing = link.join("missing").join("x.rs");
        assert!(!inside_repo_resolved(missing.to_str().unwrap(), roots));
        let dotted = format!("{}/../link/a.rs", link.display());
        assert!(!inside_repo_resolved(&dotted, roots));
    }

    #[test]
    fn a_failed_write_gets_no_written_hash_at_checkpoint() {
        // Der ganze Weg: Pre + PostToolUseFailure, korreliert ueber dieselbe
        // Kennung ⇒ kein Hash, der Grund steht dabei.
        let failure = r#"{"tool_name":"Write","tool_input":{"file_path":"a.rs","content":"agent\n"},"tool_use_id":"f","error":"File has not been read yet"}"#;
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "f"),
            ),
            ev(1, EventKind::ToolPost, "PostToolUseFailure", "t1", failure),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        let e = first_write(&s);
        assert_eq!(e.written, None);
        assert_eq!(
            e.written_unavailable,
            Some(WrittenUnavailable::PayloadWithoutContent)
        );
    }

    #[test]
    fn two_pre_events_with_one_id_and_a_post_on_another_path_get_nothing() {
        // Doppelte Pre-Kennung: keiner der beiden Aufrufe bekommt das Post.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "dup"),
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                &write_pre("b.rs", "dup"),
            ),
            ev(
                2,
                EventKind::ToolPost,
                "PostToolUse",
                "t2",
                &write_post("a.rs", "dup"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        for call in s.turns.iter().flat_map(|t| &t.tool_calls) {
            let e = call.effect.as_ref().unwrap();
            assert_eq!(e.written, None, "{:?}", e.path);
            assert_eq!(
                e.written_unavailable,
                Some(WrittenUnavailable::PayloadWithoutContent)
            );
        }
        // Gleiche Kennung, aber das Post nennt einen anderen Pfad: Die
        // Gegenprobe schlaegt fehl, kein Hash.
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                &write_pre("a.rs", "x"),
            ),
            ev(
                1,
                EventKind::ToolPost,
                "PostToolUse",
                "t1",
                &write_post("b.rs", "x"),
            ),
        ];
        let s = checkpoint(&key(), &events, &scanned());
        assert_eq!(first_write(&s).written, None);
    }

    #[test]
    fn a_read_effect_is_hashed_but_a_secret_read_never() {
        // Seit Phase 6 traegt auch ein Read-Effekt den Inhalts-Hash — das
        // Beweismittel fuer Content-Uebergaben. Die Orakel-Regel gilt
        // unveraendert: Secret-Dateien bekommen nie einen Hash.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("read.rs"), b"content").unwrap();
        fs::write(dir.path().join(".env"), b"SECRET=x").unwrap();
        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Read","tool_input":{"file_path":"read.rs"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                r#"{"tool_name":"Read","tool_input":{"file_path":".env"}}"#,
            ),
        ];
        // Die Read-Grenze: nur getrackte Pfade bekommen einen Lese-Hash.
        let tracked: std::collections::BTreeSet<String> = ["read.rs".to_string()].into();
        let ctx = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: Some(&tracked),
            redaction: Some(policy()),
        };
        let s = checkpoint(&key(), &events, &ctx);
        let calls: Vec<_> = s.turns.iter().flat_map(|t| t.tool_calls.iter()).collect();
        let read = calls
            .iter()
            .find(|c| c.effect.as_ref().and_then(|e| e.path.as_deref()) == Some("read.rs"))
            .unwrap();
        let expected = ContentHash::from_bytes(*blake3::hash(b"content").as_bytes());
        assert_eq!(
            read.effect.as_ref().unwrap().content.as_ref(),
            Some(&expected)
        );
        // Ohne tracked-Set (Grenze unbekannt) entsteht fuer Reads NIE ein
        // Hash — fail-closed in Richtung „weniger Fingerabdruck".
        let ctx_unbounded = Checkpoint {
            root: Some(dir.path()),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        let s2 = checkpoint(&key(), &events, &ctx_unbounded);
        let read2 = s2
            .turns
            .iter()
            .flat_map(|t| t.tool_calls.iter())
            .find(|c| c.effect.as_ref().and_then(|e| e.path.as_deref()) == Some("read.rs"))
            .unwrap();
        assert!(read2.effect.as_ref().unwrap().content.is_none());

        // Die Secretwall hat den .env-Aufruf schon auf dem heissen Pfad
        // gewallt — hier darf so oder so nie ein Hash entstehen.
        for call in &calls {
            if let Some(effect) = &call.effect {
                if effect.path.as_deref() == Some(".env") {
                    assert!(effect.content.is_none(), "Hash-Orakel ueber Secret-Datei");
                }
            }
        }
    }

    #[test]
    fn a_write_effect_that_escapes_the_repo_is_never_hashed() {
        // `path` ist Text, den der Agent selbst waehlt — bei Claudes
        // `file_path`-Feld ebenso wie bei Codex' Diff-Kopfzeile. Ein
        // absoluter oder ueber `..` fluechtender Pfad darf nie zu einem
        // Content-Hash fuehren, sonst waere er ein Orakel ueber beliebige
        // Dateien der Maschine.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("outside.txt"), b"secret machine content").unwrap();

        let events = vec![
            ev(
                0,
                EventKind::ToolPre,
                "PreToolUse",
                "t0",
                r#"{"tool_name":"Write","tool_input":{"file_path":"../outside.txt"}}"#,
            ),
            ev(
                1,
                EventKind::ToolPre,
                "PreToolUse",
                "t1",
                &format!(
                    r#"{{"tool_name":"Write","tool_input":{{"file_path":"{}"}}}}"#,
                    dir.path().join("outside.txt").display()
                ),
            ),
        ];
        let ctx = Checkpoint {
            root: Some(&dir.path().join("repo")),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        std::fs::create_dir_all(dir.path().join("repo")).unwrap();
        let s = checkpoint(&key(), &events, &ctx);
        for call in s.turns.iter().flat_map(|t| &t.tool_calls) {
            let effect = call.effect.as_ref().unwrap();
            assert!(
                effect.content.is_none(),
                "Pfad-Flucht darf nie gehasht werden: {:?}",
                effect.path
            );
        }
    }

    #[test]
    fn a_codex_apply_patch_diff_pointing_outside_the_repo_is_never_hashed() {
        // Derselbe Fall ueber den Codex-Adapter: Der Pfad kommt aus einer
        // freien Diff-Kopfzeile, nicht aus einem strukturierten Feld — noch
        // weniger Grund, ihr zu vertrauen.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repo")).unwrap();
        fs::write(dir.path().join("outside.txt"), b"secret machine content").unwrap();

        let diff = "--- a/x\n+++ b/../outside.txt\n@@ ...\n";
        let events = vec![ev(
            0,
            EventKind::ToolPre,
            "PreToolUse",
            "t0",
            &format!(r#"{{"tool_name":"apply_patch","tool_input":{{"diff":"{diff}"}}}}"#)
                .replace('\n', "\\n"),
        )];
        let key = SessionKey::new("codex", key().local_id()).unwrap();
        let ctx = Checkpoint {
            root: Some(&dir.path().join("repo")),
            commit: None,
            tracked: None,
            redaction: Some(policy()),
        };
        let s = checkpoint(&key, &events, &ctx);
        let effect = s.turns[0].tool_calls[0].effect.as_ref().unwrap();
        assert_eq!(effect.path.as_deref(), Some("../outside.txt"));
        assert!(
            effect.content.is_none(),
            "Diff-basierte Pfad-Flucht darf nie gehasht werden"
        );
    }
}
