//! Reads lines of English on standard input and writes each as Kokoro's phonemes, through the same G2P the `kokoro`
//! runtime speaks from (`omuzaki_speech::g2p`): `phonemize <us_gold.json> < sentences.txt`. The lip-sync evaluation's
//! ground truth starts here (`tools/lipsync-eval/`).
#![allow(clippy::disallowed_methods, reason = "a dev tool reads the lexicon file it is given; no op runs here")]

use std::io::{BufRead, Write};
use std::process::ExitCode;

use omuzaki_speech::g2p::Lexicon;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: phonemize <us_gold.json> < lines");
        return ExitCode::FAILURE;
    };
    let lexicon = match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| Lexicon::from_json(&b).map_err(|e| e.to_string()))
    {
        Ok(lexicon) => lexicon,
        Err(error) => {
            eprintln!("{}: {error}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { return ExitCode::FAILURE };
        if line.trim().is_empty() {
            continue;
        }
        if writeln!(out, "{}", lexicon.phonemize(&line)).is_err() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
