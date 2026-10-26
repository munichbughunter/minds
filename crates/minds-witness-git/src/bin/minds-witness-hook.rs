//! `minds-witness-hook`: pre-receive-Hook für Tests und den Durchstich
//! (Spec S2 §11). Startet nur mit `"operation": "test"`.

#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(minds_witness_git::hook::run::main_with(&args));
}
