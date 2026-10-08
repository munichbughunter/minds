//! Erstsicht-Gegenzeichnungen als MR-Note (EA-19).
//!
//! `minds anchor` legt seine Gegenzeichnungen unter
//! `refs/minds/anchors/first-sight/` ab — dort, wo auch jeder mit Push-Recht
//! auf `refs/minds/*` sie löschen kann. Die Note am Merge Request ist die
//! Kopie, die der Agent nicht löschen kann: `minds verify --online` liest sie
//! und meldet einen Ref, den sie nennt, der aber fehlt.
//!
//! # Was in der Note steht
//!
//! Je Pipeline eine Note mit dem Marker `<!-- minds:anchor:<pipeline>:<teil> -->`,
//! die Seal-Ids als Liste und — eingeklappt — je Seal der **signierte Text
//! samt Signatur**. Eine Note kann jeder schreiben, der den Merge Request
//! kommentieren darf; ein Befund entsteht deshalb nur aus einem Eintrag,
//! dessen Signatur der Prüfer gegen seine Signer unter `minds-anchor`
//! verifiziert. Mehr als [`MAX_PER_NOTE`] Einträge (der erste Lauf in einem
//! Repository mit vielen Seals) verteilen sich auf mehrere Notes derselben
//! Pipeline — eine Note hat bei GitLab eine Höchstlänge.
//!
//! # Idempotenz nur über eigene Notes
//!
//! Anders als bei [`crate::Project::mirror`] zählt ein Marker nur in einer
//! Note, die der Nutzer des Tokens selbst geschrieben hat: Sonst könnte
//! jeder, der kommentieren darf, den Marker einer kommenden Pipeline vorab
//! posten — die echte Note entstünde nie, und ein danach gelöschter Ref fiele
//! nicht mehr auf. Der Marker muss die Note **beginnen**: Eine gespiegelte
//! Review-Zusammenfassung desselben Bots, die ihn zitiert, zählt nicht.
//! Nach einem Wechsel des Bot-Benutzers (Token-Rotation) entsteht beim
//! Wiederholen einer Pipeline eine zweite Note — für `verify` harmlos.
//!
//! # Lesen
//!
//! [`entries`] ist tolerant: Es sucht in beliebigem Markdown nach Blöcken aus
//! fünf Zeilen `minds-anchor-v1`… und einer armierten Signatur direkt danach.
//! Was es findet, ist **ungeprüft** — Form und Signatur prüft der Leser.
//! Gelesen wird vollständig oder gar nicht: Ein Merge Request mit mehr Notes
//! (oder ein Commit mit mehr Merge Requests), als gelesen werden, ist ein
//! Fehler — eine Prüfung, die nicht alles sah, sagte sonst „nichts gefunden".
//! Das hat einen Preis: Wer einen Merge Request mit Kommentaren flutet, macht
//! `verify --online` für dessen Commits zum operativen Fehler (Exit 4) —
//! laut, nie grün.

use minds_core::first_sight::{FIRST_SIGHT_LINES, FIRST_SIGHT_VERSION, FirstSight};
use minds_core::intent_anchor::is_armored_signature;

use crate::Project;

/// So viele Einträge trägt eine Note höchstens (ein Eintrag ≈ 500 Zeichen;
/// GitLab begrenzt eine Note auf 1 000 000).
pub const MAX_PER_NOTE: usize = 400;

/// So viele Seiten (je [`PER_PAGE`]) Notes werden je Merge Request gelesen.
const MAX_NOTE_PAGES: usize = 50;

/// So viele Seiten (je [`PER_PAGE`]) Merge Requests werden je Commit gelesen.
const MAX_MR_PAGES: usize = 5;

/// Einträge je Seite.
const PER_PAGE: usize = 100;

/// So viele Commits eines Pushs fragt `--mirror` höchstens nach ihren
/// Merge Requests.
const MAX_MIRROR_COMMITS: usize = 100;

/// So viele Anker-Einträge liest `verify --online` je Commit höchstens —
/// mehr ist ein Fehler, nie „nichts gefunden".
const MAX_ENTRIES: usize = 20_000;

/// So viele Zeilen nach dem Text darf die Signatur höchstens enden — die
/// Suche ist begrenzt, ein Text ohne Signatur kostet nie den Rest der Note.
const SIGNATURE_WINDOW: usize = 64;

/// Der Marker einer Anker-Note: Pipeline und Teil (ab 1).
pub fn marker(pipeline: u64, part: usize) -> String {
    format!("<!-- minds:anchor:{pipeline}:{part} -->")
}

/// Der Anfang jedes Anker-Markers — so erkennt [`entries`] die Notes, die
/// es überhaupt liest.
const MARKER_PREFIX: &str = "<!-- minds:anchor:";

/// Ein Eintrag: Textform und Signatur einer Gegenzeichnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Die Textform (`minds-anchor-v1`, fünf Zeilen mit `\n` am Ende).
    pub text: String,
    /// Die armierte `ssh-sig`-Signatur, mit `\n` am Ende.
    pub signature: String,
}

/// Was die Spiegelung an einem Merge Request bewirkt hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirrored {
    /// Der Merge Request.
    pub mr: u64,
    /// Neu gespiegelte Einträge.
    pub posted: usize,
    /// Einträge, die schon in einer eigenen Note standen.
    pub kept: usize,
    /// Notes **anderer** Autoren mit dem Marker dieser Pipeline — ignoriert,
    /// gemeldet.
    pub foreign_markers: usize,
}

/// Was `mirror_anchors` über alle Merge Requests ergab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorReport {
    /// Je gemergtem Merge Request das Ergebnis.
    pub results: MirrorResults,
    /// Offene Merge Requests in den Branch, die einen Commit des Bereichs
    /// enthalten — gemeldet: Läuft die Pipeline vor dem Merge-Status,
    /// entstünde für sie sonst still keine Note (geschlossene zählen nicht).
    pub not_merged: usize,
}

/// Die Notes einer Pipeline: je Teil Marker und Text. `entries` werden nach
/// Seal sortiert und in Teile zu höchstens [`MAX_PER_NOTE`] geschnitten — ein
/// wiederholter Lauf über dieselben Einträge ergibt dieselben Teile.
///
/// Ein Eintrag, dessen Text nicht parst oder dessen Signatur nicht armiert
/// ist, wird ausgelassen: Die Note gibt nur wieder, was im Repository steht.
pub fn note_bodies(pipeline: u64, entries: &[Entry]) -> Vec<(String, String)> {
    let mut parsed: Vec<(FirstSight, &Entry)> = entries
        .iter()
        .filter(|entry| is_armored_signature(&entry.signature))
        .filter_map(|entry| Some((FirstSight::parse(&entry.text).ok()?, entry)))
        .collect();
    parsed.sort_by(|(a, _), (b, _)| a.seal.cmp(&b.seal));
    parsed.dedup_by(|(a, _), (b, _)| a.seal == b.seal);
    let parts = parsed.len().div_ceil(MAX_PER_NOTE);
    parsed
        .chunks(MAX_PER_NOTE)
        .enumerate()
        .map(|(index, chunk)| {
            let part = index + 1;
            let marker = marker(pipeline, part);
            let of = if parts > 1 {
                format!(" (part {part} of {parts})")
            } else {
                String::new()
            };
            let mut body = format!(
                "{marker}\n\n\
                 ⚓ **minds anchor** — pipeline #{pipeline} countersigned {} seal(s) on first sight{of}.\n\n",
                chunk.len()
            );
            for (anchor, _) in chunk {
                body.push_str(&format!("- `{}`\n", anchor.seal));
            }
            body.push_str(
                "\n<details><summary>Countersignatures (minds-anchor)</summary>\n\n~~~text\n",
            );
            for (_, entry) in chunk {
                body.push_str(&entry.text);
                body.push_str(entry.signature.trim_end_matches('\n'));
                body.push('\n');
            }
            body.push_str(
                "~~~\n\n</details>\n\n\
                 <sub>Mirrored from `refs/minds/anchors/first-sight` · \
                 The repository is the source of truth; this note outlives a deleted ref.</sub>",
            );
            (marker, body)
        })
        .collect()
}

/// Die Einträge in einem Note-Text — tolerant gelesen (`\r\n`, Leerzeilen
/// davor und danach, beliebiges Markdown drumherum), ungeprüft. Nur Notes mit
/// Anker-Marker werden gelesen; jede Suche nach dem Ende einer Signatur ist
/// auf [`SIGNATURE_WINDOW`] Zeilen begrenzt (linear in der Länge der Note).
pub fn entries(body: &str) -> Vec<Entry> {
    if !body.contains(MARKER_PREFIX) {
        return Vec::new();
    }
    let lines: Vec<&str> = body.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if lines[at] != FIRST_SIGHT_VERSION || at + FIRST_SIGHT_LINES >= lines.len() {
            at += 1;
            continue;
        }
        let text: String = lines[at..at + FIRST_SIGHT_LINES]
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        let begin = at + FIRST_SIGHT_LINES;
        let window = &lines[begin..lines.len().min(begin + SIGNATURE_WINDOW)];
        let end = window
            .iter()
            .position(|line| line.starts_with("-----END SSH SIGNATURE-----"))
            .map(|offset| begin + offset);
        let signature: Option<String> = end.map(|end| {
            lines[begin..=end]
                .iter()
                .map(|line| format!("{line}\n"))
                .collect()
        });
        match (end, signature) {
            (Some(end), Some(signature)) if is_armored_signature(&signature) => {
                out.push(Entry { text, signature });
                at = end + 1;
            }
            _ => at += 1,
        }
    }
    out
}

/// Eine Note: Id, Autor (Benutzer-Id) und Text.
struct Note {
    id: Option<u64>,
    author: Option<u64>,
    body: String,
    /// `created_at`, wie GitLab ihn liefert.
    created_at: Option<String>,
    /// Der Text wurde nachträglich bearbeitet (`last_edited_at` gesetzt).
    edited: bool,
}

/// Ein Eintrag samt Herkunft, wie `verify --online` ihn braucht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedEntry {
    /// Der Eintrag.
    pub entry: Entry,
    /// Aus einem Merge Request, der in den erwarteten Branch gemergt ist.
    pub merged: bool,
    /// Wann die Note angelegt wurde (RFC 3339 laut GitLab) — `None`, wenn
    /// GitLab es nicht sagt.
    pub created_at: Option<String>,
    /// Die Note wurde nach dem Anlegen bearbeitet. Eine Anker-Note schreibt
    /// `minds anchor --mirror` einmal und nie wieder.
    pub edited: bool,
}

/// Die Notes eines JSON-Arrays von GitLab (`[{"id": …, "body": …, "author":
/// {"id": …}}]`), ohne System-Notes, und die Zahl der Elemente (für die
/// Seitenlogik).
fn notes_of(json: &str) -> Result<(Vec<Note>, usize), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "GitLab returned unreadable notes".to_owned())?;
    let array = value
        .as_array()
        .ok_or_else(|| "GitLab returned unreadable notes".to_owned())?;
    let notes = array
        .iter()
        .filter(|note| note.get("system").and_then(serde_json::Value::as_bool) != Some(true))
        .filter_map(|note| {
            Some(Note {
                id: note.get("id").and_then(serde_json::Value::as_u64),
                author: note
                    .get("author")
                    .and_then(|author| author.get("id"))
                    .and_then(serde_json::Value::as_u64),
                body: note.get("body")?.as_str()?.to_owned(),
                created_at: note
                    .get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                // `last_edited_at`, wo GitLab es liefert; sonst fail-closed
                // über `updated_at` ≠ `created_at`.
                edited: match note.get("last_edited_at") {
                    Some(value) => !value.is_null(),
                    None => {
                        let at = |key: &str| note.get(key).and_then(serde_json::Value::as_str);
                        at("updated_at").is_some_and(|updated| Some(updated) != at("created_at"))
                    }
                },
            })
        })
        .collect();
    Ok((notes, array.len()))
}

/// Je Merge Request (iid) das Ergebnis der Spiegelung — ein Fehler an
/// einem hält die übrigen nicht auf.
pub type MirrorResults = Vec<(u64, Result<Mirrored, String>)>;

/// Ein Merge Request, wie die Liste zu einem Commit ihn nennt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    /// Die iid im Projekt.
    pub iid: u64,
    /// `opened`, `merged`, `closed`, …
    pub state: String,
    /// Der Ziel-Branch.
    pub target_branch: String,
}

impl Project {
    /// Liest die Notes eines Merge Requests Seite für Seite und reicht jede
    /// an `visit` — gespeichert wird nichts. Höchstens [`MAX_NOTE_PAGES`]
    /// Seiten; ein Merge Request mit mehr Notes ist ein Fehler. Gibt die Ids
    /// in Lesereihenfolge zurück.
    fn each_note(
        &self,
        mr: u64,
        visit: &mut dyn FnMut(Note) -> Result<(), String>,
    ) -> Result<Vec<Option<u64>>, String> {
        let mut ids = Vec::new();
        for page in 1..=MAX_NOTE_PAGES {
            let body = self.get(&format!(
                "/projects/{}/merge_requests/{mr}/notes?per_page={PER_PAGE}&page={page}&sort=asc&order_by=created_at",
                self.project
            ))?;
            let (notes, count) = notes_of(&body)?;
            drop(body);
            for note in notes {
                ids.push(note.id);
                visit(note)?;
            }
            if count < PER_PAGE {
                return Ok(ids);
            }
        }
        Err(format!(
            "MR !{mr} has more than {} notes — not read completely",
            MAX_NOTE_PAGES * PER_PAGE
        ))
    }

    /// Die Benutzer-Id des Tokens (`GET /user`).
    fn current_user(&self) -> Result<u64, String> {
        let body = self.get("/user")?;
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|user| user.get("id")?.as_u64())
            .ok_or_else(|| "GitLab returned no user for the token".to_owned())
    }

    /// Die Merge Requests, die den Commit `sha` enthalten — vollständig
    /// gelesen, sonst ein Fehler.
    pub fn merge_requests_of(&self, sha: &str) -> Result<Vec<MergeRequest>, String> {
        // Die SHA wird Teil eines Pfads: nur ein Git-Objektname.
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("not a commit id".into());
        }
        let unreadable = || "GitLab returned unreadable merge requests".to_owned();
        let mut out: Vec<MergeRequest> = Vec::new();
        for page in 1..=MAX_MR_PAGES {
            let body = self.get(&format!(
                "/projects/{}/repository/commits/{sha}/merge_requests?per_page={PER_PAGE}&page={page}",
                self.project
            ))?;
            let value: serde_json::Value = serde_json::from_str(&body).map_err(|_| unreadable())?;
            let array = value.as_array().ok_or_else(unreadable)?;
            out.extend(array.iter().filter_map(|mr| {
                let text = |key: &str| {
                    mr.get(key)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                Some(MergeRequest {
                    iid: mr.get("iid")?.as_u64()?,
                    state: text("state"),
                    target_branch: text("target_branch"),
                })
            }));
            if array.len() < PER_PAGE {
                out.sort_by_key(|mr| mr.iid);
                out.dedup_by_key(|mr| mr.iid);
                return Ok(out);
            }
        }
        Err(format!(
            "the commit is in more than {} merge requests — not read completely",
            MAX_MR_PAGES * PER_PAGE
        ))
    }

    /// Spiegelt `entries` an den Merge Request `mr` — **idempotent über den
    /// Inhalt**: Was schon in einer eigenen Anker-Note steht (Autor `bot`,
    /// die Note **beginnt** mit einem Anker-Marker), wird nicht noch einmal
    /// geschickt; der Rest als neue Note(s) mit dem Marker dieser Pipeline.
    /// Eine Note eines anderen Autors oder eine, die den Marker nur zitiert
    /// (eine gespiegelte Review-Zusammenfassung desselben Bots), zählt nie —
    /// und eine eigene Note mit Marker, aber anderem Inhalt, hält nichts auf.
    fn mirror_anchors_as(
        &self,
        bot: u64,
        mr: u64,
        pipeline: u64,
        entries_to_post: &[Entry],
    ) -> Result<Mirrored, String> {
        let markers: Vec<String> = (1..=entries_to_post.len().div_ceil(MAX_PER_NOTE).max(1))
            .map(|part| marker(pipeline, part))
            .collect();
        let mut result = Mirrored {
            mr,
            posted: 0,
            kept: 0,
            foreign_markers: 0,
        };
        let mut present: std::collections::BTreeSet<(String, String)> = Default::default();
        self.each_note(mr, &mut |note| {
            if note.author == Some(bot) && note.body.starts_with(MARKER_PREFIX) {
                present.extend(
                    entries(&note.body)
                        .into_iter()
                        .map(|entry| (entry.text, entry.signature)),
                );
            } else if markers.iter().any(|m| note.body.contains(m.as_str())) {
                result.foreign_markers += 1;
            }
            Ok(())
        })?;
        let missing: Vec<Entry> = entries_to_post
            .iter()
            .filter(|entry| !present.contains(&(entry.text.clone(), entry.signature.clone())))
            .cloned()
            .collect();
        result.kept = entries_to_post.len() - missing.len();
        for (_, body) in note_bodies(pipeline, &missing) {
            let payload = serde_json::json!({ "body": body });
            self.post(
                &format!("/projects/{}/merge_requests/{mr}/notes", self.project),
                &payload.to_string(),
            )?;
        }
        result.posted = missing.len();
        Ok(result)
    }

    /// Spiegelt `entries` (Gegenzeichnungen, je mit ihrer eigenen Pipeline)
    /// mit dem Marker der Pipeline `pipeline` an die Merge Requests, über die
    /// der Commit `sha` in den Branch `branch` kam (`merged`, Ziel `branch`)
    /// — nicht an jeden, der ihn zufällig enthält: Einen Merge Request in
    /// einen alten Branch kann jeder öffnen.
    ///
    /// Ein Fehler an einem Merge Request hält die übrigen nicht auf; er steht
    /// in seinem Ergebnis.
    pub fn mirror_anchors(
        &self,
        shas: &[String],
        branch: &str,
        pipeline: u64,
        entries: &[Entry],
    ) -> Result<MirrorReport, String> {
        if note_bodies(pipeline, entries).is_empty() || shas.is_empty() {
            return Ok(MirrorReport::default());
        }
        if shas.len() > MAX_MIRROR_COMMITS {
            return Err(format!(
                "more than {MAX_MIRROR_COMMITS} commits name sessions in this push — not mirrored"
            ));
        }
        // Ein Push kann mehrere Merges bringen (ein Lauf davor scheiterte
        // oder wurde abgebrochen): Jeder Merge Request, über den ein Commit
        // des Bereichs kam, bekommt die Note — `verify --online` fragt nach
        // den Merge Requests des geprüften Commits.
        let mut all: Vec<MergeRequest> = Vec::new();
        for sha in shas {
            all.extend(self.merge_requests_of(sha)?);
        }
        all.sort_by_key(|mr| mr.iid);
        all.dedup_by_key(|mr| mr.iid);
        let merged: Vec<u64> = all
            .iter()
            .filter(|mr| mr.state == "merged" && mr.target_branch == branch)
            .map(|mr| mr.iid)
            .collect();
        let not_merged = all
            .iter()
            .filter(|mr| mr.state == "opened" && mr.target_branch == branch)
            .count();
        if merged.is_empty() {
            return Ok(MirrorReport {
                results: Vec::new(),
                not_merged,
            });
        }
        let bot = self.current_user()?;
        Ok(MirrorReport {
            results: merged
                .into_iter()
                .map(|mr| (mr, self.mirror_anchors_as(bot, mr, pipeline, entries)))
                .collect(),
            not_merged,
        })
    }

    /// Die Scopes des Tokens (`GET /personal_access_tokens/self` — auch für
    /// Projekt- und Gruppen-Tokens).
    pub fn token_scopes(&self) -> Result<Vec<String>, String> {
        let body = self.get("/personal_access_tokens/self")?;
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|token| {
                Some(
                    token
                        .get("scopes")?
                        .as_array()?
                        .iter()
                        .filter_map(|scope| scope.as_str().map(str::to_owned))
                        .collect(),
                )
            })
            .ok_or_else(|| "GitLab returned no scopes for the token".to_owned())
    }

    /// Der Default-Branch des Projekts (`GET /projects/:id`).
    fn default_branch(&self) -> Result<String, String> {
        let body = self.get(&format!("/projects/{}", self.project))?;
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|project| Some(project.get("default_branch")?.as_str()?.to_owned()))
            .ok_or_else(|| "GitLab returned no default branch for the project".to_owned())
    }

    /// Die Anker-Einträge aller Notes der Merge Requests, die den Commit
    /// `sha` enthalten (`minds verify --online`) — ungeprüft, gleich von
    /// welchem Autor: Was zählt, entscheidet die Signatur. Je Eintrag, ob er
    /// aus einem Merge Request stammt, der in `branch` gemergt ist (ohne
    /// `branch`: in den Default-Branch des Projekts) — einen in einen
    /// ungeschützten Branch gemergten kann ein Agent selbst anlegen.
    ///
    /// Gelesen wird seitenweise, und zwar **zweimal** (Liste und Notes): Wer
    /// eigene Notes löscht, während gelesen wird, verschiebt die Seiten — die
    /// Note des Bots rutschte auf eine schon gelesene. Weichen die beiden
    /// Läufe ab, ist das ein Fehler. Mehr als [`MAX_ENTRIES`] Einträge
    /// ebenso.
    pub fn anchor_entries_for_commit(
        &self,
        sha: &str,
        branch: Option<&str>,
    ) -> Result<Vec<SourcedEntry>, String> {
        let changed = || "merge request notes changed while being read — retry".to_owned();
        let mrs = self.merge_requests_of(sha)?;
        let branch = match branch {
            Some(branch) => branch.to_owned(),
            None if mrs.iter().any(|mr| mr.state == "merged") => self.default_branch()?,
            None => String::new(),
        };
        let mut out = Vec::new();
        for mr in &mrs {
            let merged = mr.state == "merged" && mr.target_branch == branch;
            let first = self.each_note(mr.iid, &mut |note| {
                out.extend(entries(&note.body).into_iter().map(|entry| SourcedEntry {
                    entry,
                    merged,
                    created_at: note.created_at.clone(),
                    edited: note.edited,
                }));
                if out.len() > MAX_ENTRIES {
                    return Err(format!(
                        "more than {MAX_ENTRIES} anchor entries in merge request notes — not read completely"
                    ));
                }
                Ok(())
            })?;
            let second = self.each_note(mr.iid, &mut |_| Ok(()))?;
            if first != second {
                return Err(changed());
            }
        }
        let again: Vec<u64> = self
            .merge_requests_of(sha)?
            .iter()
            .map(|mr| mr.iid)
            .collect();
        if again != mrs.iter().map(|mr| mr.iid).collect::<Vec<_>>() {
            return Err(changed());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::stub_server;
    use minds_core::ContentHash;

    const SIG: &str =
        "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\nAAAA\n-----END SSH SIGNATURE-----\n";

    fn entry(byte: u8) -> Entry {
        Entry {
            text: FirstSight {
                seal: ContentHash::from_bytes([byte; 32]),
                project: "group/repo".into(),
                pipeline: 42,
                at: "2026-10-08T12:00:00Z".into(),
            }
            .to_text()
            .unwrap(),
            signature: SIG.into(),
        }
    }

    /// Der Rundlauf: Was die Note trägt, liest [`entries`] byte-gleich
    /// zurück — sortiert nach Seal, ohne Dubletten.
    #[test]
    fn the_note_carries_every_signed_entry_and_reads_back() {
        let input = [entry(2), entry(1), entry(2)];
        let bodies = note_bodies(42, &input);
        assert_eq!(bodies.len(), 1);
        let (marker, body) = &bodies[0];
        assert_eq!(marker, "<!-- minds:anchor:42:1 -->");
        assert!(body.starts_with(marker.as_str()), "{body}");
        assert!(
            body.contains("pipeline #42 countersigned 2 seal(s)"),
            "{body}"
        );
        assert!(body.contains(&format!("- `{}`", ContentHash::from_bytes([1; 32]))));
        assert_eq!(entries(body), vec![entry(1), entry(2)]);
        // GitLab darf Zeilenenden umschreiben.
        assert_eq!(
            entries(&body.replace('\n', "\r\n")),
            vec![entry(1), entry(2)]
        );
    }

    #[test]
    fn unparseable_entries_are_left_out_of_the_note() {
        let broken = Entry {
            text: "minds-anchor-v1\nseal=x\n".into(),
            signature: SIG.into(),
        };
        let unsigned = Entry {
            signature: "free text".into(),
            ..entry(3)
        };
        assert!(note_bodies(42, &[broken, unsigned]).is_empty());
    }

    /// Mehr als [`MAX_PER_NOTE`] Einträge verteilen sich auf Teile mit
    /// eigenem Marker.
    #[test]
    fn many_entries_are_split_into_parts() {
        let many: Vec<Entry> = (0..=MAX_PER_NOTE)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[..8].copy_from_slice(&(i as u64).to_be_bytes());
                Entry {
                    text: FirstSight {
                        seal: ContentHash::from_bytes(bytes),
                        project: "group/repo".into(),
                        pipeline: 42,
                        at: "2026-10-08T12:00:00Z".into(),
                    }
                    .to_text()
                    .unwrap(),
                    signature: SIG.into(),
                }
            })
            .collect();
        let bodies = note_bodies(42, &many);
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[1].0, "<!-- minds:anchor:42:2 -->");
        assert!(bodies[0].1.contains("(part 1 of 2)"));
        assert_eq!(entries(&bodies[0].1).len(), MAX_PER_NOTE);
        assert_eq!(entries(&bodies[1].1).len(), 1);
    }

    /// Nur Notes mit Anker-Marker werden gelesen; eine Signatur ohne Ende
    /// oder ein abgeschnittener Text ergeben keinen Eintrag.
    #[test]
    fn only_marked_complete_blocks_are_entries() {
        let block = format!("{}{SIG}", entry(1).text);
        assert!(entries(&block).is_empty());
        let marked = format!("{}\n{block}", marker(42, 1));
        assert_eq!(entries(&marked), vec![entry(1)]);
        let cut = format!("{}\n{}", marker(42, 1), &block[..block.len() - 30]);
        assert!(entries(&cut).is_empty());
    }

    const SHA: &str = "abababababababababababababababababababab";

    fn mr_list(iids: &[u64]) -> String {
        serde_json::Value::Array(
            iids.iter()
                .map(|iid| serde_json::json!({ "iid": iid, "state": "merged", "target_branch": "main" }))
                .collect(),
        )
        .to_string()
    }

    /// Eine Note als JSON-Array-Element.
    fn note(id: u64, author: u64, body: &str) -> serde_json::Value {
        serde_json::json!({ "id": id, "author": {"id": author}, "body": body, "system": false })
    }

    /// Spiegelt `entries` für Pipeline 42 nach `main`; jeder Merge Request
    /// muss gelingen.
    fn mirror(project: &Project, entries: &[Entry]) -> Vec<Mirrored> {
        project
            .mirror_anchors(&[SHA.to_owned()], "main", 42, entries)
            .unwrap()
            .results
            .into_iter()
            .map(|(_, result)| result.unwrap())
            .collect()
    }

    #[test]
    fn mirroring_posts_once_as_the_bot() {
        let (url, received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, r#"{"id":5,"username":"minds-bot"}"#.into()),
            (200, "[]".into()),
            (201, r#"{"id":1}"#.into()),
        ]);
        let project = Project::with_token(&url, "group%2Frepo", "geheim123".into());
        assert_eq!(
            mirror(&project, &[entry(1)]),
            vec![Mirrored {
                mr: 7,
                posted: 1,
                kept: 0,
                foreign_markers: 0
            }]
        );
        assert_eq!(
            received.recv().unwrap().path,
            format!(
                "/api/v4/projects/group%2Frepo/repository/commits/{SHA}/merge_requests?per_page=100&page=1"
            )
        );
        assert_eq!(received.recv().unwrap().path, "/api/v4/user");
        assert!(received.recv().unwrap().path.starts_with(
            "/api/v4/projects/group%2Frepo/merge_requests/7/notes?per_page=100&page=1"
        ));
        let post = received.recv().unwrap();
        assert_eq!(post.method, "POST");
        let payload: serde_json::Value = serde_json::from_str(&post.body).unwrap();
        let body = payload["body"].as_str().unwrap().to_owned();
        assert_eq!(entries(&body), vec![entry(1)]);

        // Zweiter Lauf — auch in einer späteren Pipeline: Der Inhalt steht
        // schon in einer eigenen Note, also kein POST.
        let existing = serde_json::json!([note(1, 5, &body)]);
        let (url, received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, r#"{"id":5}"#.into()),
            (200, existing.to_string()),
        ]);
        let project = Project::with_token(&url, "group%2Frepo", "geheim123".into());
        let report = project
            .mirror_anchors(&[SHA.to_owned()], "main", 43, &[entry(1)])
            .unwrap();
        assert_eq!(
            report.results[0].1,
            Ok(Mirrored {
                mr: 7,
                posted: 0,
                kept: 1,
                foreign_markers: 0
            })
        );
        for _ in 0..3 {
            assert_eq!(received.recv().unwrap().method, "GET");
        }
        assert!(received.recv().is_err());
    }

    /// Nur, was noch fehlt, wird nachgeschickt.
    #[test]
    fn only_missing_entries_are_posted() {
        let body = note_bodies(42, &[entry(1)]).remove(0).1;
        let existing = serde_json::json!([note(1, 5, &body)]);
        let (url, received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, r#"{"id":5}"#.into()),
            (200, existing.to_string()),
            (201, r#"{"id":2}"#.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        assert_eq!(
            mirror(&project, &[entry(1), entry(2)])[0],
            Mirrored {
                mr: 7,
                posted: 1,
                kept: 1,
                foreign_markers: 0
            }
        );
        for _ in 0..3 {
            received.recv().unwrap();
        }
        let post = received.recv().unwrap();
        let payload: serde_json::Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(entries(payload["body"].as_str().unwrap()), vec![entry(2)]);
    }

    /// Security-Review EA-19: Ein Marker in der Note eines **anderen**
    /// Autors — oder eine eigene Note, die den Marker nur zitiert oder ohne
    /// diese Einträge trägt — hält die echte Note nicht auf.
    #[test]
    fn squatted_or_quoted_markers_do_not_stop_the_note() {
        let squats = serde_json::json!([
            note(1, 999, &marker(42, 1)),
            note(
                2,
                5,
                &format!("<!-- minds:review:x -->\n> {}", marker(42, 1))
            ),
            note(3, 5, &format!("{}\njunk", marker(42, 1))),
        ]);
        let (url, received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, r#"{"id":5}"#.into()),
            (200, squats.to_string()),
            (201, r#"{"id":4}"#.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let mirrored = mirror(&project, &[entry(1)]);
        assert_eq!(mirrored[0].posted, 1);
        assert_eq!(mirrored[0].foreign_markers, 2);
        for _ in 0..3 {
            received.recv().unwrap();
        }
        assert_eq!(received.recv().unwrap().method, "POST");
    }

    /// Nur Merge Requests, über die der Commit in den Branch kam; ein noch
    /// nicht gemergter wird gezählt (gemeldet), nicht beschrieben.
    #[test]
    fn only_merged_requests_into_the_branch_get_the_note() {
        let list = serde_json::json!([
            { "iid": 8, "state": "opened", "target_branch": "main" },
            { "iid": 9, "state": "merged", "target_branch": "old" },
        ]);
        let (url, received) = stub_server(vec![(200, list.to_string())]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let report = project
            .mirror_anchors(&[SHA.to_owned()], "main", 42, &[entry(1)])
            .unwrap();
        assert_eq!(
            report,
            MirrorReport {
                results: Vec::new(),
                not_merged: 1
            }
        );
        received.recv().unwrap();
        assert!(received.recv().is_err());
    }

    /// Ein Fehler an einem Merge Request hält den nächsten nicht auf.
    #[test]
    fn one_failing_merge_request_does_not_stop_the_others() {
        let (url, _received) = stub_server(vec![
            (200, mr_list(&[7, 8])),
            (200, r#"{"id":5}"#.into()),
            (500, r#"{"message":"boom"}"#.into()),
            (200, "[]".into()),
            (201, r#"{"id":4}"#.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let results = project
            .mirror_anchors(&[SHA.to_owned()], "main", 42, &[entry(1)])
            .unwrap()
            .results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 7);
        assert!(results[0].1.as_ref().unwrap_err().contains("500"));
        assert_eq!(results[1].0, 8);
        assert_eq!(results[1].1.as_ref().unwrap().posted, 1);
    }

    /// Eine volle Seite heißt: weiterlesen — die Note kann auf Seite 2
    /// stehen.
    #[test]
    fn the_idempotency_check_reads_every_page() {
        let filler: Vec<serde_json::Value> = (0..PER_PAGE as u64)
            .map(|i| note(i, 1, &format!("note {i}")))
            .collect();
        let body = note_bodies(42, &[entry(1)]).remove(0).1;
        let second = serde_json::json!([note(1000, 5, &body)]);
        let (url, received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, r#"{"id":5}"#.into()),
            (200, serde_json::Value::Array(filler).to_string()),
            (200, second.to_string()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        assert_eq!(mirror(&project, &[entry(1)])[0].kept, 1);
        received.recv().unwrap();
        received.recv().unwrap();
        assert!(received.recv().unwrap().path.contains("page=1"));
        assert!(received.recv().unwrap().path.contains("page=2"));
        assert!(received.recv().is_err());
    }

    /// Security-Review EA-19: Mehr Merge Requests, als gelesen werden, sind
    /// ein Fehler — nie „nichts gefunden".
    #[test]
    fn too_many_merge_requests_are_an_error() {
        let full = mr_list(&(1..=PER_PAGE as u64).collect::<Vec<_>>());
        let pages = (0..MAX_MR_PAGES).map(|_| (200, full.clone())).collect();
        let (url, _received) = stub_server(pages);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let err = project
            .anchor_entries_for_commit(SHA, Some("main"))
            .unwrap_err();
        assert!(err.contains("not read completely"), "{err}");
    }

    #[test]
    fn verify_reads_the_notes_of_every_merge_request_of_the_commit() {
        let body = note_bodies(42, &[entry(1)]).remove(0).1;
        let notes = serde_json::json!([
            note(1, 5, &body),
            { "id": 2, "body": format!("{}\n{}{SIG}", marker(9, 1), entry(2).text), "system": true },
        ])
        .to_string();
        let list = r#"[{"iid":7,"state":"opened","target_branch":"main"}]"#;
        let (url, received) = stub_server(vec![
            (200, list.into()),
            (200, notes.clone()),
            (200, notes),
            (200, list.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        assert_eq!(
            project
                .anchor_entries_for_commit(SHA, Some("main"))
                .unwrap()
                .into_iter()
                .map(|sourced| (sourced.entry, sourced.merged))
                .collect::<Vec<_>>(),
            vec![(entry(1), false)]
        );
        assert_eq!(
            received.recv().unwrap().path,
            format!(
                "/api/v4/projects/1/repository/commits/{SHA}/merge_requests?per_page=100&page=1"
            )
        );
        assert!(
            received
                .recv()
                .unwrap()
                .path
                .contains("/merge_requests/7/notes")
        );
        assert!(
            project
                .anchor_entries_for_commit("HEAD/../x", None)
                .is_err()
        );
    }

    /// Security-Review EA-19: „gemergt" heißt in den erwarteten Branch —
    /// ohne Angabe in den Default-Branch des Projekts.
    #[test]
    fn merged_means_merged_into_the_default_branch() {
        let body = note_bodies(42, &[entry(1)]).remove(0).1;
        let notes = serde_json::json!([note(1, 5, &body)]).to_string();
        let list = r#"[{"iid":7,"state":"merged","target_branch":"feature"}]"#;
        let (url, received) = stub_server(vec![
            (200, list.into()),
            (200, r#"{"default_branch":"main"}"#.into()),
            (200, notes.clone()),
            (200, notes),
            (200, list.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        assert_eq!(
            project
                .anchor_entries_for_commit(SHA, None)
                .unwrap()
                .into_iter()
                .map(|sourced| (sourced.entry, sourced.merged))
                .collect::<Vec<_>>(),
            vec![(entry(1), false)]
        );
        received.recv().unwrap();
        assert_eq!(received.recv().unwrap().path, "/api/v4/projects/1");
    }

    /// Security-Review EA-19: Verschwindet zwischen zwei Läufen eine Note
    /// (die Seiten verschieben sich), ist das ein Fehler — nie still.
    #[test]
    fn notes_that_change_while_being_read_are_an_error() {
        let before = serde_json::json!([note(1, 9, "a"), note(2, 5, "b")]).to_string();
        let after = serde_json::json!([note(2, 5, "b")]).to_string();
        let (url, _received) = stub_server(vec![(200, mr_list(&[7])), (200, before), (200, after)]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let err = project
            .anchor_entries_for_commit(SHA, Some("main"))
            .unwrap_err();
        assert!(err.contains("changed while being read"), "{err}");
    }

    /// Viele Textzeilen ohne Signatur kosten nur ihr Fenster, nicht die
    /// ganze Note.
    #[test]
    fn the_scan_for_a_signature_is_bounded() {
        let text = entry(1).text;
        let mut body = format!("{}\n", marker(42, 1));
        for _ in 0..2_000 {
            body.push_str(&text);
        }
        body.push_str(&"filler\n".repeat(SIGNATURE_WINDOW));
        body.push_str(SIG);
        assert!(entries(&body).is_empty());
        let near = format!("{}\n{text}{SIG}", marker(42, 1));
        assert_eq!(entries(&near), vec![entry(1)]);
    }

    #[test]
    fn an_error_on_mirroring_never_names_the_token() {
        let (url, _received) = stub_server(vec![(
            401,
            r#"{"message":"401 Unauthorized","echo":"geheim123"}"#.into(),
        )]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let err = project
            .mirror_anchors(&[SHA.to_owned()], "main", 42, &[entry(1)])
            .unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(!err.contains("geheim123"), "{err}");
    }

    /// Security-Review EA-19: Bringt ein Push zwei Merges, bekommen beide
    /// Merge Requests die Note — `verify` fragt je Commit nach seinen.
    #[test]
    fn every_merge_of_the_push_gets_the_note() {
        let other = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let (url, _received) = stub_server(vec![
            (200, mr_list(&[7])),
            (200, mr_list(&[8])),
            (200, r#"{"id":5}"#.into()),
            (200, "[]".into()),
            (201, r#"{"id":1}"#.into()),
            (200, "[]".into()),
            (201, r#"{"id":2}"#.into()),
        ]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        let report = project
            .mirror_anchors(&[SHA.to_owned(), other.to_owned()], "main", 42, &[entry(1)])
            .unwrap();
        let mrs: Vec<u64> = report.results.iter().map(|(mr, _)| *mr).collect();
        assert_eq!(mrs, vec![7, 8]);
        assert!(
            report
                .results
                .iter()
                .all(|(_, r)| r.as_ref().unwrap().posted == 1)
        );
    }

    #[test]
    fn token_scopes_are_read() {
        let (url, _received) = stub_server(vec![(200, r#"{"id":1,"scopes":["read_api"]}"#.into())]);
        let project = Project::with_token(&url, "1", "geheim123".into());
        assert_eq!(project.token_scopes().unwrap(), vec!["read_api".to_owned()]);
    }
}
