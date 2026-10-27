//! Spec S3-MVP Commit 4: geteilte, inhaltsgebundene Seal-Auswahl
//! (`minds_witness_git::selection`). Die Auswahl des Witness und die
//! Abdeckung des Server-Inspektors folgen derselben Regel.

mod common;

use std::collections::BTreeSet;

use common::*;
use minds_core::evidence::{SCOPE_AGENT_HOOKS_V1, SCOPE_WITNESS_FS_V1, SealOutcome};
use minds_core::observation::MAX_OBSERVED_BYTES;
use minds_core::observation::{ObservationReason, Observations};
use minds_witness_git::selection::{self, SealEvidence, Selection, select_seals};
use minds_witness_git::view::{MODE_BLOB, MODE_EXEC, MODE_LINK};

/// Legt Observations und Seal (wie der Witness) ab und liefert den
/// Auswahl-Kandidaten aus genau diesen Bytes.
fn own_seal(w: &mut World, obs: &Observations) -> SealEvidence {
    let bytes = obs.canonical_bytes().unwrap();
    let id = w.evidence(obs);
    let obs_id = format!("b3-{}", hex(blake3::hash(&bytes).as_bytes()));
    let text = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "witness",
    );
    SealEvidence::from_parts(&id, &text, &bytes, OBS_MAX).expect("anrechenbarer Seal")
}

/// `max_observations_bytes` des Servers (Spec S2 §9).
const OBS_MAX: u64 = 8 * 1024 * 1024;

fn set(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// Bindet genau die gewählten Seals und liefert die Abdeckung des Servers.
/// Die Bindung wird danach wieder entfernt, damit sie spätere Prüfungen
/// nicht beeinflusst (der Server vereinigt alle Bindungen eines Commits).
fn server_coverage(w: &mut World, c: &str, seals: &[String]) -> BTreeSet<String> {
    let saved = w.view.clone();
    let b = w.binding(c, PROJECT, seals);
    w.put_binding(&b);
    let t = w.touched(c);
    w.view = saved;
    t
}

/// Basis mit `a`, `b`, `d`; `c` ändert `a`, `b` und löscht `d`.
fn base(w: &mut World) -> String {
    let b = w.commit(
        &[],
        &[
            ("a", MODE_BLOB, b"a0"),
            ("b", MODE_BLOB, b"b0"),
            ("d", MODE_BLOB, b"d0"),
        ],
    );
    w.commit(&[b], &[("a", MODE_BLOB, b"a1"), ("b", MODE_BLOB, b"b1")])
}

#[test]
fn s3_29_selection_is_content_bound() {
    let mut w = World::new();
    let c = base(&mut w);
    // Drei Seals: `a` mit dem Inhalt im Commit, `b` mit dem Inhalt im
    // Commit, `b` mit anderem Inhalt; dazu die Löschung von `d`.
    let s_a = own_seal(
        &mut w,
        &observations(&[("a", Obs::Content(b"a1"))], Some(separated())),
    );
    let s_b = own_seal(
        &mut w,
        &observations(
            &[("b", Obs::Content(b"b1")), ("d", Obs::Deleted)],
            Some(separated()),
        ),
    );
    let s_other = own_seal(
        &mut w,
        &observations(&[("b", Obs::Content(b"b-other"))], Some(separated())),
    );
    let sel = select_seals(
        &w.view,
        &c,
        &[s_other.clone(), s_b.clone(), s_a.clone()],
        32,
    )
    .unwrap();
    let mut want = vec![s_a.id().to_owned(), s_b.id().to_owned()];
    want.sort();
    assert_eq!(
        sel,
        Selection {
            seals: want.clone(),
            covered: set(&["a", "b", "d"]),
            total: 3,
        }
    );
    assert!(
        !sel.seals.contains(&s_other.id().to_owned()),
        "anderer Inhalt nie gewählt"
    );
    // Vorschau = Server.
    assert_eq!(server_coverage(&mut w, &c, &sel.seals), sel.covered);

    // Nur der Seal mit fremdem Inhalt: keine Auswahl, keine Bindung.
    let sel = select_seals(&w.view, &c, &[s_other], 32).unwrap();
    assert!(sel.seals.is_empty() && sel.covered.is_empty());
    // Ein Hash deckt keine Löschung, eine Löschung keinen Inhalt.
    let mut w2 = World::new();
    let c2 = base(&mut w2);
    let wrong = own_seal(
        &mut w2,
        &observations(
            &[("d", Obs::Content(b"d0")), ("a", Obs::Deleted)],
            Some(separated()),
        ),
    );
    assert!(
        select_seals(&w2.view, &c2, &[wrong], 32)
            .unwrap()
            .seals
            .is_empty()
    );
}

#[test]
fn s3_29_only_seals_the_server_would_count() {
    let mut w = World::new();
    let obs = observations(&[("a", Obs::Content(b"a1"))], Some(separated()));
    let bytes = obs.canonical_bytes().unwrap();
    let obs_id = format!("b3-{}", hex(blake3::hash(&bytes).as_bytes()));
    let good = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: obs_id.clone(),
        },
        "witness",
    );
    let id = w.put_seal(&good, "minds-witness");
    assert!(SealEvidence::from_parts(&id, &good, &bytes, OBS_MAX).is_some());
    // Falsche Seal-Id.
    assert!(SealEvidence::from_parts(&seal_id(9), &good, &bytes, OBS_MAX).is_none());
    // Anderer Scope.
    let hooks = seal_text(
        SCOPE_AGENT_HOOKS_V1,
        SealOutcome::ObservationsStored {
            observations: obs_id,
        },
        "witness",
    );
    let hooks_id = w.put_seal(&hooks, "minds-witness");
    assert!(SealEvidence::from_parts(&hooks_id, &hooks, &bytes, OBS_MAX).is_none());
    // Observation-Bytes passen nicht zur Id im Seal.
    let other = observations(&[("a", Obs::Content(b"x"))], Some(separated()))
        .canonical_bytes()
        .unwrap();
    assert!(SealEvidence::from_parts(&id, &good, &other, OBS_MAX).is_none());
    // Nicht getrennter Witness (Profil `local`): nie anrechenbar.
    let local = observations(&[("a", Obs::Content(b"a1"))], None);
    let local_bytes = local.canonical_bytes().unwrap();
    let local_text = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: format!("b3-{}", hex(blake3::hash(&local_bytes).as_bytes())),
        },
        "witness",
    );
    let local_id = w.put_seal(&local_text, "minds-witness");
    assert!(SealEvidence::from_parts(&local_id, &local_text, &local_bytes, OBS_MAX).is_none());
    // Nicht kanonische Observation-Bytes.
    let mut spaced = bytes.clone();
    spaced.push(b'\n');
    let spaced_text = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: format!("b3-{}", hex(blake3::hash(&spaced).as_bytes())),
        },
        "witness",
    );
    let spaced_id = w.put_seal(&spaced_text, "minds-witness");
    assert!(SealEvidence::from_parts(&spaced_id, &spaced_text, &spaced, OBS_MAX).is_none());
    // Gründe außer `deleted` belegen nichts.
    let opaque = observations(
        &[("a", Obs::Reason(ObservationReason::TooLarge))],
        Some(separated()),
    );
    let mut w2 = World::new();
    let c = base(&mut w2);
    let s = own_seal(&mut w2, &opaque);
    assert!(s.states().is_empty());
    assert!(
        select_seals(&w2.view, &c, &[s], 32)
            .unwrap()
            .seals
            .is_empty()
    );
}

/// Größengrenzen des Servers gelten auch für die Auswahl; zu große Inhalte
/// und Nicht-UTF-8-Pfade sind auf beiden Seiten nie belegt.
#[test]
fn s3_29_size_limits_and_uncoverable_paths() {
    let mut w = World::new();
    let obs = observations(&[("a", Obs::Content(b"a1"))], Some(separated()));
    let bytes = obs.canonical_bytes().unwrap();
    let text = seal_text(
        SCOPE_WITNESS_FS_V1,
        SealOutcome::ObservationsStored {
            observations: format!("b3-{}", hex(blake3::hash(&bytes).as_bytes())),
        },
        "witness",
    );
    let id = w.put_seal(&text, "minds-witness");
    let len = bytes.len() as u64;
    assert!(SealEvidence::from_parts(&id, &text, &bytes, len).is_some());
    // Über `max_observations_bytes`: Der Server liest das Objekt nicht,
    // also wählt der Witness es auch nicht.
    assert!(SealEvidence::from_parts(&id, &text, &bytes, len - 1).is_none());

    // Inhalt über MAX_OBSERVED_BYTES: nie belegt, auch mit passendem Hash.
    let big = vec![b'x'; usize::try_from(MAX_OBSERVED_BYTES).unwrap() + 1];
    let parent = w.commit(&[], &[("big", MODE_BLOB, b"old")]);
    let c = w.commit(std::slice::from_ref(&parent), &[("big", MODE_BLOB, &big)]);
    let s = own_seal(
        &mut w,
        &observations(&[("big", Obs::Content(&big))], Some(separated())),
    );
    let sel = select_seals(&w.view, &c, std::slice::from_ref(&s), 32).unwrap();
    assert!(sel.seals.is_empty() && sel.covered.is_empty());
    assert!(server_coverage(&mut w, &c, &[s.id().to_owned()]).is_empty());
    // Nicht-UTF-8-Pfade (Form `\0…`, S1 v1.5 V6): nie abdeckbar.
    assert_eq!(
        selection::target(&w.view, &c, "\0big", true).unwrap(),
        selection::Target::Never
    );
}

#[test]
fn s3_29_mode_rules_match_the_server() {
    type Files<'a> = Vec<(&'a str, u32, &'a [u8])>;
    let x: &[u8] = b"x1";
    let cases: Vec<(&str, Vec<Files>, Files, bool)> = vec![
        (
            "neue 100755",
            vec![vec![("o", MODE_BLOB, b"o")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "100644 → 100755",
            vec![vec![("f", MODE_BLOB, b"x0")]],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
        (
            "100755 → 100755",
            vec![vec![("f", MODE_EXEC, b"x0")]],
            vec![("f", MODE_EXEC, x)],
            true,
        ),
        (
            "Merge 100755 / fehlt",
            vec![vec![("f", MODE_EXEC, b"x0")], vec![("p", MODE_BLOB, b"p")]],
            vec![("f", MODE_EXEC, x)],
            true,
        ),
        (
            "Symlink",
            vec![vec![("o", MODE_BLOB, b"o")]],
            vec![("f", MODE_LINK, x)],
            false,
        ),
        (
            "ohne Eltern 100755",
            vec![],
            vec![("f", MODE_EXEC, x)],
            false,
        ),
    ];
    for (name, parents, files, expected) in cases {
        let mut w = World::new();
        let ids: Vec<String> = parents.iter().map(|f| w.commit(&[], f)).collect();
        let c = w.commit(&ids, &files);
        let s = own_seal(
            &mut w,
            &observations(&[("f", Obs::Content(x))], Some(separated())),
        );
        let sel = select_seals(&w.view, &c, std::slice::from_ref(&s), 32).unwrap();
        assert_eq!(sel.covered.contains("f"), expected, "{name}");
        // Server mit demselben Seal: gleiches Ergebnis.
        assert_eq!(
            server_coverage(&mut w, &c, &[s.id().to_owned()]).contains("f"),
            expected,
            "{name} (Server)"
        );
    }
}

#[test]
fn s3_30_selection_is_deterministic_greedy_and_capped() {
    let mut w = World::new();
    let files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("f{i}"), format!("v{i}").into_bytes()))
        .collect();
    let parent = w.commit(&[], &[("keep", MODE_BLOB, b"k")]);
    let refs: Vec<(&str, u32, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), MODE_BLOB, b.as_slice()))
        .chain([("keep", MODE_BLOB, b"k".as_slice())])
        .collect();
    let c = w.commit(&[parent], &refs);
    let obs = |w: &mut World, idx: &[usize]| {
        let entries: Vec<(&str, Obs<'_>)> = idx
            .iter()
            .map(|&i| (files[i].0.as_str(), Obs::Content(files[i].1.as_slice())))
            .collect();
        own_seal(w, &observations(&entries, Some(separated())))
    };
    // big belegt 0..=3; s45 belegt 4, 5; s3 und s3b belegen nur 3 (von big
    // schon belegt); two_a und two_b belegen je 4, 5 (Gleichstand mit s45).
    let big = obs(&mut w, &[0, 1, 2, 3]);
    let s45 = obs(&mut w, &[4, 5]);
    let s3 = obs(&mut w, &[3]);
    let alt = obs(&mut w, &[5, 4]);
    let all = vec![big.clone(), s45.clone(), s3.clone(), alt.clone()];
    let sel = select_seals(&w.view, &c, &all, 32).unwrap();
    // Gierig: erst big (4), dann von s45/alt (je 2) die kleinere Id.
    let tie = std::cmp::min(s45.id().to_owned(), alt.id().to_owned());
    let mut want = vec![big.id().to_owned(), tie.clone()];
    want.sort();
    assert_eq!(sel.seals, want);
    assert_eq!(sel.covered.len(), 6);
    assert_eq!(sel.total, 6);
    // Reihenfolge der Kandidaten egal.
    let mut rev = all.clone();
    rev.reverse();
    assert_eq!(select_seals(&w.view, &c, &rev, 32).unwrap(), sel);
    // Doppelte Ids werden zusammengeführt.
    let mut dup = all.clone();
    dup.push(big.clone());
    assert_eq!(select_seals(&w.view, &c, &dup, 32).unwrap(), sel);
    // Grenze: höchstens `max` Seals, der beste zuerst.
    let one = select_seals(&w.view, &c, &all, 1).unwrap();
    assert_eq!(one.seals, vec![big.id().to_owned()]);
    assert_eq!(one.covered.len(), 4);
    assert!(select_seals(&w.view, &c, &all, 0).unwrap().seals.is_empty());
    // Vorschau = Server.
    assert_eq!(server_coverage(&mut w, &c, &sel.seals), sel.covered);
}

/// Kleiner deterministischer Zufallsgenerator (keine neue Abhängigkeit).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Zufällige Welten: Die Auswahl meldet genau die Abdeckung, die der
/// Server für eine Bindung mit den gewählten Seals berechnet; ohne Grenze
/// ist das die Abdeckung aller Kandidaten zusammen.
#[test]
fn s3_31_selection_matches_server_coverage() {
    let mut rng = Lcg(0x5eed_2026_1027);
    // Der Test darf nicht leer laufen: genug Runden mit Abdeckung und mit
    // wirksamer Grenze.
    let (mut nonempty, mut cut) = (0, 0);
    for round in 0..24 {
        let mut w = World::new();
        let n = 3 + rng.below(6) as usize;
        let names: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
        let mode = |r: u64| match r {
            0 => MODE_EXEC,
            1 => MODE_LINK,
            _ => MODE_BLOB,
        };
        let parent_files: Vec<(String, u32, Vec<u8>)> = names
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    mode(rng.below(6)),
                    format!("{p}-old").into_bytes(),
                )
            })
            .collect();
        // Unveränderte Dateien in allen Eltern und im Commit: Seals dürfen sie
        // belegen, zählen aber nie (nicht geändert).
        let unchanged: [(&str, u32, &[u8]); 2] =
            [("u0", MODE_BLOB, b"u-same"), ("u1", MODE_EXEC, b"u-same")];
        let mut pf: Vec<(&str, u32, &[u8])> = parent_files
            .iter()
            .map(|(p, m, b)| (p.as_str(), *m, b.as_slice()))
            .collect();
        pf.extend(unchanged);
        let mut parents = vec![w.commit(&[], &pf)];
        // Jede dritte Runde ein Merge mit einem zweiten Elternteil.
        let mut other_files: Vec<(String, u32, Vec<u8>)> = Vec::new();
        for p in &names {
            if rng.below(2) == 0 {
                let m = mode(rng.below(4));
                other_files.push((p.clone(), m, format!("{p}-other").into_bytes()));
            }
        }
        if round % 3 == 2 {
            let mut of: Vec<(&str, u32, &[u8])> = other_files
                .iter()
                .map(|(p, m, b)| (p.as_str(), *m, b.as_slice()))
                .collect();
            of.extend(unchanged);
            parents.push(w.commit(&[], &of));
        }
        // Im Commit: geändert (neuer Inhalt, evtl. anderer Modus) oder
        // gelöscht.
        let mut child_files: Vec<(String, u32, Vec<u8>)> = Vec::new();
        for (p, m, _) in &parent_files {
            if rng.below(4) == 0 {
                continue; // gelöscht
            }
            let m = if rng.below(5) == 0 {
                mode(rng.below(3))
            } else {
                *m
            };
            child_files.push((p.clone(), m, format!("{p}-new").into_bytes()));
        }
        let mut cf: Vec<(&str, u32, &[u8])> = child_files
            .iter()
            .map(|(p, m, b)| (p.as_str(), *m, b.as_slice()))
            .collect();
        cf.extend(unchanged);
        let c = w.commit(&parents, &cf);
        // Seals mit zufälligen Behauptungen: richtiger Inhalt, alter
        // Inhalt, Löschung.
        let k = 1 + rng.below(5) as usize;
        let mut cands = Vec::new();
        for _ in 0..k {
            let mut entries: Vec<(String, u8)> = Vec::new();
            for p in &names {
                match rng.below(5) {
                    0 => entries.push((p.clone(), 0)),
                    1 => entries.push((p.clone(), 1)),
                    2 => entries.push((p.clone(), 2)),
                    _ => {}
                }
            }
            if entries.is_empty() {
                entries.push((names[0].clone(), 0));
            }
            // Unveränderte Pfade mit genau dem Inhalt im Commit.
            for u in ["u0", "u1"] {
                if rng.below(2) == 0 {
                    entries.push((u.to_owned(), 3));
                }
            }
            let new: Vec<Vec<u8>> = entries
                .iter()
                .map(|(p, k)| match k {
                    3 => b"u-same".to_vec(),
                    _ => format!("{p}-new").into_bytes(),
                })
                .collect();
            let old: Vec<Vec<u8>> = entries
                .iter()
                .map(|(p, _)| format!("{p}-old").into_bytes())
                .collect();
            let obs_entries: Vec<(&str, Obs<'_>)> = entries
                .iter()
                .enumerate()
                .map(|(i, (p, kind))| {
                    let o = match kind {
                        0 | 3 => Obs::Content(new[i].as_slice()),
                        1 => Obs::Content(old[i].as_slice()),
                        _ => Obs::Deleted,
                    };
                    (p.as_str(), o)
                })
                .collect();
            cands.push(own_seal(
                &mut w,
                &observations(&obs_entries, Some(separated())),
            ));
        }
        let unlimited = select_seals(&w.view, &c, &cands, usize::MAX).unwrap();
        let capped = select_seals(&w.view, &c, &cands, 1).unwrap();
        // Alle Kandidaten zusammen, vom Server berechnet.
        let all: Vec<String> = cands.iter().map(|s| s.id().to_owned()).collect();
        let union = server_coverage(&mut w, &c, &all);
        assert_eq!(
            unlimited.covered, union,
            "Runde {round}: ohne Grenze = alle"
        );
        if unlimited.seals.is_empty() {
            assert!(union.is_empty());
            continue;
        }
        nonempty += 1;
        for (sel, max) in [(&unlimited, usize::MAX), (&capped, 1)] {
            // Form der Bindung: aufsteigend, ohne Doppelte, höchstens `max`.
            assert!(sel.seals.windows(2).all(|p| p[0] < p[1]), "Runde {round}");
            assert!(sel.seals.len() <= max);
            assert!(!sel.covered.contains("u0") && !sel.covered.contains("u1"));
        }
        if capped.covered.len() < unlimited.covered.len() {
            cut += 1;
        }
        assert_eq!(
            server_coverage(&mut w, &c, &capped.seals),
            capped.covered,
            "Runde {round}: Grenze 1"
        );
        assert_eq!(
            server_coverage(&mut w, &c, &unlimited.seals),
            unlimited.covered,
            "Runde {round}: ohne Grenze"
        );
    }
    assert!(
        nonempty >= 8 && cut >= 3,
        "zu schwach: {nonempty} mit Abdeckung, {cut} mit Grenze"
    );
}
