//! `nirlockctl`: the user/root CLI (DESIGN §1.1, §9.6). M1 adds `record`
//! (behind the `debug-dump` feature): the lab's `fuprobe record`, driven by
//! `nirlock-cam`, writing the same session layout so that the lab's
//! analysis tools work on it. The other verbs arrive with the daemon in M3.

#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};

mod enroll;
mod preview;
mod probe;
#[cfg(feature = "debug-dump")]
mod record;
mod reset;
mod setup;
mod status;

/// nirlock control CLI.
#[derive(Parser, Debug)]
#[command(name = "nirlockctl", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print the version (default when no verb is given).
    Version,
    /// Record an IR session (frames + metadata) into the lab's data
    /// directory, like `fuprobe record`.
    #[cfg(feature = "debug-dump")]
    Record(record::Args),
    /// Guided enrolment: five looks, so the face is recognised from more
    /// than the one angle it was recorded at.
    Enroll(enroll::Args),
    /// Interactive setup: status, enrol, try it, turn it on or off.
    Setup(setup::Args),
    /// Report the cameras on this machine and whether nirlock can use them.
    Probe(probe::ProbeArgs),
    /// Bring face unlock back after too many consecutive failures.
    #[command(name = "reset-lockout")]
    ResetLockout(reset::Args),
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Version) {
        Cmd::Version => println!(
            "nirlockctl {} (proto {})",
            env!("CARGO_PKG_VERSION"),
            nirlock_wire::PROTO
        ),
        Cmd::Enroll(args) => std::process::exit(enroll::run(&args)),
        Cmd::Setup(args) => std::process::exit(setup::run(&args)),
        Cmd::ResetLockout(args) => std::process::exit(reset::run(&args)),
        Cmd::Probe(args) => std::process::exit(probe::run(&args)),
        #[cfg(feature = "debug-dump")]
        Cmd::Record(args) => {
            if let Err(e) = record::run(&args) {
                eprintln!("nirlockctl record: {e}");
                std::process::exit(1);
            }
        }
    }
}
