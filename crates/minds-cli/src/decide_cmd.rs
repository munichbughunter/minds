//! `minds decide` und `minds decisions` — Entscheidungen als Git-Objekte.
//!
//! Das Paar folgt `review`/`reviews`: Ein Verb schreibt, das Substantiv
//! liest. Mehr Kommandos braucht es nicht — `brief`, `recall`, `distill` und
//! die TUI (`inspect`) zeigen Entscheidungen dort mit, wo sie hingehören.
//!
//! # `decide`
//!
//! Liest einen Record als JSON von stdin (`--template` druckt die Form),
//! setzt Art, Schema und Zeit, prüft ihn, redigiert den Freitext
//! (`minds_redact::RedactionPipeline::redact_decision`), legt ihn unter
//! `refs/minds/decisions/<id>` ab und druckt die Id — die Zeile
//! `decision b3-…`, die der Checkpoint aus der Tool-Antwort liest, um die
//! Session mit dem Record zu verknüpfen. Die Id ist die der **redigierten**
//! Bytes (`check → redact → id → store`).
//!
//! # `decisions`
//!
//! Liest alle Records, wertet die Proben gegen den **Index** aus
//! ([`minds_reader::decisions::IndexTree`]) und zeigt den Stand je
//! Entscheidung: offen, neu zu bewerten, anzusehen, ungeprüft, in Kraft.
//! Mit einem Pfad nur die, deren Scope ihn betrifft; mit einer Id eine
//! einzelne im Detail; `--check` macht den Stand zum Exit-Code für CI.

use std::io::Read;
use std::process::ExitCode;
use std::time::Instant;

use minds_core::ContentHash;
use minds_core::decision::{DECISION_KIND, DECISION_SCHEMA, DecisionRecord, MAX_RECORD, Probe};
use minds_reader::decisions::{
    Assessment, IndexTree, ProbeResult, Standing, Worktree, assess_all, concerns, concerns_any,
    named_within_call, validate_probe,
};

use crate::context::Context;
use crate::text::sanitize;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Exit-Code von `decisions --check`, wenn eine Entscheidung neu zu
/// bewerten, anzusehen oder ungeprüft ist — wie die Gates von `minds
/// verify`: Was das Gate nicht bewerten konnte, lässt es nicht passieren.
const EXIT_RECHECK: u8 = 2;

/// So viele Records liest ein Lauf höchstens (die Spitzen der jüngsten
/// bekommen die Proben, [`minds_reader::decisions::MAX_ASSESSED`]).
const MAX_READ: usize = 1024;

/// So viele Index-Dateien unter einem Verzeichnis-Argument werden je
/// Record gegen den Scope abgeglichen, bevor der Record als betroffen gilt.
const MAX_DIR_FILES: usize = 2048;

/// Ein Pfad-Argument, normalisiert: `./` vorn und `/` hinten abgestreift,
/// so oft es geht; leer und `.` heißen „kein Pfad" (`Ok(None)`). Was dann
/// nicht repo-relativ ist (`..`, `/abs`, `a//b`, `src/.`), ist ein Fehler —
/// ein stilles „no decision concerns …" wäre bei `--check` ein grünes Gate
/// auf einem Tippfehler.
fn path_arg(target: &str) -> Result<Option<String>, &'static str> {
    let mut path = target;
    loop {
        let before = path;
        path = path.strip_prefix("./").unwrap_or(path);
        path = path.strip_suffix('/').unwrap_or(path);
        if path == before {
            break;
        }
    }
    if path.is_empty() || path == "." {
        return Ok(None);
    }
    if !minds_core::decision::is_repo_relative(path) {
        return Err("the path is not repo-relative (no `..`, no leading `/`, no `//`)");
    }
    Ok(Some(path.to_owned()))
}

/// Die Form, die `--template` druckt: das Beispiel aus dem Entwurf.
const TEMPLATE: &str = r#"{
  "topic": "lock for the witness socket on macOS",
  "chosen": {
    "approach": "flock on a lock file",
    "rests_on": [{"assumption": "local-fs", "weight": "load-bearing"}]
  },
  "alternatives": [
    {
      "approach": "Unix socket as the lock",
      "rejected_because": [
        {"assumption": "fork", "weight": "load-bearing"},
        {"assumption": "no-windows", "weight": "marginal"}
      ]
    }
  ],
  "assumptions": [
    {"id": "local-fs", "statement": "the lock file lives on a local file system"},
    {"id": "fork", "statement": "daemon.rs forks",
     "probe": {"type": "pattern", "path": "src/daemon.rs", "must_match": "\\bfork\\("}},
    {"id": "no-windows", "statement": "no Windows support is needed",
     "probe": {"type": "pattern", "glob": "crates/*/Cargo.toml", "must_not_match": "windows-sys"}}
  ],
  "scope": ["src/**"]
}
"#;

/// Führt `minds decide` aus.
pub fn run_decide(template: bool, human: bool, supersedes: Option<&str>) -> ExitCode {
    if template {
        print!("{TEMPLATE}");
        return ExitCode::SUCCESS;
    }
    match decide(human, supersedes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds decide: {}", sanitize(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

fn decide(human: bool, supersedes: Option<&str>) -> Fallible<()> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("reading the decision from stdin (minds decide --template prints the form)");
    }
    let mut raw = Vec::new();
    std::io::stdin()
        .take(MAX_RECORD as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_RECORD {
        return Err(format!("the record is larger than {MAX_RECORD} bytes").into());
    }
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Err(
            "expected the decision as JSON on stdin (minds decide --template prints the form)"
                .into(),
        );
    }
    let mut value: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|err| format!("not valid JSON: {}", err.classify_word()))?;
    let Some(object) = value.as_object_mut() else {
        return Err("the record must be a JSON object".into());
    };
    // Art, Schema und Zeit setzt das Kommando — nie der Aufrufer: Die Zeit
    // ist die des Ablegens, kein Freitext.
    object.insert("kind".into(), DECISION_KIND.into());
    object.insert("schema".into(), DECISION_SCHEMA.into());
    object.insert("at".into(), minds_capture::clock::now().0.into());
    // Wer entschieden hat, sagt das Flag, nicht das JSON: Ein Agent, der
    // `"decided_by":"human"` schreibt, bliebe sonst ununterscheidbar.
    object.insert(
        "decided_by".into(),
        if human { "human" } else { "agent" }.into(),
    );
    // Die Ablösung kommt vom Flag, nie aus dem JSON — wie Art, Schema und
    // Zeit: Was das Kommando setzt, setzt nur das Kommando.
    object.remove("supersedes");
    if let Some(old) = supersedes {
        let old: ContentHash = old
            .parse()
            .map_err(|_| "--supersedes takes a record id (b3-<64 hex>)")?;
        object.insert("supersedes".into(), old.as_str().into());
    }
    let record: DecisionRecord = serde_json::from_value(value)
        .map_err(|err| format!("not a decision record: {}", err.classify_word()))?;
    // Jeder Fehler der Prüfung nennt eine Position, nie einen Wert aus
    // der Eingabe — die Meldung landet in der Tool-Antwort des Agenten.
    record.check()?;
    for (i, assumption) in record.assumptions.iter().enumerate() {
        if let Some(probe) = &assumption.probe
            && let Err(why) = validate_probe(probe)
        {
            return Err(format!(
                "the probe of assumptions[{i}]: {why} — the pattern would never be checked"
            )
            .into());
        }
    }

    let ctx = Context::open()?;
    if let Some(old) = &record.supersedes {
        if ctx.store.get_decision(old)?.is_none() {
            return Err(format!("--supersedes {old}: no such decision in this store").into());
        }
    }
    let pipeline = crate::config::load_redaction(&ctx.root)?.pipeline()?;
    let redacted = pipeline.redact_decision(record)?;
    let id = ctx.store.put_decision(&redacted)?;
    let record = redacted.record();

    // Die erste Zeile ist der Vertrag mit dem Checkpoint: `decision <id>`.
    println!("decision {id}");
    println!("  topic       {}", sanitize(&record.topic));
    match &record.chosen {
        Some(chosen) => println!("  chosen      {}", sanitize(&chosen.approach)),
        None => println!(
            "  open        {} option{} for a human",
            record.alternatives.len(),
            plural(record.alternatives.len())
        ),
    }
    if record.chosen.is_some() && !record.alternatives.is_empty() {
        println!(
            "  rejected    {}",
            record
                .alternatives
                .iter()
                .map(|a| sanitize(&a.approach))
                .collect::<Vec<_>>()
                .join(" · ")
        );
    }
    let probed = record
        .assumptions
        .iter()
        .filter(|a| a.probe.is_some())
        .count();
    println!(
        "  assumptions {} ({probed} with a probe)",
        record.assumptions.len()
    );
    if !record.scope.is_empty() {
        println!(
            "  scope       {}",
            record
                .scope
                .iter()
                .map(|g| sanitize(g))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(old) = &record.supersedes {
        println!("  supersedes  {old}");
    }
    println!(
        "  by          {} · {}",
        minds_reader::decisions::by_word(record.decided_by),
        sanitize(&record.at)
    );
    let counts = redacted.audit().counts();
    if counts.secrets > 0 || counts.pii > 0 {
        crate::text::warn(&format!(
            "the redaction policy replaced {} secret(s) and {} PII in the free text \
             (topic, statements, approaches); probes and scope are unchanged",
            counts.secrets, counts.pii
        ));
    }
    Ok(())
}

/// Führt `minds decisions` aus.
pub fn run_decisions(target: Option<&str>, check: bool) -> ExitCode {
    match decisions(target, check) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("minds decisions: {}", sanitize(&err.to_string()));
            ExitCode::FAILURE
        }
    }
}

/// Alle Entscheidungen des Stores, gegen den Index bewertet — für
/// `decisions`, `brief`, `recall`, `distill` und die TUI dieselbe Rechnung.
pub(crate) struct Assessed {
    /// Die Bewertungen, sortiert wie [`assess_all`] sie liefert.
    pub all: Vec<Assessment>,
    /// Records, die sich nicht lesen ließen: Id und Grund — genannt, nicht
    /// nur gezählt, denn ein veränderter Record ist ein Befund.
    pub unreadable: Vec<(ContentHash, &'static str)>,
    /// Refs im Namensraum ohne lesbaren Commit oder ohne Id als Namen.
    pub skipped: usize,
    /// Ob der Lauf nicht jeden Record gesehen hat: der Store hat mehr
    /// gelistet, als gelesen werden ([`MAX_READ`]), oder selbst gedeckelt.
    pub truncated: bool,
    /// Die Dateien des Index — damit ein Verzeichnis als Pfad die
    /// Entscheidungen trifft, deren Scope eine Datei darunter betrifft.
    files: Vec<String>,
    /// Wie lange die Proben liefen.
    pub elapsed: std::time::Duration,
}

impl Assessed {
    /// Liest und bewertet. Ein unlesbarer Index ist ein Fehler: Ohne ihn
    /// liefe keine Probe, und „in Kraft" ohne Probe wäre eine Lüge — darum
    /// wird er geöffnet, bevor ein Record gelesen wird.
    pub(crate) fn load(ctx: &Context) -> Fallible<Self> {
        let tree = IndexTree::open(&ctx.repo)
            .map_err(|why| format!("the index is not readable ({why}) — probes cannot run"))?;
        let files = tree
            .files()
            .map_err(|why| format!("the index is not readable ({why}) — probes cannot run"))?;
        let listing = ctx.store.list_decisions()?;
        // Mehr als MAX_READ wird nicht gelesen — und das Gate weiß es.
        let truncated = listing.more || listing.ids.len() > MAX_READ;
        let mut records: Vec<(ContentHash, DecisionRecord)> = Vec::new();
        let mut unreadable: Vec<(ContentHash, &'static str)> = Vec::new();
        // Die ersten MAX_READ in Ref-Reihenfolge (Hex) — welche das sind,
        // ist Zufall; darum schließt `truncated` das Gate, und die Anzeige
        // sagt, dass nicht alles gelesen wurde.
        for id in listing.ids.into_iter().take(MAX_READ) {
            use minds_store::StoreError;
            match ctx.store.get_decision(&id) {
                Ok(Some((record, _))) => records.push((id, record)),
                // Ein Ref im Namensraum ohne lesbaren Record, oder ein
                // beschädigter: Er nimmt den anderen nicht die Anzeige —
                // er wird genannt, mit Grund, nie mit Inhalt.
                Ok(None) => unreadable.push((id, "no record under its ref")),
                Err(StoreError::DecisionMismatch { .. }) => {
                    unreadable.push((id, "content does not hash to its id — altered"));
                }
                Err(StoreError::MalformedDecision { .. }) => {
                    unreadable.push((id, "not a readable decision record"));
                }
                Err(_) => unreadable.push((id, "not readable")),
            }
        }
        let started = Instant::now();
        let all = assess_all(records, &tree);
        Ok(Self {
            all,
            unreadable,
            skipped: listing.skipped,
            truncated,
            files,
            elapsed: started.elapsed(),
        })
    }

    /// Wie viele Records fehlen: unlesbar oder übersprungen.
    pub(crate) fn missing(&self) -> usize {
        self.unreadable.len() + self.skipped
    }

    /// Ob der Lauf nicht alles gesehen hat — dann bleibt ein Gate zu, auch
    /// ohne Treffer: Der fehlende Record könnte der sein, der zählt.
    pub(crate) fn incomplete(&self) -> bool {
        self.missing() > 0 || self.truncated
    }

    /// Nennt auf stderr, was fehlt — Id und Grund, nie Inhalt.
    pub(crate) fn report_missing(&self, program: &str) {
        for (id, why) in &self.unreadable {
            eprintln!("{program}: decision {id} {why}");
        }
        if self.skipped > 0 {
            eprintln!(
                "{program}: {} ref{} under refs/minds/decisions/ without a readable record",
                self.skipped,
                plural(self.skipped)
            );
        }
        if self.truncated {
            eprintln!(
                "{program}: not every record was read — more than {MAX_READ} in the store, or the listing was cut"
            );
        }
    }

    /// Ein Pfad-Argument, einmal vorbereitet: der normalisierte Pfad und
    /// die Index-Dateien darunter — damit nicht jeder Record die Dateiliste
    /// erneut durchläuft. Ein Verzeichnis trifft, wenn eine Datei darunter
    /// trifft — `minds decisions src` soll finden, was `src/**` betrifft;
    /// `.` und `./` sind das ganze Repo. Mehr als [`MAX_DIR_FILES`] Dateien
    /// darunter werden nicht abgeglichen: dann gilt jeder Record als
    /// betroffen — lieber einer zu viel als ein Gate, das zu lange rechnet
    /// oder zu wenig zeigt. Kein Pfad oder keiner, der repo-relativ ist:
    /// alle (`decisions` selbst lehnt den Pfad vorher ab).
    pub(crate) fn query(&self, path: &str) -> PathQuery<'_> {
        let path = match path_arg(path) {
            Ok(Some(path)) => path,
            Ok(None) | Err(_) => return PathQuery::All,
        };
        let prefix = format!("{path}/");
        // Der Index ist nach Pfad sortiert: der Bereich unter dem Präfix.
        let from = self.files.partition_point(|f| f.as_str() < prefix.as_str());
        let under: Vec<&str> = self.files[from..]
            .iter()
            .take_while(|f| f.starts_with(&prefix))
            .take(MAX_DIR_FILES + 1)
            .map(String::as_str)
            .collect();
        if under.len() > MAX_DIR_FILES {
            return PathQuery::All;
        }
        PathQuery::Under { path, under }
    }

    /// Die Spitzen, die einen der Pfade betreffen — oder, ohne Pfade, alle
    /// Spitzen.
    pub(crate) fn concerning<'a>(&'a self, paths: &[String]) -> Vec<&'a Assessment> {
        let queries: Vec<PathQuery<'_>> = paths.iter().map(|p| self.query(p)).collect();
        self.all
            .iter()
            .filter(|a| a.is_head())
            .filter(|a| queries.is_empty() || queries.iter().any(|q| q.concerns(&a.record)))
            .collect()
    }

    /// Die Spitzen, die eine Session nennt (`decision_events`) — auch wenn
    /// der genannte Record inzwischen abgelöst ist: dann seine Nachfolgerin
    /// — oder deren Scope einen der Pfade betrifft.
    pub(crate) fn for_sessions<'a>(
        &'a self,
        sessions: &[minds_core::Session],
        paths: &[String],
    ) -> Vec<&'a Assessment> {
        // Nur Nennungen, die zu ihrem Aufruf passen: Der Record entstand im
        // Zeitfenster des `minds decide`-Aufrufs — eine fremde Id, die ein
        // Kommando daneben druckte, fällt hier heraus. Eine Id ohne Record
        // ebenso — auch über dem Deckel (`truncated`): Es zählt nur, was
        // gelesen ist; wie viele Nennungen so wegfielen, sagt niemand
        // (Folge-Arbeit: in `report_missing` zählen).
        let named: std::collections::BTreeSet<ContentHash> = sessions
            .iter()
            .flat_map(|s| {
                s.decision_events.iter().filter(move |e| {
                    self.all
                        .iter()
                        .find(|a| a.id == e.decision_id)
                        .is_some_and(|a| named_within_call(s, e, &a.record.at))
                })
            })
            .map(|e| self.head_of(e.decision_id.clone()))
            .collect();
        let queries: Vec<PathQuery<'_>> = paths.iter().map(|p| self.query(p)).collect();
        self.all
            .iter()
            .filter(|a| a.is_head())
            .filter(|a| named.contains(&a.id) || queries.iter().any(|q| q.concerns(&a.record)))
            .collect()
    }

    /// Die Spitze der Kette, in der `id` steht.
    fn head_of(&self, id: ContentHash) -> ContentHash {
        let mut at = id;
        for _ in 0..self.all.len() {
            match self
                .all
                .iter()
                .find(|a| a.id == at)
                .and_then(|a| a.superseded_by.clone())
            {
                Some(next) => at = next,
                None => break,
            }
        }
        at
    }
}

/// Ein vorbereitetes Pfad-Argument (siehe [`Assessed::query`]).
pub(crate) enum PathQuery<'a> {
    /// Kein Pfad, oder einer, den der Abgleich nicht eingrenzt: alle.
    All,
    /// Ein Pfad und die Index-Dateien darunter.
    Under { path: String, under: Vec<&'a str> },
}

impl PathQuery<'_> {
    /// Ob der Scope des Records den Pfad betrifft.
    pub(crate) fn concerns(&self, record: &DecisionRecord) -> bool {
        match self {
            PathQuery::All => true,
            PathQuery::Under { path, under } => {
                concerns(record, path) || concerns_any(record, under.iter().copied())
            }
        }
    }
}

fn decisions(target: Option<&str>, check: bool) -> Fallible<ExitCode> {
    let ctx = Context::open()?;
    let assessed = Assessed::load(&ctx)?;
    // Was fehlt, zuerst und nach stderr: Id und Grund.
    assessed.report_missing("minds decisions");
    let all = &assessed.all;
    let elapsed = assessed.elapsed;

    // Eine Id (oder ein eindeutiger Anfang davon) zeigt das Detail.
    match target.map(|t| find(all, t)) {
        Some(Lookup::One(found)) => {
            print_detail(found, all);
            print_missing(&assessed);
            // Das Gate gilt der Spitze der Kette, nicht dem historischen
            // Stand eines abgelösten Records — und bleibt zu, wenn Records
            // fehlen: Der fehlende könnte diese Entscheidung ablösen.
            let head = assessed.head_of(found.id.clone());
            let standing = all
                .iter()
                .find(|a| a.id == head)
                .map_or(found.standing, |a| a.standing);
            return Ok(if check && (gated(standing) || assessed.incomplete()) {
                ExitCode::from(EXIT_RECHECK)
            } else {
                ExitCode::SUCCESS
            });
        }
        Some(Lookup::Ambiguous(n)) => {
            return Err(
                format!("the id prefix matches {n} decisions — give more characters").into(),
            );
        }
        Some(Lookup::NoMatch) => {
            return Err("no decision with that id in this store".into());
        }
        Some(Lookup::TooShort) => {
            return Err("give at least 8 hex characters of the id".into());
        }
        Some(Lookup::NotAnId) | None => {}
    }
    let path = target.map(path_arg).transpose()?.flatten();
    let query = path.as_deref().map(|p| assessed.query(p));
    let shown: Vec<&Assessment> = all
        .iter()
        .filter(|a| query.as_ref().is_none_or(|q| q.concerns(&a.record)))
        .collect();
    let heads: Vec<&Assessment> = shown.iter().copied().filter(|a| a.is_head()).collect();

    if shown.is_empty() {
        match &path {
            Some(p) => println!("no decision concerns {}", sanitize(p)),
            None => println!("no decisions recorded — minds decide --template prints the form"),
        }
        print_missing(&assessed);
        return Ok(if check && assessed.incomplete() {
            ExitCode::from(EXIT_RECHECK)
        } else {
            ExitCode::SUCCESS
        });
    }

    let count = |standing: Standing| heads.iter().filter(|a| a.standing == standing).count();
    let mut summary = format!("{} decision{}", heads.len(), plural(heads.len()));
    for (n, word) in [
        (count(Standing::Open), "open"),
        (count(Standing::Review), "to re-evaluate"),
        (count(Standing::ReCheck), "to re-check"),
        (count(Standing::Unchecked), "unchecked"),
        (count(Standing::InForce), "in force"),
    ] {
        if n > 0 {
            summary.push_str(&format!(" · {n} {word}"));
        }
    }
    if let Some(p) = &path {
        summary.push_str(&format!(" concerning {}", sanitize(p)));
    }
    println!("{summary}");
    let width = heads
        .iter()
        .map(|a| a.topic.chars().count().min(48))
        .max()
        .unwrap_or(0);
    for a in &heads {
        println!(
            "{} {}  {:<width$}  {}",
            glyph(a.standing),
            short(&a.id),
            clip(&a.topic, 48),
            a.headline(),
            width = width
        );
    }
    let superseded = shown.len() - heads.len();
    if superseded > 0 {
        println!("{superseded} superseded (history: minds decisions <id>)");
    }
    print_missing(&assessed);
    // Die Laufzeit nach stderr: stdout bleibt byte-deterministisch.
    eprintln!(
        "probes ran against the index (staged content) in {:.1} s",
        elapsed.as_secs_f64()
    );

    let hits = heads.iter().filter(|a| gated(a.standing)).count();
    if check && (hits > 0 || assessed.incomplete()) {
        let missing = if assessed.incomplete() {
            ", records missing"
        } else {
            ""
        };
        println!("{hits} to re-evaluate, re-check or unchecked{missing} — exit {EXIT_RECHECK}");
        return Ok(ExitCode::from(EXIT_RECHECK));
    }
    Ok(ExitCode::SUCCESS)
}

/// Was fehlt, auf stdout als Zahl — die Ids stehen auf stderr.
fn print_missing(assessed: &Assessed) {
    let missing = assessed.missing();
    if missing > 0 {
        println!(
            "{missing} record{} not readable — named on stderr",
            plural(missing)
        );
    }
    if assessed.truncated {
        println!("not every record read — more than {MAX_READ} in the store");
    }
}

/// Was `--check` nicht passieren lässt: gefallen, verändert — und
/// ungeprüft, denn ein Symlink an der Stelle der Datei, ein Konflikt oder
/// ein überfüllter Store dürfen das Gate nicht öffnen.
fn gated(standing: Standing) -> bool {
    matches!(
        standing,
        Standing::Review | Standing::ReCheck | Standing::Unchecked
    )
}

/// Was ein Ziel-Argument ist.
enum Lookup<'a> {
    /// Genau eine Entscheidung.
    One(&'a Assessment),
    /// Ein Id-Anfang, der mehrere trifft.
    Ambiguous(usize),
    /// Sieht aus wie eine Id (volle Länge oder `b3-`), trifft aber keine.
    NoMatch,
    /// `b3-` mit weniger als 8 Hex-Zeichen — zu kurz, um eine zu meinen.
    TooShort,
    /// Keine Id — ein Pfad.
    NotAnId,
}

/// Die Entscheidung zu einer Id oder einem Anfang davon (mindestens 8
/// Hex-Zeichen, mit oder ohne `b3-`).
fn find<'a>(all: &'a [Assessment], target: &str) -> Lookup<'a> {
    let hex = target.strip_prefix("b3-").unwrap_or(target);
    if target.starts_with("b3-") && hex.len() < 8 {
        return Lookup::TooShort;
    }
    if hex.len() < 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Lookup::NotAnId;
    }
    let hex = hex.to_ascii_lowercase();
    let matches: Vec<&Assessment> = all
        .iter()
        .filter(|a| a.id.hex().starts_with(&hex))
        .collect();
    match matches.as_slice() {
        [] if hex.len() == 64 || target.starts_with("b3-") => Lookup::NoMatch,
        [] => Lookup::NotAnId,
        [one] => Lookup::One(one),
        many => Lookup::Ambiguous(many.len()),
    }
}

fn print_detail(a: &Assessment, all: &[Assessment]) {
    let r = &a.record;
    println!("decision {}", a.id);
    println!("  topic       {}", a.topic);
    println!("  standing    {} {}", glyph(a.standing), a.headline());
    println!(
        "  by          {} · {}",
        minds_reader::decisions::by_word(r.decided_by),
        sanitize(&r.at)
    );
    let status = |id: &str| -> String {
        a.assumptions
            .iter()
            .find(|s| s.id == id)
            .map(|s| format!("{} {}", result_glyph(s.result), s.result.as_str()))
            .unwrap_or_else(|| "? unknown assumption".to_owned())
    };
    let reasons = |label: &str, list: &[minds_core::decision::Reason]| {
        for (i, reason) in list.iter().enumerate() {
            let statement = r
                .assumption(&reason.assumption)
                .map(|x| sanitize(&x.statement))
                .unwrap_or_default();
            println!(
                "                {:<9} {}  {} ({})  {}",
                if i == 0 { label } else { "" },
                sanitize(&reason.assumption),
                clip(&statement, 40),
                reason.weight.as_str(),
                status(&reason.assumption)
            );
        }
    };
    if let Some(chosen) = &a.chosen {
        println!("  chosen      {chosen}");
        if let Some(c) = &r.chosen {
            reasons("rests on", &c.rests_on);
        }
    }
    let label = if r.open { "option" } else { "rejected" };
    for (i, alternative) in r.alternatives.iter().enumerate() {
        println!("  {:<11} {}", label, a.alternatives[i]);
        reasons("because", &alternative.rejected_because);
        reasons("rests on", &alternative.rests_on);
    }
    for (i, s) in a.assumptions.iter().enumerate() {
        let probe = r.assumptions[i]
            .probe
            .as_ref()
            .map(describe_probe)
            .unwrap_or_else(|| "no probe".to_owned());
        println!(
            "  {:<11} {}  {}",
            if i == 0 { "assumptions" } else { "" },
            s.id,
            clip(&s.statement, 60)
        );
        println!(
            "                {}  {} {}",
            probe,
            result_glyph(s.result),
            s.result.as_str()
        );
    }
    if !r.scope.is_empty() {
        println!(
            "  scope       {}",
            r.scope
                .iter()
                .map(|g| sanitize(g))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(old) = &r.supersedes {
        let known = all.iter().any(|x| &x.id == old);
        println!(
            "  supersedes  {old}{}",
            if known { "" } else { " (not in this store)" }
        );
    }
    if let Some(by) = &a.superseded_by {
        println!("  superseded  by {by}");
    }
    for (i, note) in a.notes.iter().enumerate() {
        println!("  {:<11} {}", if i == 0 { "notes" } else { "" }, note);
    }
}

fn describe_probe(probe: &Probe) -> String {
    match probe {
        Probe::Pattern {
            path,
            glob,
            must_match,
            must_not_match,
        } => {
            let mut out = format!(
                "pattern in {}",
                sanitize(path.as_deref().or(glob.as_deref()).unwrap_or("?"))
            );
            if let Some(m) = must_match {
                out.push_str(&format!(" must match {}", sanitize(m)));
            }
            if let Some(m) = must_not_match {
                out.push_str(&format!(" must not match {}", sanitize(m)));
            }
            out
        }
        Probe::Path { path, exists } => format!(
            "path {} {}",
            sanitize(path),
            if *exists { "exists" } else { "is absent" }
        ),
        Probe::Blob { path, blob } => {
            format!(
                "blob {} unchanged ({})",
                sanitize(path),
                sanitize(&blob.chars().take(8).collect::<String>())
            )
        }
    }
}

/// Die Glyphen des Registers der TUI, eine Bedeutung je Zeichen: `◦` offen,
/// `✗` gefallen, `~` verändert, `?` nicht geprüft, `◌` Aussage in Kraft.
fn glyph(standing: Standing) -> &'static str {
    match standing {
        Standing::Open => "◦",
        Standing::Review => "✗",
        Standing::ReCheck => "~",
        Standing::Unchecked => "?",
        Standing::InForce => "✓",
    }
}

fn result_glyph(result: ProbeResult) -> &'static str {
    match result {
        ProbeResult::Holds => "✓",
        ProbeResult::Fails => "✗",
        ProbeResult::Stale => "~",
        ProbeResult::Unverifiable => "·",
        ProbeResult::Unknown(_) => "?",
    }
}

fn short(id: &ContentHash) -> String {
    format!("b3-{}", &id.hex()[..8])
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Nur die Fehlerklasse von serde, nie ihr Text — er zitierte Eingabe.
trait ClassifyWord {
    fn classify_word(&self) -> &'static str;
}

impl ClassifyWord for serde_json::Error {
    fn classify_word(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "a field has the wrong type or is missing",
            serde_json::error::Category::Eof => "unexpected end of input",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::decision::{DECISION_KIND, DECISION_SCHEMA, DecidedBy};
    use minds_reader::decisions::{MemoryTree, assess};

    fn assessment(bytes: [u8; 32]) -> Assessment {
        let record = DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "t".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T13:09:00Z".into(),
            chosen: Some(minds_core::decision::Choice {
                approach: "x".into(),
                rests_on: Vec::new(),
            }),
            open: false,
            alternatives: Vec::new(),
            assumptions: Vec::new(),
            scope: Vec::new(),
            supersedes: None,
        };
        assess(ContentHash::from_bytes(bytes), record, &MemoryTree::new())
    }

    /// Ein Präfix trifft genau eine, mehrere, oder — mit voller Länge —
    /// keine; ein Pfad ist keine Id.
    #[test]
    fn id_lookup_distinguishes_one_many_none_and_paths() {
        let mut a = [1u8; 32];
        let mut b = [1u8; 32];
        a[8] = 0xaa;
        b[8] = 0xbb;
        let all = vec![assessment(a), assessment(b)];
        assert!(matches!(find(&all, "01010101"), Lookup::Ambiguous(2)));
        assert!(matches!(find(&all, "0101010101010101aa"), Lookup::One(_)));
        assert!(matches!(
            find(&all, &format!("b3-{}", "0".repeat(64))),
            Lookup::NoMatch
        ));
        assert!(matches!(find(&all, "b3-ffffffff"), Lookup::NoMatch));
        assert!(matches!(find(&all, "src/lib.rs"), Lookup::NotAnId));
        assert!(matches!(find(&all, "abc"), Lookup::NotAnId));
        // Groß geschrieben trifft ebenso.
        assert!(matches!(find(&all, "0101010101010101AA"), Lookup::One(_)));
    }
}
