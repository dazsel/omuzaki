//! The `omuzaki` binary. See the library for the logic: [`omuzaki_cli::app::run`].

use std::process::ExitCode;

fn main() -> ExitCode {
    omuzaki_cli::link(); // the product's ops, importers and exporters (ADR 0015)
    omuzaki_cli::app::run()
}
