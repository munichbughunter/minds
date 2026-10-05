//! Das Latenzbudget des Hooks mit Witness (EA-07, W3).
//!
//! Eigenes Test-Binary mit genau einem Test: Cargo führt Test-Binaries
//! nacheinander aus, Tests in einem Binary parallel. Hier mitzulaufen hieße,
//! gegen die Prozessstarts der Nachbartests zu messen.
//!
//! Gemessen wird der ganze Prozess, wie ein Agent ihn erlebt — Start, stdin,
//! Ende. Die drei Varianten laufen verschränkt, damit eine Lastspitze der
//! Maschine alle gleich trifft und nicht eine allein.

#![cfg(unix)]

#[path = "support/hook_witness.rs"]
mod support;

use std::os::unix::net::UnixListener;
use std::time::Duration;

use support::*;

const RUNS: usize = 200;
const BUDGET: Duration = Duration::from_millis(5);
/// Ein einzelner Ausreißer der Maschine darf den Test nicht rot machen; eine
/// echte Verschlechterung um mehr als das Budget besteht in jedem Durchgang.
const ATTEMPTS: usize = 3;

fn p99(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    // Nearest-Rank: der kleinste Wert, unter dem 99 % der Messungen liegen.
    samples[(samples.len() * 99).div_ceil(100) - 1]
}

#[test]
fn hook_latency_budget_with_witness() {
    let f = Fixture::new();
    let _daemon = f.start();
    let stale = f.dir.path().join("stale.sock");
    drop(UnixListener::bind(&stale).unwrap());

    let mut report = Vec::new();
    for attempt in 0..ATTEMPTS {
        let (mut today, mut live, mut down) = (Vec::new(), Vec::new(), Vec::new());
        for n in 0..RUNS {
            let session = format!("lat-{attempt}-{n}");
            let stdin = f.payload(
                &session,
                "PostToolUse",
                r#","tool_name":"Read","tool_input":{"file_path":"src/lib.rs"},"tool_response":"fn main() {}""#,
            );
            for (socket, samples) in [
                (None, &mut today),
                (Some(f.socket()), &mut live),
                (Some(stale.clone()), &mut down),
            ] {
                let (out, took) = f.hook(socket.as_deref(), stdin.as_bytes());
                assert_silent_success(&out);
                samples.push(took);
            }
        }
        let (today, live, down) = (p99(today), p99(live), p99(down));
        if live <= today + BUDGET && down <= today + BUDGET {
            return;
        }
        report.push(format!(
            "p99 today {today:?}, live witness {live:?}, witness down {down:?}"
        ));
    }
    panic!(
        "Latenzbudget (+{BUDGET:?} auf p99) in keinem von {ATTEMPTS} Durchgängen gehalten:\n{}",
        report.join("\n")
    );
}
