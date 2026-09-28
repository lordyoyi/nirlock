//! Persistent policy: what the daemon refuses to try, and why.
//!
//! Two layers (DESIGN §6, simplified for v1 to what the user asked for):
//!
//! * **Per lock session** — a handful of attempts, then password only for
//!   that session. That one lives in the lock plugin: it is UX, and a fresh
//!   budget cannot be obtained without unlocking first.
//! * **Persistent, here** — consecutive recognition failures across lock
//!   sessions, daemon restarts and reboots. This is the layer a physical
//!   attacker meets: re-locking, killing the daemon or unplugging the
//!   laptop does not give them a fresh budget.
//!
//! Freshness (forcing a typed password every N hours even while the face
//! works) is **off** by the user's decision on 2026-09-27: the disk is
//! LUKS-encrypted, so a stolen laptop that is rebooted asks for the
//! passphrase and the face never enters the picture. What "off" gives up is
//! the bound on how long a working spoof stays useful on a laptop taken
//! while running and locked — and the printed-photo attack is the one we
//! have not been able to test (DESIGN §12 risk 1).

use std::path::{Path, PathBuf};

/// What the policy allows right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allow {
    Yes,
    /// Too many failures in a row; only a password, a reboot or
    /// `nirlockctl reset-lockout` brings the face back.
    LockedOut,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Consecutive recognition failures before the face is switched off.
    pub max_consecutive_failures: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 10,
        }
    }
}

/// The part that must survive a restart. Written atomically; a corrupt or
/// unreadable file is treated as "locked out", never as "allowed", so that
/// deleting it is not a way to clear a lockout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub consecutive_failures: u32,
    /// Boot this state was last written in. A new boot resets the lockout
    /// because booting this machine requires the LUKS passphrase, which is
    /// a stronger proof of presence than anything the face can offer.
    pub boot_id: String,
    pub corrupt: bool,
}

pub struct Policy {
    path: PathBuf,
    limits: Limits,
    state: State,
    boot_id: String,
}

impl Policy {
    pub fn load(path: &Path, limits: Limits) -> Self {
        let boot_id = current_boot_id();
        let mut state = match std::fs::read_to_string(path) {
            Ok(text) => parse(&text).unwrap_or(State {
                corrupt: true,
                // A file we cannot read is not an invitation to start over.
                consecutive_failures: limits.max_consecutive_failures,
                boot_id: boot_id.clone(),
            }),
            // No file at all is a first run, not a tampered one. Anything
            // else — permissions, I/O, a directory where a file belongs — is
            // NOT a first run, and treating it as one reset the counter to
            // zero and handed back the attempts the policy exists to take
            // away. Only ENOENT means fresh.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                boot_id: boot_id.clone(),
                ..State::default()
            },
            Err(e) => {
                eprintln!(
                    "nirlockd: cannot read the lockout counter at {}: {e}. \
                     Face unlock is off until this is readable; the password is unaffected.",
                    path.display()
                );
                State {
                    corrupt: true,
                    consecutive_failures: limits.max_consecutive_failures,
                    boot_id: boot_id.clone(),
                }
            }
        };
        if !state.corrupt && state.boot_id != boot_id {
            // New boot: the LUKS passphrase was typed on the way here.
            state.consecutive_failures = 0;
            state.boot_id = boot_id.clone();
        }
        Self {
            path: path.to_path_buf(),
            limits,
            state,
            boot_id,
        }
    }

    pub fn allow(&self) -> Allow {
        if self.state.consecutive_failures >= self.limits.max_consecutive_failures {
            Allow::LockedOut
        } else {
            Allow::Yes
        }
    }

    pub fn failures(&self) -> u32 {
        self.state.consecutive_failures
    }

    pub fn remaining(&self) -> u32 {
        self.limits
            .max_consecutive_failures
            .saturating_sub(self.state.consecutive_failures)
    }

    /// A request that scored at least one frame below the threshold. Frames
    /// that never reached the embedder (nobody in view, lid shut, camera
    /// busy) are not failures: they say nothing about who was there.
    pub fn record_failure(&mut self) {
        self.state.consecutive_failures = self.state.consecutive_failures.saturating_add(1);
        self.state.boot_id = self.boot_id.clone();
        self.state.corrupt = false;
        self.save();
    }

    pub fn record_success(&mut self) {
        self.state.consecutive_failures = 0;
        self.state.boot_id = self.boot_id.clone();
        self.state.corrupt = false;
        self.save();
    }

    fn save(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        let text = format!(
            "{{\"consecutive_failures\":{},\"boot_id\":\"{}\"}}\n",
            self.state.consecutive_failures, self.state.boot_id
        );
        // Write-then-rename so a crash mid-write cannot leave a file that
        // parses as "no failures".
        // A failure here is reported, not swallowed. It is deliberately not
        // a lockout: a full or read-only disk would then lock the user out of
        // their own screen, and the evasion it would buy an attacker —
        // restarting the daemon to drop the in-memory count — already needs
        // root, which is past everything this policy defends. But it must be
        // visible, because from the outside the only symptom is a counter
        // that quietly forgets across restarts.
        let persisted = std::fs::write(&tmp, text).is_ok() && {
            match std::fs::rename(&tmp, &self.path) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("nirlockd: could not persist the lockout counter: {e}");
                    false
                }
            }
        };
        if !persisted {
            eprintln!(
                "nirlockd: the lockout counter at {} is NOT durable; it will reset if the daemon restarts",
                self.path.display()
            );
        }
    }
}

fn current_boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Deliberately tiny: this file is written by us and read by us, and a
/// parser that accepts more than it must is a way to smuggle a reset in.
fn parse(text: &str) -> Option<State> {
    let field = |key: &str| -> Option<&str> {
        let pat = format!("\"{key}\":");
        let rest = text.split_once(&pat)?.1.trim_start();
        let end = rest.find([',', '}'])?;
        Some(rest[..end].trim().trim_matches('"'))
    };
    Some(State {
        consecutive_failures: field("consecutive_failures")?.parse().ok()?,
        boot_id: field("boot_id")?.to_string(),
        corrupt: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("nirlock-policy-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn counts_failures_and_locks_out() {
        let p = tmp("lockout");
        let mut pol = Policy::load(&p, Limits::default());
        assert_eq!(pol.allow(), Allow::Yes);
        for i in 1..10 {
            pol.record_failure();
            assert_eq!(pol.allow(), Allow::Yes, "still allowed after {i}");
        }
        pol.record_failure();
        assert_eq!(pol.allow(), Allow::LockedOut);
        assert_eq!(pol.remaining(), 0);
        // An accept clears it.
        pol.record_success();
        assert_eq!(pol.allow(), Allow::Yes);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_lockout_survives_a_restart_but_not_a_reboot() {
        let p = tmp("restart");
        {
            let mut pol = Policy::load(&p, Limits::default());
            for _ in 0..10 {
                pol.record_failure();
            }
            assert_eq!(pol.allow(), Allow::LockedOut);
        }
        // Same boot: killing the daemon changes nothing.
        assert_eq!(
            Policy::load(&p, Limits::default()).allow(),
            Allow::LockedOut
        );
        // A different boot: getting here needed the LUKS passphrase.
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(
            &p,
            text.replace(&current_boot_id(), "00000000-0000-0000-0000-000000000000"),
        )
        .unwrap();
        assert_eq!(Policy::load(&p, Limits::default()).allow(), Allow::Yes);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_unreadable_state_file_locks_out_rather_than_opening_up() {
        let p = tmp("corrupt");
        std::fs::write(&p, "{ not json at all").unwrap();
        let pol = Policy::load(&p, Limits::default());
        assert_eq!(
            pol.allow(),
            Allow::LockedOut,
            "deleting or corrupting the file must not clear a lockout"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn round_trips_through_the_file() {
        let p = tmp("roundtrip");
        {
            let mut pol = Policy::load(&p, Limits::default());
            pol.record_failure();
            pol.record_failure();
        }
        let pol = Policy::load(&p, Limits::default());
        assert_eq!(pol.failures(), 2);
        assert_eq!(pol.remaining(), 8);
        let _ = std::fs::remove_file(&p);
    }
}
