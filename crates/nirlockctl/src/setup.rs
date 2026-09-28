//! `nirlockctl setup`: the one screen a person needs.
//!
//! This is what Omarchy's menu opens (Setup → Security → Face Unlock) and
//! what the README tells everyone else to run. It is a plain numbered menu
//! rather than a full-screen TUI: it has to work in a floating terminal, in
//! a TTY and over SSH, and a list of choices needs no cursor addressing.
//!
//! It runs as the person, not as root. Actions that need root re-invoke
//! `nirlockctl` through `sudo`, so the password prompt appears where they
//! can answer it — rather than demanding the whole session be root, which
//! would then write root-owned files around their home.

use std::io::{BufRead as _, Write as _};
use std::process::Command;

use crate::status::{self, Status};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Socket of the daemon.
    #[arg(long, default_value = "/run/nirlock/sock")]
    pub socket: String,
    /// Print the status and exit, for scripts and for the menu's `when:`.
    #[arg(long)]
    pub status: bool,
    /// Add (or refresh) the Omarchy menu entry under Setup → Security.
    /// A no-op on systems without Omarchy.
    #[arg(long)]
    pub install_menu: bool,
    /// Remove the Omarchy menu entry.
    #[arg(long)]
    pub remove_menu: bool,
}

/// The Omarchy menu merges `~/.config/omarchy/extensions/omarchy-menu.jsonc`
/// over its own definition, so an entry can be added without touching any
/// package-owned file. The markers follow the convention the `hey` CLI
/// already uses in that same file: everything between them belongs to us
/// and is replaced wholesale, everything outside is the user's and is never
/// touched.
const MENU_BEGIN: &str = "// >>> nirlock — managed by `nirlockctl setup --install-menu`, do not edit between the markers";
const MENU_END: &str = "// <<< nirlock";
const MENU_ENTRY: &str = r#"  "setup.security.face": {"icon":"󰈻","label":"Face Unlock","description":"Enrol your face and turn IR unlock on or off","when":"command -v nirlockctl >/dev/null","action":"omarchy-launch-floating-terminal-with-presentation 'nirlockctl setup'"},"#;

fn menu_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/omarchy/extensions/omarchy-menu.jsonc"))
}

/// Rewrites only our block. Returns what to tell the user.
fn write_menu(install: bool) -> String {
    if !omarchy_present() {
        return "No Omarchy here, so there is no menu to add to — `nirlockctl setup` is the way in.".into();
    }
    let Some(path) = menu_path() else {
        return "HOME is not set; cannot find the menu file.".into();
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "{
}
"
        .to_string()
    });
    // Drop any previous block of ours, wherever it sits.
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in existing.lines() {
        if line.trim_start().starts_with(MENU_BEGIN) {
            inside = true;
            continue;
        }
        if inside {
            if line.trim_start().starts_with(MENU_END) {
                inside = false;
            }
            continue;
        }
        kept.push(line);
    }
    let mut text = kept.join(
        "
",
    );
    if !text.trim_end().ends_with('}') {
        text = "{
}"
        .to_string();
    }
    if install {
        // Insert just after the opening brace, which the merge accepts
        // regardless of order.
        let Some(at) = text.find('{') else {
            return format!(
                "{} does not look like a JSONC object; left alone.",
                path.display()
            );
        };
        let block = format!(
            "
  {MENU_BEGIN}
{MENU_ENTRY}
  {MENU_END}"
        );
        text.insert_str(at + 1, &block);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(
        &path,
        format!(
            "{}
",
            text.trim_end()
        ),
    ) {
        return format!("could not write {}: {e}", path.display());
    }
    if install {
        format!(
            "Added to the Omarchy menu: Setup → Security → Face Unlock
({})",
            path.display()
        )
    } else {
        format!("Removed from the Omarchy menu ({})", path.display())
    }
}

pub fn run(a: &Args) -> i32 {
    if a.install_menu || a.remove_menu {
        println!("{}", write_menu(a.install_menu));
        return 0;
    }
    if a.status {
        let s = status::read(&a.socket);
        println!("{}", s.summary());
        return i32::from(!s.usable());
    }
    loop {
        let s = status::read(&a.socket);
        show(&s);
        match prompt() {
            Some('1') => enrol(&s),
            Some('2') => test(&a.socket),
            Some('3') => forget(&s),
            Some('4') => lock_screen_toggle(&s),
            Some('5') => diagnose(),
            Some('q') | None => return 0,
            _ => println!("\n  Not one of the choices.\n"),
        }
    }
}

fn show(s: &Status) {
    println!("\x1b[2J\x1b[H\x1b[1m  nirlock — face unlock\x1b[0m\n");
    let tick = |ok: bool| {
        if ok {
            "\x1b[32m✓\x1b[0m"
        } else {
            "\x1b[31m✗\x1b[0m"
        }
    };
    println!(
        "  {} Daemon                {}",
        tick(s.daemon),
        s.daemon_note()
    );
    println!(
        "  {} Your face             {}",
        tick(s.enrolled),
        s.enrol_note()
    );
    if let Some(plugin) = s.lock_plugin {
        println!(
            "  {} Lock screen           {}",
            tick(plugin),
            if plugin {
                "face unlock is on"
            } else {
                "off — the lock screen asks for the password only"
            }
        );
    }
    println!();
    println!(
        "  1  {}",
        if s.enrolled {
            "Enrol my face again"
        } else {
            "Enrol my face"
        }
    );
    println!("  2  Try it now");
    if s.enrolled {
        println!("  3  Forget my face");
    }
    if s.lock_plugin.is_some() {
        println!(
            "  4  Turn face unlock {} for the lock screen",
            if s.lock_plugin == Some(true) {
                "off"
            } else {
                "on"
            }
        );
    }
    println!("  5  Show what happened recently");
    println!("  q  Quit\n");
}

fn prompt() -> Option<char> {
    print!("  > ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
        return None;
    }
    line.trim().chars().next()
}

fn run_visible(cmd: &mut Command) {
    println!();
    match cmd.status() {
        Ok(st) if st.success() => {}
        Ok(st) => println!("\n  (exited with {})", st.code().unwrap_or(-1)),
        Err(e) => println!("\n  could not run it: {e}"),
    }
    pause();
}

fn pause() {
    print!("\n  Press Enter to go back ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}

fn enrol(s: &Status) {
    let mut c = Command::new("sudo");
    c.arg("nirlockctl").arg("enroll");
    if s.enrolled {
        c.arg("--force");
    }
    run_visible(&mut c);
}

fn forget(s: &Status) {
    let Some(user) = &s.user else { return };
    println!("\n  This deletes the enrolled face for {user}.");
    print!("  Type yes to confirm: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    if line.trim() != "yes" {
        println!("  Left alone.");
        pause();
        return;
    }
    run_visible(
        Command::new("sudo")
            .arg("rm")
            .arg("-rf")
            .arg(format!("/var/lib/nirlock/templates/{user}")),
    );
}

/// A real verification through the daemon, so "try it" means the same path
/// the lock screen takes rather than a separate code path that might work
/// when the real one does not.
fn test(socket: &str) {
    println!("\n  Look at the camera…\n");
    match status::verify_now(socket) {
        Ok((outcome, reason, ms)) => {
            let word = match outcome.as_str() {
                "accept" => "\x1b[32mrecognised you\x1b[0m",
                "reject" => "\x1b[33mdid not recognise you\x1b[0m",
                "locked_out" => "\x1b[31mis switched off after too many failures\x1b[0m",
                _ => "\x1b[33mcould not try\x1b[0m",
            };
            println!("  It {word} in {ms} ms ({reason}).");
            if outcome != "accept" {
                println!("\n  {}", explain(&reason));
            }
        }
        Err(e) => println!("  Could not ask the daemon: {e}"),
    }
    pause();
}

fn explain(reason: &str) -> &'static str {
    match reason {
        "no_face" => {
            "The camera saw no face. Look straight at the screen, and check the room is not pitch dark."
        }
        "no_match" => {
            "It saw a face but it did not match. If that keeps happening, enrol again (choice 1)."
        }
        "not_enrolled" => "Nobody has enrolled a face for this account yet — choice 1.",
        "too_many_failures" => {
            "Too many failures in a row. A reboot or a successful unlock brings it back."
        }
        "camera_busy" => "Another program is using the camera.",
        "timeout" => "It ran out of time. Usually that means the face was not in view.",
        _ => "See choice 5 for the detail.",
    }
}

fn lock_screen_toggle(s: &Status) {
    let on = s.lock_plugin == Some(true);
    let verb = if on { "disable" } else { "enable" };
    run_visible(Command::new(format!("omarchy-plugin-{verb}")).arg("nirlock.lock"));
}

fn diagnose() {
    println!();
    let out = Command::new("journalctl")
        .args(["-u", "nirlockd", "-n", "15", "--no-pager"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let lines: Vec<&str> = text.lines().filter(|l| l.contains("verify ")).collect();
            if lines.is_empty() {
                println!("  No unlock attempts recorded yet.");
            } else {
                for l in lines.iter().rev().take(8).rev() {
                    println!("  {}", l.split_once("]: ").map_or(*l, |(_, r)| r));
                }
            }
        }
        _ => println!("  Could not read the log (journalctl -u nirlockd)."),
    }
    pause();
}

/// Is there a lock screen we know how to switch? Only Omarchy, for now.
pub fn omarchy_present() -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v omarchy-plugin-list >/dev/null")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
