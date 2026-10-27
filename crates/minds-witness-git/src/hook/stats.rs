//! `stats`: Auswertung der `push_stats`-Zeilen aus `log.jsonl` (Spec S3
//! §3.4): P50/P95/P99 der `ssh-keygen`-Laufzeit, Prozesse je Push und
//! Deadline-Verbrauch.

use std::path::Path;

/// Perzentil nach dem Nearest-Rank-Verfahren (`p` in Prozent).
pub fn percentile(sorted: &[u64], p: u32) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (u64::from(p) * sorted.len() as u64).div_ceil(100).max(1);
    sorted.get(usize::try_from(rank - 1).ok()?).copied()
}

/// Gibt die Auswertung auf stdout aus; Exit 1, wenn die Datei fehlt.
pub fn stats_main(path: &Path) -> i32 {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("minds-witness: Log nicht lesbar");
        return 1;
    };
    let mut times = Vec::new();
    let mut procs = Vec::new();
    let mut deadline = Vec::new();
    let mut totals = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(s) = v.get("push_stats") else {
            continue;
        };
        if let Some(list) = s.get("ssh_keygen_us").and_then(|x| x.as_array()) {
            times.extend(list.iter().filter_map(serde_json::Value::as_u64));
        }
        if let Some(n) = s
            .get("ssh_keygen_processes")
            .and_then(serde_json::Value::as_u64)
        {
            procs.push(n);
        }
        if let Some(n) = s
            .get("deadline_used_permille")
            .and_then(serde_json::Value::as_u64)
        {
            deadline.push(n);
        }
        if let Some(n) = s.get("total_us").and_then(serde_json::Value::as_u64) {
            totals.push(n);
        }
    }
    for list in [&mut times, &mut procs, &mut deadline, &mut totals] {
        list.sort_unstable();
    }
    let show = |name: &str, list: &[u64], unit: &str| {
        let p = |q| percentile(list, q).map_or("-".to_owned(), |v| format!("{v}{unit}"));
        println!(
            "{name}: n={} P50={} P95={} P99={} max={}",
            list.len(),
            p(50),
            p(95),
            p(99),
            list.last().map_or("-".to_owned(), |v| format!("{v}{unit}"))
        );
    };
    println!("pushes: {}", totals.len());
    show("ssh-keygen je Prozess", &times, "us");
    show("ssh-keygen-Prozesse je Push", &procs, "");
    show("Deadline-Verbrauch je Push", &deadline, "‰");
    show("pre-receive gesamt", &totals, "us");
    0
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50), Some(50));
        assert_eq!(percentile(&v, 95), Some(95));
        assert_eq!(percentile(&v, 99), Some(99));
        assert_eq!(percentile(&[7], 99), Some(7));
        assert_eq!(percentile(&[], 50), None);
    }
}
