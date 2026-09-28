//! Asking the daemon what it knows, over the same socket the PAM module
//! uses. The CLI cannot look at `/var/lib/nirlock` itself: it is 0700 and
//! owned by the daemon's own user, which is the point of the system
//! service.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

pub struct Status {
    pub daemon: bool,
    pub enrolled: bool,
    pub locked_out: bool,
    pub user: Option<String>,
    /// `None` when this is not a system with a lock screen we can switch.
    pub lock_plugin: Option<bool>,
}

impl Status {
    pub fn usable(&self) -> bool {
        self.daemon && self.enrolled && !self.locked_out
    }

    pub fn summary(&self) -> String {
        if !self.daemon {
            return "daemon not running".into();
        }
        if !self.enrolled {
            return "no face enrolled".into();
        }
        if self.locked_out {
            return "switched off after too many failures".into();
        }
        "ready".into()
    }

    pub fn daemon_note(&self) -> &'static str {
        if self.daemon {
            "running"
        } else {
            "not running — start it with: sudo systemctl start nirlockd"
        }
    }

    pub fn enrol_note(&self) -> &'static str {
        if self.locked_out {
            "switched off after too many failures"
        } else if self.enrolled {
            "enrolled"
        } else {
            "not enrolled yet"
        }
    }
}

fn hello(socket: &str) -> Option<String> {
    let s = UnixStream::connect(socket).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(2500))).ok()?;
    let mut w = s.try_clone().ok()?;
    let mut r = BufReader::new(s);
    w.write_all(
        format!(
            "{{\"t\":\"hello\",\"v\":1,\"client\":\"ctl\",\"ver\":\"{}\"}}\n",
            env!("CARGO_PKG_VERSION")
        )
        .as_bytes(),
    )
    .ok()?;
    w.flush().ok()?;
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    Some(line)
}

fn has(line: &str, needle: &str) -> bool {
    line.contains(needle)
}

pub fn read(socket: &str) -> Status {
    let welcome = hello(socket);
    let user = std::env::var("USER").ok().filter(|u| !u.is_empty());
    let (daemon, enrolled, locked_out) = match &welcome {
        Some(l) => (
            true,
            has(l, "\"reason\":\"ok\""),
            has(l, "\"reason\":\"locked_out\""),
        ),
        None => (false, false, false),
    };
    let lock_plugin = if crate::setup::omarchy_present() {
        Command::new("omarchy-plugin-list").output().ok().map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.starts_with("nirlock.lock") && l.contains("enabled"))
        })
    } else {
        None
    };
    Status {
        daemon,
        enrolled,
        locked_out,
        user,
        lock_plugin,
    }
}

/// One real verification through the daemon: the same path the lock screen
/// takes, so "try it" cannot pass while the real thing fails.
pub fn verify_now(socket: &str) -> Result<(String, String, u64), String> {
    let user = std::env::var("USER").map_err(|_| "no USER in the environment".to_string())?;
    let s = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(s);
    w.write_all(b"{\"t\":\"hello\",\"v\":1,\"client\":\"pam\",\"ver\":\"setup\"}\n")
        .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;

    let nonce: String = {
        // Not security-critical here (the daemon checks it is fresh), but
        // it must differ every time or the second try is a replay.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        format!("{t:032x}").chars().rev().take(32).collect()
    };
    w.write_all(
        format!(
            "{{\"t\":\"verify\",\"v\":1,\"nonce\":\"{nonce}\",\"user\":\"{user}\",\"lane\":\"lock\",\
             \"service\":\"nirlockctl\",\"tty\":\"\",\"rhost\":\"\",\"budget_ms\":4000}}\n"
        )
        .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    line.clear();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let field = |k: &str| -> String {
        let pat = format!("\"{k}\":\"");
        line.split_once(&pat)
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(v, _)| v.to_string())
            .unwrap_or_default()
    };
    let ms = line
        .split_once("\"ms\":")
        .and_then(|(_, r)| r.split(['}', ',']).next())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    Ok((field("outcome"), field("reason"), ms))
}
