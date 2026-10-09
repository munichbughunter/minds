//! `cargo xtask <aufgabe>` — siehe `xtask/src/lib.rs`.

use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask proof-table";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["proof-table"] => {
            // Mit den Marken: So lässt sich der Block in der Doku 1:1 ersetzen.
            println!(
                "{}\n\n{}\n{}",
                xtask::BEGIN_MARKER,
                xtask::proof_table(),
                xtask::END_MARKER
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
