//! `cargo xtask <aufgabe>` — siehe `xtask/src/lib.rs`.

use std::process::ExitCode;

const USAGE: &str =
    "usage: cargo xtask proof-table | demo-fixture <dir> [<minds-binary>] | demo-tamper <dir>";

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
        ["demo-fixture", dir, ref rest @ ..] => {
            // Das gebaute Binary — vorher `cargo build -p minds-cli`.
            let minds = rest
                .first()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("target/debug/minds"));
            let minds = match minds.canonicalize() {
                Ok(minds) => minds,
                Err(_) => {
                    eprintln!(
                        "no minds binary at {} — cargo build -p minds-cli",
                        minds.display()
                    );
                    return ExitCode::from(2);
                }
            };
            match xtask::demo::create(std::path::Path::new(dir), &minds) {
                Ok(demo) => {
                    println!(
                        "clean commit  {}  ({} discarded approaches)",
                        demo.clean_commit, demo.discarded
                    );
                    println!("gap commit    {}", demo.gap_commit);
                    println!(
                        "now: cd {dir} && minds inspect   ·   tamper: cargo xtask demo-tamper {dir}"
                    );
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    eprintln!("demo-fixture: {why}");
                    ExitCode::FAILURE
                }
            }
        }
        ["demo-tamper", dir] => match xtask::demo::tamper(std::path::Path::new(dir)) {
            Ok(()) => {
                println!(
                    "seal material changed — minds inspect shows the banner, minds verify says TAMPERED"
                );
                ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("demo-tamper: {why}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
