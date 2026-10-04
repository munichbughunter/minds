use minds_core::ContentHash;
use minds_core::evidence::{
    CTX_CHAIN, ChainFolder, ChainItem, Coverage, FolderState, GapRecord, chain, chain_salted,
};

// Wie im Redaction-Korpus: reproduzierbare Property-Tests ohne neue Dependency.
// SplitMix64 erzeugt Fälle aus einem festen Seed, einschließlich beliebiger
// Reihenfolgen, Duplikate und der u64-Grenzen.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn bytes(&mut self) -> [u8; 32] {
        let mut bytes = [0; 32];
        for chunk in bytes.chunks_exact_mut(8) {
            chunk.copy_from_slice(&self.next().to_le_bytes());
        }
        bytes
    }

    fn seq(&mut self) -> u64 {
        match self.next() % 4 {
            0 => 0,
            1 => u64::MAX,
            _ => self.next(),
        }
    }

    fn item(&mut self) -> ChainItem {
        match self.next() % 4 {
            0 => ChainItem::Event {
                seq: self.seq(),
                hash: ContentHash::from_bytes(self.bytes()),
            },
            1 => ChainItem::PreChain { seq: self.seq() },
            2 => {
                let a = self.seq();
                let b = self.seq();
                ChainItem::Gap(GapRecord::Missing {
                    from: a.min(b),
                    to: a.max(b),
                })
            }
            _ => ChainItem::Gap(GapRecord::Damaged {
                seq: (self.next() % 2 == 0).then(|| self.seq()),
                bytes: (self.next() % 2 == 0).then(|| ContentHash::from_bytes(self.bytes())),
            }),
        }
    }
}

fn random_case(seed: u64) -> ([u8; 32], Vec<ChainItem>) {
    let mut rng = Rng(seed);
    let salt = rng.bytes();
    // Leere und lange Folgen unabhängig vom Zufall garantiert abdecken.
    let items = (0..seed % 65).map(|_| rng.item()).collect();
    (salt, items)
}

#[test]
fn folder_equals_batch_fold() {
    for seed in 0..512 {
        let (salt, items) = random_case(seed);
        let mut salted = ChainFolder::new_salted(&salt);
        let mut unsalted = ChainFolder::new_unsalted();
        // Auch jeder Präfix muss samt Coverage gleich sein, nicht nur der Root
        // am Ende. Insbesondere darf snapshot() die Fortsetzung nicht ändern.
        for end in 0..=items.len() {
            assert_eq!(salted.snapshot(), chain_salted(&salt, &items[..end]));
            assert_eq!(unsalted.snapshot(), chain(&items[..end]));
            assert_eq!(salted.head(), salted.snapshot().root);
            assert_eq!(unsalted.head(), unsalted.snapshot().root);
            if let Some(item) = items.get(end) {
                salted.push(item);
                unsalted.push(item);
            }
        }
    }
}

fn json_roundtrip(folder: &ChainFolder) -> ChainFolder {
    let state = folder.to_state();
    let json = serde_json::to_vec(&state).unwrap();
    let restored: FolderState = serde_json::from_slice(&json).unwrap();
    assert_eq!(state, restored);
    let resumed = ChainFolder::from_state(restored);
    assert_eq!(resumed.snapshot(), folder.snapshot());
    resumed
}

fn check_recovery(salt: &[u8; 32], items: &[ChainItem]) {
    for (mut folder, expected) in [
        (ChainFolder::new_unsalted(), chain(items)),
        (ChainFolder::new_salted(salt), chain_salted(salt, items)),
    ] {
        // Jeder mögliche Crash-Punkt, einschließlich vor dem ersten und nach
        // dem letzten Glied. Ein JSON-Roundtrip übt die echte Persistenzform.
        for split in 0..=items.len() {
            let mut resumed = json_roundtrip(&folder);
            for item in &items[split..] {
                resumed.push(item);
            }
            assert_eq!(resumed.snapshot(), expected, "Crash bei Glied {split}");
            assert_eq!(resumed.head(), expected.root);
            if let Some(item) = items.get(split) {
                folder.push(item);
                // Zusätzlich wiederholte Neustarts in derselben Kette.
                folder = json_roundtrip(&folder);
            }
        }
    }
}

#[test]
fn folder_state_roundtrip_continues_identically() {
    for seed in 0..128 {
        let (salt, items) = random_case(seed);
        check_recovery(&salt, &items);
    }
    // Alle Gap-Optionen garantiert, führende Lücken sowie Some(0) vs. None
    // für first_seq und u64::MAX ohne Verlust durch eine JSON-Zahlkonvertierung.
    let mut items = vec![ChainItem::Gap(GapRecord::Missing {
        from: 0,
        to: u64::MAX,
    })];
    for seq in [None, Some(0), Some(u64::MAX)] {
        for bytes in [None, Some(ContentHash::from_bytes([0xff; 32]))] {
            items.push(ChainItem::Gap(GapRecord::Damaged { seq, bytes }));
        }
    }
    items.extend([
        ChainItem::PreChain { seq: 0 },
        ChainItem::Event {
            seq: u64::MAX,
            hash: ContentHash::from_bytes([0; 32]),
        },
        ChainItem::PreChain { seq: 17 },
    ]);
    check_recovery(&[0; 32], &items);
}

#[test]
fn folder_head_changes_on_every_push() {
    let items = [
        ChainItem::Event {
            seq: 0,
            hash: ContentHash::from_bytes([0; 32]),
        },
        ChainItem::PreChain { seq: 0 },
        ChainItem::Gap(GapRecord::Missing { from: 0, to: 0 }),
        ChainItem::Gap(GapRecord::Damaged {
            seq: None,
            bytes: None,
        }),
    ];
    for mut folder in [
        ChainFolder::new_unsalted(),
        ChainFolder::new_salted(&[0; 32]),
    ] {
        for item in &items {
            // Auch dasselbe Glied zweimal bindet jedes Mal den Vorgänger.
            for _ in 0..2 {
                let before = folder.head();
                folder.push(item);
                assert_ne!(folder.head(), before);
            }
        }
    }
}

#[test]
fn folder_empty_and_gap_only_coverage_claim_no_events() {
    let empty = Coverage {
        first_seq: 0,
        last_seq: 0,
        events: 0,
        gaps: vec![],
        pre_chain: 0,
    };
    let salt = [7; 32];
    let unsalted = ChainFolder::new_unsalted();
    let salted = ChainFolder::new_salted(&salt);
    assert_eq!(unsalted.head(), ContentHash::from_bytes([0; 32]));
    assert_eq!(
        salted.head(),
        ContentHash::from_bytes(blake3::derive_key(CTX_CHAIN, &salt))
    );
    for mut folder in [unsalted, salted] {
        assert_eq!(folder.snapshot().coverage, empty);
        let gaps = vec![
            GapRecord::Missing {
                from: 1,
                to: u64::MAX,
            },
            GapRecord::Damaged {
                seq: Some(42),
                bytes: Some(ContentHash::from_bytes([0xab; 32])),
            },
        ];
        for gap in &gaps {
            folder.push(&ChainItem::Gap(gap.clone()));
        }
        let snapshot = folder.snapshot();
        assert_eq!(
            snapshot.coverage,
            Coverage {
                gaps: gaps.clone(),
                ..empty.clone()
            }
        );
        folder = json_roundtrip(&folder);
        folder.push(&ChainItem::PreChain { seq: 0 });
        folder = json_roundtrip(&folder);
        folder.push(&ChainItem::Event {
            seq: 17,
            hash: ContentHash::from_bytes([0xcd; 32]),
        });
        assert_eq!(
            folder.snapshot().coverage,
            Coverage {
                first_seq: 0,
                last_seq: 17,
                events: 1,
                gaps,
                pre_chain: 1,
            }
        );
        // Ein zuvor gelieferter Snapshot ist vom weiteren Falten unabhängig.
        assert_eq!(snapshot.coverage.events, 0);
        assert_eq!(snapshot.coverage.pre_chain, 0);
        assert_eq!(snapshot.coverage.last_seq, 0);
    }
}
