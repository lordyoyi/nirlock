//! The one place the daemon needs libc: reading the peer credentials of a
//! connected UNIX socket. Everything a client says about itself is a hint
//! (DESIGN §1.2, boundary B1); `SO_PEERCRED` is what the kernel says, and
//! it is the only thing the authority matrix of `PROTOCOL.md` §4 trusts.

// The socket server lands next in M3; until it calls these, the compiler
// cannot see a user for them.
#![allow(dead_code)]

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// Kernel-reported identity of the process on the other end, taken at
/// `connect(2)` time: it cannot be changed afterwards by the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// Reads `SO_PEERCRED`.
///
/// `std::os::unix::net::UnixStream::peer_cred` is still unstable, so this
/// is the one `getsockopt` the daemon makes.
#[allow(unsafe_code)]
pub fn peer_cred(s: &UnixStream) -> io::Result<PeerCred> {
    let mut uc = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `uc` is a live, correctly sized `ucred` and `len` holds its
    // size; `getsockopt` writes at most `len` bytes into it and updates
    // `len`. The fd is owned by `s` and stays open for the call.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut uc).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred {
        pid: uc.pid,
        uid: uc.uid,
        gid: uc.gid,
    })
}

/// Name of `uid` from the password database, for matching the `user` field
/// of a `verify` against the peer that sent it.
pub fn user_name(uid: u32) -> Option<String> {
    // Reading /etc/passwd directly avoids pulling NSS (and its dlopen) into
    // a sandboxed daemon; local accounts are all this needs to resolve.
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        let _pw = f.next()?;
        (f.next()?.parse::<u32>().ok()? == uid).then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_cred_of_a_socketpair_is_this_process() {
        let (a, _b) = UnixStream::pair().unwrap();
        let c = peer_cred(&a).unwrap();
        assert_eq!(c.pid, std::process::id() as i32);
        // SAFETY-free: getuid/getgid have no preconditions, but the daemon
        // has no other use for them, so compare against the passwd entry.
        assert!(
            user_name(c.uid).is_some(),
            "uid {} not in /etc/passwd",
            c.uid
        );
    }

    #[test]
    fn user_name_resolves_root_and_rejects_an_impossible_uid() {
        assert_eq!(user_name(0).as_deref(), Some("root"));
        assert_eq!(user_name(u32::MAX - 1), None);
    }
}
