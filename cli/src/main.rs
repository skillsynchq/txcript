//! The `txcript` binary: parse the command line, hand it to the library.

use clap::Parser;

fn main() -> std::process::ExitCode {
    txcript_cli::run(txcript_cli::Cli::parse())
}
