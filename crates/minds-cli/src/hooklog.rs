//! `<git-dir>/minds/hook.log` — wohin der Hook-Pfad seine Fehler schreibt.
//!
//! # Warum es diese Datei gibt
//!
//! Die von `minds enable` installierten Hooks werfen ihre Ausgabe weg
//! (`>/dev/null 2>&1`), und das aus gutem Grund: Ein Rekorder, der in den
//! Commit-Ablauf hineinredet, ist ein Rekorder, den man abschaltet. Der Preis
//! ist, dass ein Fehler dort spurlos verschwindet — und der teuerste Fall ist
//! genau der stille. Ein Tippfehler in `.minds/redact.json` bricht `checkpoint`
//! *fail-closed* ab; ab da wird nie wieder eine Session eingecheckt, das Journal
//! wächst, und an keiner Stelle steht warum. „Darf den Commit nie scheitern
//! lassen" ist damit umgesetzt — „der Fehler ist wenigstens irgendwo sichtbar"
//! war es nicht.
//!
//! # Die Regeln
//!
//! **Best effort, immer still.** Schlägt das Loggen selbst fehl, ist Schweigen
//! die einzig verbleibende Option: Ein Rekorder, der lauter wird, je kaputter er
//! ist, macht die Sitzung unbenutzbar. Keine Funktion hier gibt einen Fehler
//! zurück, und keine schreibt je auf stdout oder stderr — stdout gehört bei
//! mehreren Agents dem Steuerkanal.
//!
//! **Eine Zeile je Eintrag**, und zwar wirklich eine: Der Wortlaut fremder
//! Fehler geht durch [`crate::text::sanitize`], sonst täuschte ein
//! Zeilenumbruch in einer Meldung einen eigenen Eintrag vor.
//!
//! **Begrenzt.** Der Auslöser aus #10 schreibt bei *jedem* Commit — ohne Grenze
//! wüchse die Datei, solange der Fehler besteht, und das ist per Definition
//! lange. Erreicht sie [`MAX_BYTES`], wird auf `hook.log.1` umgeschichtet; mehr
//! als zwei Dateien entstehen nie. Geprüft wird **vor** dem Schreiben, die
//! Grenze ist also „ein MiB plus eine Zeile", keine harte Kante.
//!
//! **Eine Datei für alle Hook-Pfade.** Wer „bei mir kommt nichts an"
//! untersucht, soll einen Ort aufmachen, nicht vier — welcher Pfad geschrieben
//! hat, steht in der Zeile ([`Source`]).
//!
//! # Was hier stehen darf und was `fsck` daraus macht
//!
//! Die Datei liegt neben dem Journal, also dort, wo der **rohe**, noch nicht
//! redigierte Mitschnitt ohnehin liegt: Ein Fehlertext, der einen Ausschnitt
//! mitführte, öffnete hier keine neue Tür. Er tut es heute auch nicht — keine
//! `Display`-Implementierung auf diesem Pfad bettet Nutzlast ein, und
//! `a_hook_error_never_carries_the_raw_transcript` hält das fest, weil ein
//! künftiges `#[error("… {0}")]` es sonst unbemerkt aufgäbe.
//!
//! `minds fsck` zitiert den Wortlaut **nicht**, sondern verweist nur auf die
//! Datei: Seine Ausgabe landet im Job-Log der Pipeline, und das ist ein ganz
//! anderer Ort als ein Verzeichnis unter `.git`.
//!
//! Was hier **nicht** hineingeschrieben wird, ist ebenso wichtig: nichts durch
//! einen Symlink hindurch (siehe [`log_at`]) — dieselbe Regel, die
//! `enable::read_existing_hook` für Hook-Dateien anlegt.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use minds_capture::{Journal, clock};

use crate::text::sanitize;

/// Das Verzeichnis unter dem Git-Verzeichnis, in dem auch das Journal liegt.
const LOG_DIR: &str = "minds";

/// Die Log-Datei selbst.
const LOG_FILE: &str = "hook.log";

/// Der Vorgänger, auf den bei Überlauf umgeschichtet wird.
const ROTATED_FILE: &str = "hook.log.1";

/// Ab dieser Größe wird umgeschichtet.
const MAX_BYTES: u64 = 1024 * 1024;

/// Wie viele Zeichen eine einzelne Meldung beisteuern darf. Ein Fehlertext kann
/// einen ganzen Dateiinhalt mitführen; eine Zeile, die niemand mehr liest, ist
/// keine Diagnose.
const MAX_MESSAGE: usize = 2000;

/// Der Hook-Pfad, aus dem ein Eintrag stammt.
///
/// Als Aufzählung statt als Zeichenkette: So steht die Liste der Pfade, die
/// still scheitern können, an genau einer Stelle, und ein Tippfehler im
/// Präfix kann keinen Eintrag unauffindbar machen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// `minds hook` — der heiße Pfad, den der Agent bei jedem Event aufruft.
    Hook,
    /// `minds checkpoint` — der post-commit-Hook.
    Checkpoint,
    /// `minds prepare-commit-msg` — der Trailer-Hook.
    PrepareCommitMsg,
    /// `minds sync` — der pre-push-Hook.
    Sync,
}

impl Source {
    /// Wie der Pfad in der Zeile heißt: der Name des Unterkommandos.
    fn as_str(self) -> &'static str {
        match self {
            Source::Hook => "hook",
            Source::Checkpoint => "checkpoint",
            Source::PrepareCommitMsg => "prepare-commit-msg",
            Source::Sync => "sync",
        }
    }
}

/// Der Pfad des Logs in diesem Repository.
pub(crate) fn path(git_dir: &Path) -> PathBuf {
    git_dir.join(LOG_DIR).join(LOG_FILE)
}

/// Der Pfad des umgeschichteten Vorgängers.
///
/// Auch dafür eine Funktion statt eines Literals beim Aufrufer: `fsck` nennt
/// die Datei im Bericht, und eine Umbenennung hier ließe es sonst lautlos auf
/// einen Pfad zeigen, den es nicht gibt.
pub(crate) fn rotated_path(git_dir: &Path) -> PathBuf {
    git_dir.join(LOG_DIR).join(ROTATED_FILE)
}

/// Führt ein Hook-Kommando aus und hält einen Panic fest, statt ihn nach
/// draußen zu lassen.
///
/// `minds hook` hat diese Klammer seit jeher (dort geht es darum, dass Exit 2
/// bei Claude Code „blockiere diese Aktion" heißt). Für den kalten Pfad ist der
/// Grund ein anderer, aber nicht kleiner: Die Hooks werfen stderr weg, also
/// verschwände die Panic-Meldung des Standard-Handlers dort spurlos — kein
/// Terminal-Output, kein Log, kein `fsck`-Hinweis. Genau der stille Ausfall, um
/// den es in #10 geht, nur eine Etage tiefer.
///
/// Der Rückgabewert bleibt der des Kommandos; ein Panic wird zu
/// [`ExitCode::FAILURE`]. Die Hooks fangen das mit `|| true` ab.
pub(crate) fn guarded(
    source: Source,
    run: impl FnOnce() -> std::process::ExitCode + std::panic::UnwindSafe,
) -> std::process::ExitCode {
    guarded_into(None, source, run)
}

/// Wie [`guarded`], schreibt aber in ein benanntes Git-Verzeichnis.
///
/// Für Tests: Ohne diese Variante ginge der Eintrag über `discover_git_dir` ab
/// `cwd` — und `cwd` ist unter `cargo test` die Crate-Wurzel, also das echte
/// Repository. Ein Test, der bei jedem Lauf eine Zeile in das Log des
/// Entwicklers schreibt, vergiftet genau den Kanal, den dieses Modul einführt.
#[cfg(test)]
fn guarded_at(
    git_dir: &Path,
    source: Source,
    run: impl FnOnce() -> std::process::ExitCode + std::panic::UnwindSafe,
) -> std::process::ExitCode {
    guarded_into(Some(git_dir), source, run)
}

fn guarded_into(
    git_dir: Option<&Path>,
    source: Source,
    run: impl FnOnce() -> std::process::ExitCode + std::panic::UnwindSafe,
) -> std::process::ExitCode {
    silence_panics();
    // Die Klammer markieren, damit der Handler *nur hier* schweigt — außerhalb
    // behält jeder Panic seine Meldung, im Test-Binary also jedes `assert!`.
    //
    // `minds hook` bleibt auch am Terminal still: Regel 2 des Hook-Moduls
    // kennt keine Ausnahme, und ein Agent, der den Hook an einem PTY startet,
    // bekäme sonst doch einen Backtrace in die Sitzung. Der kalte Pfad ist
    // anders — `minds checkpoint` ist auch ein Kommando für Menschen, und wer
    // es im Terminal aufruft, soll seinen Panic sehen.
    //
    // `replace`/Restore statt `set(None)`: Eine innere Klammer entwaffnete
    // sonst die äußere, und der nächste Panic ginge doch nach draußen. Und der
    // Slot wird beim Betreten geleert — sonst meldete ein Unwind, den unser
    // Handler nie sah, den Text eines *früheren*, längst gefangenen Panics.
    let outer = IN_GUARD.replace(Some(source != Source::Hook));
    let _ = last_panic();
    let outcome = std::panic::catch_unwind(run);
    IN_GUARD.set(outer);

    match outcome {
        Ok(code) => code,
        Err(payload) => {
            // Bevorzugt der Text aus unserem eigenen Handler: Er trägt **Ort und
            // Meldung** (`src/foo.rs:42:9: etwas ging schief`). Die Nutzlast von
            // `catch_unwind` kennt nur die Meldung — ohne Ort sagt „Panic"
            // niemandem, wo er nachsehen soll.
            let note = last_panic().unwrap_or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "ohne Meldung".to_owned())
            });
            // Für den heißen Pfad **nur der Ort**, nicht die Meldung: Eine
            // Panic-Meldung kann Nutzlast einbetten (`panic!("… {payload}")`,
            // `Result::unwrap` bettet den vollen `Debug` ein), und der rohe
            // Mitschnitt läuft genau hier vorbei. `hook.log` ist die Datei, die
            // in einem Bug-Report mitgeht — der Ort sagt, wo nachzusehen ist,
            // und mehr braucht es dafür nicht. Die kalten Pfade behalten die
            // Meldung: Dort steht kein Transkript im Speicher, und sie war
            // schon vorher drin.
            let note = match source {
                Source::Hook => format!("Panic — Event verworfen: {}", location_of(&note)),
                _ => format!("Panic — Vorgang abgebrochen: {note}"),
            };

            match git_dir {
                Some(dir) => log_at(dir, source, &note),
                None => log(source, &note),
            }
            std::process::ExitCode::FAILURE
        }
    }
}

thread_local! {
    /// Läuft gerade eine [`guarded`]-Klammer auf diesem Thread — und darf ihr
    /// Panic am Terminal trotzdem sichtbar sein?
    ///
    /// `None` heißt außerhalb, `Some(false)` in der Klammer und **immer still**
    /// (der heiße Pfad), `Some(true)` in der Klammer, aber am TTY sichtbar
    /// (der kalte Pfad).
    static IN_GUARD: std::cell::Cell<Option<bool>> = const {
        std::cell::Cell::new(None)
    };
    /// Wo der eigene Handler ablegt, was der Standard-Handler gedruckt hätte.
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Installiert einen Panic-Handler, der **innerhalb** einer [`guarded`]-Klammer
/// schweigt und den Text stattdessen für das Log aufhebt.
///
/// # Warum das nötig ist
///
/// `catch_unwind` fängt den Panic — aber **zu spät**: Der Standard-Handler
/// läuft davor und hat `thread 'main' panicked at …` samt Backtrace-Hinweis
/// schon auf stderr geschrieben. Für `minds hook` ist das ein Regelbruch:
/// stderr des Hooks gehört dem Agenten, Claude Code reicht ihn dem Modell
/// zurück. Ein Rust-Backtrace mitten in der Sitzung des Nutzers ist genau das
/// Rauschen, das dieses Modul vermeiden soll (#54). Für den kalten Pfad ist es
/// der umgekehrte Verlust: Dort wirft der Hook-Body stderr weg, und der Ort
/// des Panics wäre spurlos verschwunden.
///
/// # Warum er nicht einfach alles verschluckt
///
/// `set_hook` ist **global**. Ein Handler, der bedingungslos schweigt, nähme
/// jedem Panic im selben Prozess seine Meldung — im Test-Binary also jedem
/// fehlgeschlagenen `assert!`, dessen Diagnose die halbe Testausgabe ist.
/// Deshalb zwei Bedingungen:
///
/// - **Nur in der Klammer.** Außerhalb läuft der vorherige Handler unverändert.
/// - **Nur ohne Terminal.** Ist stderr ein TTY, sitzt ein Mensch davor: Wer
///   `RUST_BACKTRACE=1 minds checkpoint` aufruft, um einem Panic nachzugehen,
///   soll ihn sehen. Aus einem Git-Hook heraus ist stderr `/dev/null` oder eine
///   Pipe, nie ein TTY — die Zusage aus #54 bleibt dort unangetastet.
///
/// Der Handler wird **einmal je Prozess** gesetzt (`Once`) und nie wieder
/// abgenommen: Ihn pro Aufruf zu tauschen wäre ein Wettlauf, sobald mehr als
/// ein Thread läuft.
/// Erklärt den **ganzen Prozess** zum Hook-Pfad: Handler installieren *und*
/// die Klammer aufmachen, ohne sie je zu schließen.
///
/// Der Unterschied zu [`silence_panics`] ist der Geltungsbereich. `guarded`
/// klammert eine Funktion; hier geht es um alles davor — `parse`, der
/// `SPECS`-Lookup, das Log-Schreiben bei einem Argumentfehler. Ein Panic dort
/// ging bisher voll auf stderr und mit Exit 101 hinaus, und für `minds hook`
/// ist das doppelt falsch: Die Agent-Registrierung lautet `minds hook --agent
/// …` **ohne** `2>/dev/null`, anders als die drei Git-Hookbodies. Wer den
/// Prozess als Hook startet, bekommt bis zu seinem Ende die Hook-Regeln.
pub(crate) fn silence_panics_for(source: Source) {
    silence_panics();
    IN_GUARD.set(Some(source != Source::Hook));
}

pub(crate) fn silence_panics() {
    use std::io::IsTerminal;

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let Some(show_at_terminal) = IN_GUARD.get() else {
                previous(info);
                return;
            };
            // `Display` von `PanicHookInfo` ist genau das, was der
            // Standard-Handler gedruckt hätte: Ort und Meldung.
            LAST_PANIC.with(|slot| *slot.borrow_mut() = Some(info.to_string()));
            if show_at_terminal && std::io::stderr().is_terminal() {
                previous(info);
            }
        }));
    });
}

/// Nimmt den Text des letzten Panics heraus — und lässt den Platz leer, damit
/// ein zweiter Panic nicht den Text des ersten meldet.
fn last_panic() -> Option<String> {
    LAST_PANIC.with(|slot| slot.borrow_mut().take())
}

/// Nur der Ort aus einem Panic-Text: `panicked at src/x.rs:1:2:` — alles vor
/// dem ersten Zeilenumbruch.
///
/// `PanicHookInfo::to_string()` setzt Ort und Meldung genau so zusammen. Fehlt
/// der Umbruch (die Meldung kam aus der `catch_unwind`-Nutzlast, ohne Ort),
/// bleibt nichts übrig, was einen Ort benennt — dann ist die ehrliche Antwort
/// „ohne Ort", nicht die Meldung selbst.
fn location_of(text: &str) -> &str {
    match text.split_once('\n') {
        Some((location, _)) => location,
        None => "ohne Ort",
    }
}

/// Meldet einen Fehler an **beide** Senken: stderr und das Log.
///
/// Der Grund, dass es diese Funktion gibt und nicht zwei Aufrufe: Der Text ist
/// fremd — bei `sync` ist es das rohe stderr eines `git`-Prozesses, und darin
/// stehen die `remote:`-Zeilen des Servers, die Git unverändert durchreicht.
/// Ging er nur auf dem Weg in die Datei durch [`crate::text::sanitize`], könnte
/// ein feindliches Remote mit `\e[2K\e[A` die Fortschrittszeile von minds im
/// Terminal überschreiben und ein „fertig" hinschreiben, das nie stattfand.
///
/// Zwei Aufrufe an zwei Stellen wären genau die Asymmetrie, die beim nächsten
/// Mal wieder entsteht. Deshalb eine Funktion, die beides tut.
pub(crate) fn report_at(git_dir: &Path, source: Source, message: &str) {
    eprintln!("minds {}: {}", source.as_str(), sanitize(message));
    log_at(git_dir, source, message);
}

/// Wie [`report_at`], sucht das Git-Verzeichnis aber selbst.
pub(crate) fn report(source: Source, message: &str) {
    eprintln!("minds {}: {}", source.as_str(), sanitize(message));
    log(source, message);
}

/// Schreibt einen Eintrag in das Log des Repositories, in dem wir stehen.
///
/// Das Git-Verzeichnis wird gesucht statt übergeben, und zwar genauso wie auf
/// dem heißen Pfad: von `cwd` aufwärts, ohne Repository, ohne Konfiguration.
/// Das ist kein Sparen an der Genauigkeit, sondern der einzige Weg, der auch
/// dann noch trägt, wenn das Öffnen des Repositories **selbst** der Fehler war
/// — und das ist der Fall, den zu verschweigen am teuersten wäre.
pub(crate) fn log(source: Source, message: &str) {
    let Some(git_dir) = discover_git_dir() else {
        return;
    };
    log_at(&git_dir, source, message);
}

/// Schreibt einen Eintrag in das Log unter `git_dir`.
///
/// Für die Aufrufer, die das Git-Verzeichnis bereits geöffnet haben: Dort ist es
/// die belastbare Antwort, während die Suche ab `cwd` eine Vermutung bleibt —
/// und in einer Schleife obendrein eine wiederholte.
pub(crate) fn log_at(git_dir: &Path, source: Source, message: &str) {
    let path = path(git_dir);
    let Some(dir) = path.parent() else {
        return;
    };
    // **Das Verzeichnis, bevor wir es benutzen.**
    //
    // `<git-dir>/minds` legen wir selbst an — es darf deshalb kein Symlink
    // sein. Zeigte es auf ein fremdes Verzeichnis, wäre `create_dir_all` ein
    // No-op, und jede Prüfung am Blatt liefe ins Ziel des Links statt hierher:
    // `symlink_metadata` folgt allen Gliedern außer dem letzten. Das
    // Git-Verzeichnis selbst ist davon ausgenommen — ein symlinktes `.git`
    // unterstützt Git, und dorthin kann ein Checkout nichts legen.
    if is_symlink(dir) {
        return;
    }
    let _ = fs::create_dir_all(dir);
    // **Und das Blatt, bevor wir es öffnen.** Die Identitätsprüfung unten fängt
    // den Symlink zwar ab — aber erst *nach* dem `open`, und `O_CREAT` folgt
    // dem Link: Zeigt er auf einen Pfad, den es nicht gibt, entstünde dort eine
    // leere Datei. Ein „lege eine Datei an beliebiger schreibbarer Stelle an"
    // ist wenig, aber es ist mehr als nichts, und es kostet einen `lstat`.
    //
    // Dieselbe Prüfung schützt `rotate_if_full`: `fs::metadata` folgte dem Link
    // und mäße die Größe des Ziels, `fs::rename` verschöbe danach den Link nach
    // `hook.log.1` und überschriebe damit einen echten Vorgänger.
    if is_symlink(&path) {
        return;
    }
    rotate_if_full(&path);

    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    if let Ok(mut file) = options.open(&path) {
        // **Und dann, was wir tatsächlich offen haben.**
        //
        // Der Vergleich am offenen Dateizeiger fängt den Symlink am Blatt und
        // schließt zugleich das Zeitfenster zwischen Prüfen und Öffnen, statt
        // es nur zu verkleinern — ein Name ist zwischen zwei Aufrufen
        // austauschbar, ein Dateizeiger nicht.
        //
        // `enable` löst dasselbe Problem anders (schreiben über eine
        // Nachbardatei und `rename`); hier geht das nicht, weil angehängt wird.
        if !is_our_file(&file, &path) {
            return;
        }
        // Auch eine schon vorhandene Datei zurechtrücken: `mode` oben gilt nur
        // bei der Neuanlage, und die Fassung vor diesem Commit legte sie ohne
        // Modus an — bei jedem, der schon einmal einen Hook-Fehler hatte, liegt
        // sie deshalb mit Umask-Default da, während jetzt mehr hineinfließt.
        // Nach der Identitätsprüfung, nie davor: `set_permissions` auf einem
        // Dateizeiger, der woandershin zeigt, wäre selbst der Angriff.
        restrict(&file);
        let (at, _) = clock::now();
        // **Ein** `write_all`, nicht `writeln!`. `Write::write_fmt` schickt
        // jedes Format-Stück einzeln zum Dateizeiger — aus einer Zeile würden
        // sechs `write()`-Syscalls, und `O_APPEND` ist nur je Syscall atomar.
        // Bei einer Agent-Flotte schreiben mehrere `minds hook`-Prozesse
        // gleichzeitig; die Zeilen zersägten sich dann gegenseitig, und
        // `count_entries` zählte Bruchstücke. Erst bauen, dann schreiben.
        let line = format!("{at} {}: {}\n", source.as_str(), entry(message));
        let _ = file.write_all(line.as_bytes());
    }
}

/// Ob an diesem Namen ein Symlink hängt. Ein Pfad, den es nicht gibt, ist
/// keiner — dort legen wir gleich selbst an.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Ob die geöffnete Datei dieselbe ist, die unter diesem Namen liegt — und
/// nicht das Ziel eines Links am letzten Glied.
///
/// Verglichen werden Gerät und Inode: `fstat` auf dem Dateizeiger gegen `lstat`
/// auf dem Namen. Stimmen sie überein, ist der Name kein Symlink *und* er hat
/// unterwegs zu keinem gehört — ein Link an irgendeiner Stelle führte zu einer
/// anderen Inode als die, die `lstat` am Namen selbst sieht.
#[cfg(unix)]
fn is_our_file(file: &fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let (Ok(opened), Ok(named)) = (file.metadata(), fs::symlink_metadata(path)) else {
        return false;
    };
    opened.dev() == named.dev() && opened.ino() == named.ino()
}

/// Ohne Inodes bleibt die Prüfung am Namen — mehr gibt die Plattform hier nicht
/// her, und ein Diagnosepfad ist kein Ort für eine neue Dependency.
#[cfg(not(unix))]
fn is_our_file(_file: &fs::File, path: &Path) -> bool {
    !fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Nimmt der Datei die Rechte für alle außer dem Eigentümer.
#[cfg(unix)]
fn restrict(file: &fs::File) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = file.metadata() else {
        return;
    };
    if meta.permissions().mode() & 0o077 != 0 {
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
}

#[cfg(not(unix))]
fn restrict(_file: &fs::File) {}

/// Eine Meldung, wie sie in der Zeile erscheint: entschärft und gekürzt.
///
/// **Zeichenweise gebaut, nicht hinterher geschnitten.** Ein Schnitt am fertigen
/// Text fiele mitten in eine Escape-Sequenz (`\u{1b}` sind sieben Zeichen) und
/// ließe `\u{1b` stehen — genau die Uneindeutigkeit, die [`sanitize`] gerade
/// beseitigt hat, und ein Fehler, der nur bei bestimmten Längen auftritt und
/// deshalb von einem einzelnen Test nie gefunden wird. Wer stattdessen jedes
/// Zeichen erst entschärft und dann prüft, ob es noch passt, kann eine halbe
/// Sequenz gar nicht erst erzeugen.
///
/// Die Grenze zählt in **Zeichen der Ausgabe**: Eine Meldung aus lauter
/// ESC-Sequenzen wüchse sonst auf das Siebenfache — unlesbar, und lang genug,
/// um die Atomarität des einen `write_all` wieder infrage zu stellen.
fn entry(message: &str) -> String {
    let mut line = String::with_capacity(message.len());
    // Mitgezählt statt nachgezählt: `line.chars().count()` je Zeichen wäre
    // quadratisch, und eine Meldung darf nicht teurer sein als der Fehler.
    let mut written = 0usize;
    let mut cut = false;

    for c in message.chars() {
        let piece = sanitize(&c.to_string());
        let length = piece.chars().count();
        if written + length > MAX_MESSAGE {
            cut = true;
            break;
        }
        line.push_str(&piece);
        written += length;
    }

    if cut {
        line.push('…');
    }
    line
}

/// Schichtet auf [`ROTATED_FILE`] um, wenn die Datei ihre Grenze erreicht hat.
///
/// Ein vorhandener Vorgänger wird dabei überschrieben — zwei Dateien sind die
/// Zusage, nicht zwei plus Historie.
///
/// Bewusst ohne Sperre, und damit mit zwei bekannten Rennen: Sehen zwei
/// Prozesse gleichzeitig „voll", schichtet der zweite die eben angelegte
/// Nachfolgedatei um und nimmt den Eintrag des ersten mit; scheitert das
/// Umbenennen (fremde Rechte), wird einfach weiter angehängt. Beides ist
/// hinnehmbar: Es kostet im schlimmsten Fall einen Eintrag oder eine zu lange
/// Datei. Ein Lock auf dem Diagnosepfad wäre der teurere Handel — er kann
/// klemmen, und dann verlöre man alles, was danach kommt.
fn rotate_if_full(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    let _ = fs::rename(path, path.with_file_name(ROTATED_FILE));
}

/// Das Git-Verzeichnis, von `cwd` aufwärts gesucht.
fn discover_git_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let journal = Journal::discover(&cwd).ok()?;
    // `Journal::root()` ist `<git-dir>/minds/journal`; zwei Ebenen zurück ist
    // das Git-Verzeichnis. `the_journal_sits_two_levels_below_the_git_dir` hält
    // diese Annahme fest — sonst schriebe eine Umbenennung in `minds-capture`
    // das Log lautlos an einen anderen Ort.
    journal.root().parent()?.parent().map(Path::to_path_buf)
}

/// Was `fsck` über das Log zu berichten hat.
pub(crate) struct LogSummary {
    /// Wie viele Einträge in der aktuellen Datei stehen. Kann `0` sein, wenn
    /// nur noch der umgeschichtete Vorgänger da ist.
    pub(crate) entries: usize,
    /// Ob daneben ein umgeschichteter Vorgänger liegt.
    pub(crate) rotated: bool,
}

/// Fasst das Log zusammen, oder `None`, wenn es keins gibt (der Normalfall).
///
/// Gelesen wird gezählt statt gesammelt: Die Datei ist zwar begrenzt, aber eine
/// aus einer älteren Version übernommene ist es nicht, und `fsck` soll auch
/// dann eine Zahl nennen statt Speicher zu belegen.
pub(crate) fn summary(git_dir: &Path) -> Option<LogSummary> {
    let path = path(git_dir);
    let entries = count_entries(&path).unwrap_or(0);
    // Der Vorgänger zählt auch dann, wenn die aktuelle Datei leer oder gelöscht
    // ist: Wer dem Rat „erledigt? Datei löschen" folgt und nur `hook.log`
    // wegnimmt, machte `hook.log.1` sonst für immer unsichtbar.
    let rotated = path.with_file_name(ROTATED_FILE).exists();
    if entries == 0 && !rotated {
        return None;
    }
    Some(LogSummary { entries, rotated })
}

/// Zählt die Zeilen — byteweise, in konstantem Speicher und ohne Annahme über
/// die Kodierung. Eine unvollständige Schlusszeile zählt als Eintrag; unsere
/// eigenen enden immer auf `\n`, eine abgeschnittene stammt aus einem
/// abgebrochenen Schreibvorgang und ist trotzdem eine Meldung.
fn count_entries(path: &Path) -> Option<usize> {
    // Nur eine gewöhnliche Datei. `fsck` ist das CI-Gate: Läge an dieser Stelle
    // eine FIFO, bliebe `open` bis zum ersten Schreiber stehen; läge dort
    // `/dev/zero`, liefe die Schleife unten für immer. Beides hielte die
    // Pipeline an, ohne dass jemand den Grund sähe — und ein *Hinweis* darf
    // niemals mehr kosten als der Befund, den er ankündigt.
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    // Direkt in den eigenen Puffer: Ein `BufReader` davor kopierte jedes Byte
    // ein zweites Mal, ohne etwas zu vereinfachen.
    let mut reader = fs::File::open(path).ok()?;
    let mut buffer = [0u8; 8192];
    let mut entries = 0usize;
    let mut trailing = false;

    loop {
        let read = reader.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        entries += chunk.iter().filter(|byte| **byte == b'\n').count();
        trailing = chunk[read - 1] != b'\n';
    }

    Some(entries + usize::from(trailing))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein Git-Verzeichnis-Attrappe: `log_at` legt sich alles Weitere selbst an.
    fn git_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn read(git_dir: &Path) -> String {
        fs::read_to_string(path(git_dir)).unwrap()
    }

    #[test]
    fn an_entry_carries_the_source_and_the_message() {
        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, "redact.json ist kaputt");

        let content = read(dir.path());
        assert!(
            content.contains("checkpoint: redact.json ist kaputt"),
            "{content}"
        );
        assert!(content.ends_with('\n'), "{content}");
    }

    #[test]
    fn entries_accumulate_instead_of_replacing_each_other() {
        let dir = git_dir();
        log_at(dir.path(), Source::Hook, "erster");
        log_at(dir.path(), Source::Sync, "zweiter");

        let content = read(dir.path());
        assert_eq!(content.lines().count(), 2, "{content}");
        assert!(content.contains("hook: erster"), "{content}");
        assert!(content.contains("sync: zweiter"), "{content}");
    }

    #[test]
    fn the_log_lands_next_to_the_journal() {
        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, "irgendwas");
        assert_eq!(path(dir.path()), dir.path().join("minds").join("hook.log"));
    }

    #[test]
    fn the_journal_sits_two_levels_below_the_git_dir() {
        // Die Annahme, auf der `discover_git_dir` steht. Bricht sie, schriebe
        // der ohne Repository geloggte Fehler an einen anderen Ort als der mit
        // Repository geloggte — und niemand fände die halbe Diagnose.
        let dir = git_dir();
        let journal_root = Journal::open(dir.path()).root().to_path_buf();
        assert_eq!(journal_root.parent().unwrap().parent().unwrap(), dir.path());

        // Und dieselbe Ebene, nicht nur dieselbe Tiefe: Benennte `minds-capture`
        // sein Verzeichnis um, träfe die Zählung oben weiterhin zu, aber
        // `LOG_DIR` zeigte ins Leere — und die Zusage „das Log liegt neben dem
        // Journal", auf der die Sicherheitsüberlegung im Modulkopf steht, wäre
        // still gebrochen.
        assert_eq!(
            journal_root.parent().unwrap(),
            path(dir.path()).parent().unwrap()
        );
    }

    #[test]
    fn a_panic_lands_in_the_log_instead_of_nowhere() {
        // Zusage 7 des Features. Ohne die Klammer ginge die Panic-Meldung des
        // Standard-Handlers auf stderr — im Hook also nirgendwohin.
        //
        // `guarded_at` statt `guarded`: Letzteres suchte das Git-Verzeichnis ab
        // `cwd`, und das ist unter `cargo test` die Crate-Wurzel — der Test
        // schriebe seine Zeile in das echte Repo, bei jedem Lauf eine mehr.
        let dir = git_dir();
        let code = guarded_at(dir.path(), Source::Checkpoint, || {
            panic!("etwas ging schief")
        });

        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::FAILURE)
        );
        let content = read(dir.path());
        assert!(content.contains("checkpoint: Panic"), "{content}");
        // Und mit dem Wortlaut — sonst stünde da nur, *dass* etwas passiert ist.
        assert!(content.contains("etwas ging schief"), "{content}");
        // Samt **Ort** (#54): Ohne ihn weiß niemand, wo er nachsehen soll — und
        // nur dieser Teil belegt, dass der Text aus unserem eigenen Handler
        // kommt und nicht aus der `catch_unwind`-Nutzlast.
        assert!(
            content.contains("hooklog.rs:"),
            "kein Ort im Log:\n{content}"
        );
    }

    /// Die Rückfallebene: Ein Unwind, den unser Handler nie gesehen hat (er
    /// stammt von einem anderen Thread), muss trotzdem im Log landen — dann
    /// eben ohne Ort. Ohne diesen Zweig wäre ein solcher Panic spurlos.
    #[test]
    fn an_unwind_without_our_handler_still_lands_in_the_log() {
        let dir = git_dir();
        let code = guarded_at(dir.path(), Source::Sync, || {
            std::panic::resume_unwind(Box::new("aus einem anderen Thread".to_owned()))
        });

        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::FAILURE)
        );
        let content = read(dir.path());
        assert!(
            content.contains("aus einem anderen Thread"),
            "die Nutzlast fehlt:\n{content}"
        );
    }

    /// Außerhalb der Klammer bleibt der Standard-Handler zuständig — sonst
    /// nähme der stille Handler jedem `assert!` im selben Prozess seine
    /// Diagnose, und ein roter Test sagte nicht mehr, woran er scheiterte.
    #[test]
    fn outside_the_guard_panics_keep_their_message() {
        // Den Handler installieren, wie es `guarded` täte …
        silence_panics();
        // … und dann *außerhalb* panicken lassen.
        let payload = std::panic::catch_unwind(|| panic!("sichtbar")).unwrap_err();
        assert!(
            payload
                .downcast_ref::<&str>()
                .is_some_and(|s| s.contains("sichtbar")),
            "die Nutzlast ging verloren"
        );
        // Und der Slot bleibt leer: Was außerhalb passiert, gehört nicht ins Log.
        assert!(last_panic().is_none(), "der Slot wurde außerhalb gefüllt");
    }

    #[test]
    fn a_normal_return_value_passes_through_unchanged() {
        // Die Gegenprobe: Die Klammer darf den Rückgabewert nicht einebnen —
        // `checkpoint` unterscheidet Erfolg und Fehlschlag darüber.
        let dir = git_dir();
        for expected in [
            std::process::ExitCode::SUCCESS,
            std::process::ExitCode::FAILURE,
        ] {
            let code = guarded_at(dir.path(), Source::Sync, || expected);
            assert_eq!(format!("{code:?}"), format!("{expected:?}"));
        }
        assert!(summary(dir.path()).is_none(), "ohne Panic kein Eintrag");
    }

    #[test]
    fn a_shortened_message_never_ends_in_a_half_escape_at_any_offset() {
        // Der Fehler, den ein einzelner Offset nicht findet: Wo genau der
        // Schnitt fällt, hängt von der Länge ab, und `\u{1b}` ist sieben
        // Zeichen lang. Deshalb über die ganze Nachbarschaft der Grenze.
        for pad in (MAX_MESSAGE - 8)..MAX_MESSAGE {
            let message = format!("{}{}", "x".repeat(pad), "\u{1b}".repeat(5));
            let line = entry(&message);

            assert!(line.ends_with('…'), "pad={pad}: {line:?}");
            let body = &line[..line.len() - '…'.len_utf8()];
            // Jede Sequenz, die anfängt, muss auch enden: Nach dem letzten `\`
            // steht entweder nichts mehr davor oder eine vollständige.
            assert!(
                !body.ends_with('\\') && !body.contains("\\u{1") || body.ends_with('}'),
                "pad={pad}: halbe Sequenz in {body:?}"
            );
            assert!(
                line.chars().count() <= MAX_MESSAGE + 1,
                "pad={pad}: {} Zeichen",
                line.chars().count()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_does_not_get_its_target_created() {
        // `O_CREAT` folgt dem Link: Zeigt er ins Leere, entstünde dort eine
        // Datei. Wenig für sich genommen — aber es ist ein „lege etwas an
        // beliebiger schreibbarer Stelle an", und das gehört nicht in einen
        // Diagnosepfad.
        let dir = git_dir();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("gibt-es-noch-nicht.txt");

        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &log).unwrap();

        log_at(dir.path(), Source::Checkpoint, "sollte nirgends landen");

        assert!(!victim.exists(), "der Link darf nichts entstehen lassen");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_symlink() {
        // Ein Link an der Stelle des Logs zeigte auf eine fremde Datei, und die
        // bekäme bei jedem Commit und jedem Push eine Zeile angehängt — mit den
        // Rechten des Entwicklers. `enable::read_existing_hook` lehnt aus
        // demselben Grund einen verlinkten Hook ab.
        let dir = git_dir();
        let victim = dir.path().join("opfer.txt");
        fs::write(&victim, "unberührt\n").unwrap();

        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &log).unwrap();

        log_at(dir.path(), Source::Checkpoint, "sollte nirgends landen");

        assert_eq!(fs::read_to_string(&victim).unwrap(), "unberührt\n");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_symlinked_directory() {
        // Der Umweg, den eine Prüfung am Blatt nicht sieht: Nicht die Datei ist
        // der Link, sondern `<git-dir>/minds`. `symlink_metadata` auf dem
        // Blattpfad folgte ihm und fände dort eine ganz normale Datei.
        let dir = git_dir();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("hook.log");
        fs::write(&victim, "unberührt\n").unwrap();

        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("minds")).unwrap();

        log_at(dir.path(), Source::Checkpoint, "sollte nirgends landen");

        assert_eq!(fs::read_to_string(&victim).unwrap(), "unberührt\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_does_not_get_its_target_chmodded() {
        // Die zweite Hälfte desselben Fundes: `restrict()` ist ein `fchmod` und
        // damit selbst ein Werkzeug. Es darf erst laufen, wenn feststeht, dass
        // der Dateizeiger auf unsere Datei zeigt.
        use std::os::unix::fs::PermissionsExt;

        let dir = git_dir();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("hook.log");
        fs::write(&victim, "unberührt\n").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();

        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("minds")).unwrap();
        log_at(dir.path(), Source::Checkpoint, "sollte nirgends landen");

        let mode = fs::metadata(&victim).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "fremde Rechte bleiben, wie sie waren");
    }

    #[test]
    fn a_line_separator_in_the_message_cannot_forge_a_second_entry() {
        // U+2028 ist für `str::lines` kein Umbruch — für einen Browser und für
        // Pythons `splitlines()` schon. Ein Test, der nur `lines()` zählt, sähe
        // die Lücke deshalb nicht.
        let dir = git_dir();
        log_at(
            dir.path(),
            Source::Checkpoint,
            "harmlos\u{2028}1970-01-01T00:00:00Z checkpoint: alles in Ordnung",
        );

        let content = read(dir.path());
        assert_eq!(content.lines().count(), 1, "{content:?}");
        assert!(!content.contains('\u{2028}'), "{content:?}");
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_log_with_loose_permissions_is_tightened() {
        // Die Fassung vor #10 legte die Datei ohne Modus an. Bei jedem, der
        // schon einmal einen Hook-Fehler hatte, liegt sie mit Umask-Default da —
        // und jetzt fließt mehr hinein.
        use std::os::unix::fs::PermissionsExt;

        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "alt\n").unwrap();
        fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();

        log_at(dir.path(), Source::Checkpoint, "neu");

        let mode = fs::metadata(&log).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    #[test]
    fn only_a_rotated_predecessor_is_still_worth_reporting() {
        // Wer dem Rat „erledigt? Datei löschen" folgt und nur `hook.log`
        // wegnimmt, darf `hook.log.1` nicht unsichtbar machen.
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(log.with_file_name(ROTATED_FILE), "alt\nälter\n").unwrap();

        let summary = summary(dir.path()).expect("der Vorgänger zählt");
        assert_eq!(summary.entries, 0);
        assert!(summary.rotated);
    }

    #[test]
    fn a_newline_in_the_message_cannot_forge_a_second_entry() {
        let dir = git_dir();
        log_at(
            dir.path(),
            Source::Checkpoint,
            "harmlos\n1970-01-01T00:00:00Z checkpoint: alles in Ordnung",
        );

        let content = read(dir.path());
        assert_eq!(content.lines().count(), 1, "{content}");
        assert!(content.contains("\\n"), "{content}");
    }

    #[test]
    fn an_oversized_message_is_shortened() {
        let dir = git_dir();
        let huge = "x".repeat(MAX_MESSAGE * 3);
        log_at(dir.path(), Source::Checkpoint, &huge);

        let content = read(dir.path());
        // Harte Schranke: die Meldung plus Zeitstempel, Quelle und Auslassung.
        // Eine großzügige Schranke hielte den Fall darunter nicht fest.
        assert!(
            content.chars().count() <= MAX_MESSAGE + 64,
            "{} Zeichen",
            content.chars().count()
        );
        assert!(content.contains('…'), "die Kürzung wird benannt");
    }

    #[test]
    fn escaping_cannot_inflate_a_message_past_the_limit() {
        // Der Fall, den die Kürzung *vor* dem Entschärfen allein nicht abdeckt:
        // `escape_debug` macht aus einem ESC sieben Zeichen. Ohne den zweiten
        // Schnitt stünde hier eine Zeile mit dem Siebenfachen der Grenze — und
        // die stellte die Atomarität des einen `write_all` wieder infrage.
        let dir = git_dir();
        let escapes = "\u{1b}".repeat(MAX_MESSAGE * 3);
        log_at(dir.path(), Source::Checkpoint, &escapes);

        let content = read(dir.path());
        assert!(
            content.chars().count() <= MAX_MESSAGE + 64,
            "{} Zeichen",
            content.chars().count()
        );
        assert_eq!(content.lines().count(), 1, "und immer noch eine Zeile");
    }

    #[test]
    fn a_message_at_exactly_the_limit_is_not_marked_as_shortened() {
        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, &"x".repeat(MAX_MESSAGE));
        assert!(
            !read(dir.path()).contains('…'),
            "nichts abgeschnitten, nichts zu melden"
        );
    }

    #[test]
    fn concurrent_writers_do_not_saw_each_others_lines_apart() {
        // Die Umgebung, für die das Produkt gebaut ist: Eine Agent-Flotte
        // startet je Event ein eigenes `minds hook`, dazu kommen `checkpoint`
        // und `prepare-commit-msg` aus dem Commit. `O_APPEND` ist nur je
        // Syscall atomar — schriebe ein Eintrag in mehreren Stücken (wie es
        // `writeln!` tut), zersägten sich die Zeilen gegenseitig, und
        // `count_entries` zählte Bruchstücke statt Einträge.
        const WRITERS: usize = 4;
        const EACH: usize = 250;

        let dir = git_dir();
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = dir.path();
                scope.spawn(move || {
                    for round in 0..EACH {
                        log_at(path, Source::Hook, &format!("{writer}-{round}"));
                    }
                });
            }
        });

        let content = read(dir.path());
        assert_eq!(content.lines().count(), WRITERS * EACH, "keine Zeile fehlt");
        for line in content.lines() {
            assert!(line.contains(" hook: "), "zersägte Zeile: {line:?}");
        }
        assert_eq!(
            summary(dir.path()).unwrap().entries,
            WRITERS * EACH,
            "fsck zählt Einträge, nicht Bruchstücke"
        );
    }

    #[test]
    fn shortening_does_not_split_a_character() {
        // Mehrbyte-Zeichen genau an der Grenze: Ein byteweiser Schnitt panickte
        // hier (derselbe Fehler wie in #1).
        let dir = git_dir();
        let umlauts = "ä".repeat(MAX_MESSAGE + 10);
        log_at(dir.path(), Source::Checkpoint, &umlauts);
        assert!(read(dir.path()).contains('ä'));
    }

    #[test]
    fn a_full_log_is_rotated_instead_of_growing() {
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "a".repeat(MAX_BYTES as usize + 1)).unwrap();

        log_at(dir.path(), Source::Checkpoint, "nach dem Umschichten");

        let content = read(dir.path());
        assert!(content.contains("nach dem Umschichten"), "{content}");
        assert_eq!(content.lines().count(), 1, "die neue Datei fängt leer an");
        assert!(
            log.with_file_name(ROTATED_FILE).exists(),
            "der Vorgänger bleibt erhalten"
        );
    }

    #[test]
    fn rotation_keeps_at_most_two_files() {
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();

        for round in 0..3 {
            fs::write(&log, "a".repeat(MAX_BYTES as usize + 1)).unwrap();
            log_at(dir.path(), Source::Checkpoint, &format!("runde {round}"));
        }

        let files: Vec<_> = fs::read_dir(log.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files.len(), 2, "{files:?}");
    }

    #[test]
    fn a_repo_without_a_log_has_nothing_to_summarize() {
        let dir = git_dir();
        assert!(summary(dir.path()).is_none());
    }

    #[test]
    fn an_empty_log_file_has_nothing_to_summarize() {
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "").unwrap();
        assert!(summary(dir.path()).is_none());
    }

    #[test]
    fn the_summary_counts_the_entries() {
        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, "eins");
        log_at(dir.path(), Source::Checkpoint, "zwei");

        let summary = summary(dir.path()).unwrap();
        assert_eq!(summary.entries, 2);
        assert!(!summary.rotated);
    }

    #[test]
    fn the_summary_names_a_rotated_predecessor() {
        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, "eins");
        fs::write(path(dir.path()).with_file_name(ROTATED_FILE), "alt\n").unwrap();

        assert!(summary(dir.path()).unwrap().rotated);
    }

    #[test]
    fn a_log_without_a_final_newline_still_counts_its_last_entry() {
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, "eins\nzwei ohne Umbruch").unwrap();

        assert_eq!(summary(dir.path()).unwrap().entries, 2);
    }

    #[cfg(unix)]
    #[test]
    fn something_that_is_not_a_file_is_not_counted() {
        // Ein Verzeichnis an der Stelle des Logs steht hier stellvertretend für
        // FIFO und Gerätedatei: `fsck` muss daran vorbeigehen, nicht hängen.
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(&log).unwrap();

        assert!(summary(dir.path()).is_none());
    }

    #[test]
    fn a_log_with_invalid_utf8_is_still_countable() {
        // Aus einer fremden Quelle oder einem abgebrochenen Schreibvorgang. Die
        // Zählung darf daran nicht scheitern — `fsck` soll berichten, nicht
        // stolpern.
        let dir = git_dir();
        let log = path(dir.path());
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::write(&log, [0xff, 0xfe, b'\n', 0xff, b'\n']).unwrap();

        assert_eq!(summary(dir.path()).unwrap().entries, 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_log_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = git_dir();
        log_at(dir.path(), Source::Checkpoint, "geheim genug");

        let mode = fs::metadata(path(dir.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
}
