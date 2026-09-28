//! `nirlockctl reset-lockout`: bring the face back after too many failures.
//!
//! The daemon's startup message and its policy documentation both point
//! here, so it has to exist — a message that sends someone to a command
//! that does not exist is worse than no message.
//!
//! It removes the persisted counter and restarts the daemon. The restart is
//! not decoration: the daemon keeps the count in memory and rewrites the
//! file on the next failure, so deleting the file alone would be undone by
//! the very next attempt.
//!
//! Requiring root is the whole authorisation: `sudo` has already proved who
//! is asking, and a physical attacker at a locked screen has no way to run
//! it. DESIGN §6.1 wants an independent password check on top (service
//! `nirlock-admin`), which belongs with the rest of the admin verbs.

use std::path::PathBuf;
use std::process::Command;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long, default_value = nirlock_wire::DEFAULT_STATE_PATH)]
    pub state: PathBuf,
    /// Do not restart the daemon (it will keep its in-memory count).
    #[arg(long)]
    pub no_restart: bool,
}

pub fn run(a: &Args) -> i32 {
    if !nix_is_root() {
        eprintln!("this needs root, because the counter belongs to the daemon:");
        eprintln!("  sudo nirlockctl reset-lockout");
        return 1;
    }
    match std::fs::remove_file(&a.state) {
        Ok(()) => println!("cleared {}", a.state.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "no lockout was recorded ({} does not exist)",
                a.state.display()
            );
        }
        Err(e) => {
            eprintln!("could not clear {}: {e}", a.state.display());
            return 1;
        }
    }
    if a.no_restart {
        println!("not restarting; the running daemon keeps its current count.");
        return 0;
    }
    match Command::new("systemctl")
        .arg("restart")
        .arg("nirlockd")
        .status()
    {
        Ok(s) if s.success() => {
            println!("daemon restarted — face unlock is available again.");
            0
        }
        Ok(s) => {
            eprintln!(
                "systemctl restart nirlockd exited with {}",
                s.code().unwrap_or(-1)
            );
            1
        }
        Err(e) => {
            eprintln!("could not restart the daemon: {e}");
            eprintln!("run it yourself:  sudo systemctl restart nirlockd");
            1
        }
    }
}

/// `id -u` rather than libc, to keep this crate free of unsafe.
fn nix_is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "0")
        .unwrap_or(false)
}
