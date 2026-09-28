//! The IPC server: one UNIX stream socket, NDJSON lines, one request in
//! flight (PROTOCOL.md §1-§8).
//!
//! Trust boundary B1 (DESIGN §1.2): every field a client sends is a hint.
//! The only identity the daemon acts on is `SO_PEERCRED`, taken by the
//! kernel at `connect(2)` and unforgeable afterwards. In particular
//! `verify.user` is checked against the peer's uid, so a uid that asks to
//! be verified as somebody else is refused before the camera is touched.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nirlock_wire::{
    Client, FaceAvailability, FaceReason, Message, Outcome, PROTO, decode, encode, pam_result_line,
};

use crate::engine::{Decision, Engine};
use crate::policy::{Allow, Policy};
use crate::sys::{peer_cred, user_name};

/// PROTOCOL §8.
const HELLO_TIMEOUT: Duration = Duration::from_millis(2_000);
const LINE_TIMEOUT: Duration = Duration::from_millis(1_000);
const BUDGET_MIN: u32 = 1_000;
const BUDGET_MAX: u32 = 4_000;
/// PROTOCOL §2: a line longer than this is refused without parsing.
const MAX_LINE: usize = 8 * 1024;

pub struct Server {
    listener: UnixListener,
    shared: Arc<Shared>,
}

struct Shared {
    /// The camera, the models and the templates. A `Mutex` is the whole of
    /// the "one request in flight" rule (DESIGN §2.8): a second `verify`
    /// finds it locked and is refused rather than queued, so no client can
    /// make another wait for the camera.
    engine: Mutex<Engine>,
    /// Nonces are single-use per boot (PROTOCOL §7).
    seen_nonces: Mutex<HashSet<String>>,
    /// Consecutive-failure lockout, persisted. Separate from the engine
    /// mutex so a locked-out request is refused without waiting for a
    /// verification that is already running.
    policy: Mutex<Policy>,
    /// Only to answer "is this person enrolled?" in `welcome`. The daemon
    /// is the only process that can look: the directory is 0700 and owned
    /// by the daemon's user, so the CLI has to ask rather than stat it.
    templates_dir: PathBuf,
    rgb: bool,
}

/// How a request ended, in the daemon's own words before it becomes a wire
/// `Outcome`.
enum Answer {
    Accept,
    Reject(&'static str),
    Unavailable(&'static str),
}

impl Server {
    /// Binds `path`, replacing a stale socket left by a crash. The socket
    /// is `0666`: `SO_PEERCRED`, not file permissions, is what decides who
    /// may do what (PROTOCOL §1), and the lock screen runs as uid 1000
    /// while the daemon does not.
    pub fn bind(
        path: &Path,
        engine: Engine,
        policy: Policy,
        templates_dir: &Path,
        rgb: bool,
    ) -> std::io::Result<Self> {
        if let Ok(md) = std::fs::metadata(path)
            && md.file_type().is_socket()
        {
            // Only ever removes a socket, never a regular file someone
            // pointed us at by mistake.
            let _ = std::fs::remove_file(path);
        }
        let listener = UnixListener::bind(path)?;
        // 0666 on purpose. The daemon runs as its own system user while the
        // lock screen, sudo and polkit all run as somebody else, so file
        // permissions cannot express who may ask — and they should not:
        // authority comes from SO_PEERCRED, which the kernel stamps on the
        // connection and the peer cannot forge (PROTOCOL §1, boundary B1).
        // A restrictive mode here would only lock out the very clients the
        // socket exists for.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        Ok(Self {
            listener,
            shared: Arc::new(Shared {
                engine: Mutex::new(engine),
                seen_nonces: Mutex::new(HashSet::new()),
                policy: Mutex::new(policy),
                templates_dir: templates_dir.to_path_buf(),
                rgb,
            }),
        })
    }

    /// Serves until the process is stopped. Each connection gets a thread;
    /// they contend only on the engine mutex.
    pub fn serve(&self) {
        for conn in self.listener.incoming() {
            let Ok(stream) = conn else { continue };
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, &shared) {
                    eprintln!("nirlockd: connection: {e}");
                }
            });
        }
    }
}

/// `encode` and `pam_result_line` both already terminate the line, so this
/// must not add a second `\n`: an extra empty line would be read by the
/// client as the *next* message and desynchronise the stream.
fn send(w: &mut UnixStream, m: &Message) -> std::io::Result<()> {
    let line = encode(m).map_err(std::io::Error::other)?;
    debug_assert!(line.ends_with('\n'));
    w.write_all(line.as_bytes())?;
    w.flush()
}

/// Reads one line, refusing to allocate more than one line's worth.
///
/// `Take` caps the bytes the reader will hand over at all, so an unterminated
/// flood costs MAX_LINE + 1 bytes rather than whatever the sender feels like.
fn read_bounded<R: BufRead>(r: &mut R, line: &mut String) -> std::io::Result<usize> {
    Read::take(r.by_ref(), MAX_LINE as u64 + 1).read_line(line)
}

fn handle(stream: UnixStream, shared: &Shared) -> std::io::Result<()> {
    let cred = peer_cred(&stream)?;
    let peer_user = user_name(cred.uid);
    let mut w = stream.try_clone()?;
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    let mut r = BufReader::new(stream);

    // --- hello -------------------------------------------------------
    // Bounded at the read, not after it. The socket is mode 0666 by design,
    // so any local user can connect; `read_line` alone grows the String to
    // whatever is sent and only then compares against MAX_LINE, and the read
    // timeout bounds each syscall rather than the whole message. A client
    // writing continuously without a newline could therefore make the daemon
    // allocate without limit and take face unlock down with it.
    let mut line = String::new();
    let n = read_bounded(&mut r, &mut line)?;
    if n == 0 || line.len() > MAX_LINE {
        return Ok(());
    }
    let client = match decode(line.trim_end().as_bytes()) {
        Ok(Message::Hello { client, .. }) => client,
        Ok(other) => {
            eprintln!(
                "nirlockd: uid {} opened with {other:?}, not hello",
                cred.uid
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!("nirlockd: uid {} sent a bad hello: {e}", cred.uid);
            return Ok(());
        }
    };
    let ready = shared.engine.try_lock().is_ok();
    let enrolled = peer_user.as_deref().is_some_and(|u| {
        !u.is_empty()
            && !u.contains(['/', '.'])
            && shared.templates_dir.join(u).join("manifest.json").exists()
    });
    let locked_out = shared
        .policy
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .allow()
        == Allow::LockedOut;
    send(
        &mut w,
        &Message::Welcome {
            v: PROTO,
            daemon: env!("CARGO_PKG_VERSION").to_string(),
            proto: PROTO,
            ready,
            face: FaceAvailability {
                available: enrolled && !locked_out,
                reason: if !enrolled {
                    FaceReason::NotEnrolled
                } else if locked_out {
                    FaceReason::LockedOut
                } else {
                    FaceReason::Ok
                },
            },
            lockout_until_ms: 0,
        },
    )?;

    // --- one request -------------------------------------------------
    r.get_ref().set_read_timeout(Some(LINE_TIMEOUT * 30))?;
    line.clear();
    let n = read_bounded(&mut r, &mut line)?;
    if n == 0 || line.len() > MAX_LINE {
        return Ok(());
    }
    let msg = match decode(line.trim_end().as_bytes()) {
        Ok(m) => m,
        Err(e) => {
            // Never close silently: a malformed request is the hardest
            // thing to diagnose from the client side, where all that shows
            // is an EOF.
            eprintln!("nirlockd: bad request from uid {}: {e}", cred.uid);
            return Ok(());
        }
    };
    if !matches!(msg, Message::Verify { .. }) {
        eprintln!(
            "nirlockd: uid {} sent {:?} where a verify was due",
            cred.uid, msg
        );
        return Ok(());
    }
    let Message::Verify {
        nonce,
        user,
        lane,
        rhost,
        budget_ms,
        ..
    } = msg
    else {
        return Ok(());
    };

    let started = Instant::now();
    let answer = authorise(
        shared,
        client,
        &cred_user(peer_user.as_deref()),
        &user,
        &lane,
        &rhost,
        &nonce,
    );
    let (outcome, reason) = match answer {
        Err(a) => finish(a),
        Ok(()) => {
            let budget = budget_ms.clamp(BUDGET_MIN, BUDGET_MAX);
            // Checked before the camera is touched: a locked-out request
            // must not light the emitter (THREAT-MODEL: the attacker learns
            // nothing and the owner sees no scanning).
            if shared
                .policy
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .allow()
                == Allow::LockedOut
            {
                eprintln!(
                    "verify uid={} user={user} -> locked_out (too many consecutive failures)",
                    cred.uid
                );
                finish_locked()
            } else {
                match shared.engine.try_lock() {
                    // Busy: another request owns the camera. Refusing beats
                    // queueing — the caller has a deadline of its own.
                    Err(_) => finish(Answer::Unavailable("busy")),
                    Ok(mut eng) => match eng.verify(&user, budget, shared.rgb) {
                        Ok(v) => {
                            // One line per request. Without the breakdown a
                            // `no_face` is indistinguishable from "the user was
                            // not looking", which is the first question asked
                            // every time face unlock does not fire.
                            //
                            // The similarity score is deliberately NOT here.
                            // THREAT-MODEL T6 mitigates the scoring-oracle
                            // attack with "no score in the journal", and this
                            // line used to print `best` to three decimals
                            // straight into it — the mitigation contradicted
                            // by its own implementation. Frame counts, timings
                            // and the pose of a rejected frame carry the
                            // diagnostics without telling an attacker how
                            // close an artefact got.
                            eprintln!(
                                "verify uid={} user={} -> {} in {:.0} ms (lit {}, scored {};                              first frame {:.0}, gate {:.0}, embed {:.0}; rgb {:?}){}",
                                cred.uid,
                                user,
                                v.decision.reason(),
                                v.decision_ms,
                                v.lit_frames,
                                v.scored_frames,
                                v.first_frame_ms,
                                v.first_gate_pass_ms,
                                v.first_embedding_ms,
                                v.rgb_assist,
                                if v.rejects.is_empty() {
                                    String::new()
                                } else {
                                    let list = v
                                        .rejects
                                        .iter()
                                        .map(|(k, n)| format!("{k}={n}"))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    // The counts alone cannot say whether a
                                    // head was turned, tilted, or outside a
                                    // limit by a hair, which is the only
                                    // question worth asking about `pose`.
                                    match v.last_reject_pose {
                                        Some((r, y, pi)) => format!(
                                            "; rejected: {list} (last roll {r:.0}° yaw {y:.2} pitch {pi:.2})"
                                        ),
                                        None => format!("; rejected: {list}"),
                                    }
                                }
                            );
                            {
                                // Charge-first (PROTOCOL §7): the counter is
                                // persisted BEFORE the answer goes out, so
                                // killing the client mid-request cannot buy a
                                // free attempt. Only a request that actually
                                // scored a face counts as a failure: nobody in
                                // view says nothing about who was there, and
                                // punishing it would let anyone lock the owner
                                // out by waving at a covered lens.
                                let mut pol =
                                    shared.policy.lock().unwrap_or_else(|e| e.into_inner());
                                match v.decision {
                                    Decision::Accept => pol.record_success(),
                                    _ if v.below_threshold > 0 => {
                                        pol.record_failure();
                                        eprintln!(
                                            "policy: {} consecutive failures, {} attempts left before the face switches off",
                                            pol.failures(),
                                            pol.remaining()
                                        );
                                    }
                                    _ => {}
                                }
                            }
                            finish(match v.decision {
                                Decision::Accept => Answer::Accept,
                                Decision::NoMatch => Answer::Reject("no_match"),
                                Decision::NoFace => Answer::Reject("no_face"),
                                Decision::Timeout => Answer::Reject("timeout"),
                            })
                        }
                        Err(e) => {
                            eprintln!("nirlockd: verify: {e}");
                            finish(Answer::Unavailable("camera_error"))
                        }
                    },
                }
            }
        }
    };

    // Exactly one `result` per `verify`, always (PROTOCOL §7). `pam`
    // clients get the fixed byte-exact form of §6.2.
    let ms = started.elapsed().as_millis() as u64;
    if client == Client::Pam {
        let line =
            pam_result_line(&nonce, &user, outcome, reason, ms).map_err(std::io::Error::other)?;
        w.write_all(line.as_bytes())?;
        w.flush()?;
    } else {
        send(
            &mut w,
            &Message::Result {
                v: PROTO,
                nonce: nonce.clone(),
                user,
                outcome,
                reason: reason.to_string(),
                ms,
            },
        )?;
    }
    Ok(())
}

fn cred_user(u: Option<&str>) -> String {
    u.unwrap_or_default().to_string()
}

/// PAM_MAXTRIES through the lane's `maxtries=die`, so the lock screen can
/// tell "the face said no" from "the face is switched off".
fn finish_locked() -> (Outcome, &'static str) {
    (Outcome::LockedOut, "too_many_failures")
}

fn finish(a: Answer) -> (Outcome, &'static str) {
    match a {
        Answer::Accept => (Outcome::Accept, "match"),
        Answer::Reject(r) => (Outcome::Reject, r),
        Answer::Unavailable(r) => (Outcome::Unavailable, r),
    }
}

/// The authority matrix of PROTOCOL §4, for the messages M3 implements.
fn authorise(
    shared: &Shared,
    client: Client,
    peer_user: &str,
    user: &str,
    lane: &str,
    rhost: &str,
    nonce: &str,
) -> Result<(), Answer> {
    // Only `pam` may ask for a verification at all.
    if client != Client::Pam {
        return Err(Answer::Unavailable("forbidden"));
    }
    // A peer may only be verified as itself. Root is allowed to act for
    // another user (sudo, polkit in v2), which PROTOCOL §4 requires to
    // carry `ruser`; v1 has no such host, so it is refused here too.
    if peer_user.is_empty() || peer_user != user {
        return Err(Answer::Reject("wrong_user"));
    }
    if lane != "lock" {
        return Err(Answer::Unavailable("forbidden"));
    }
    // A remote host never unlocks a local seat.
    if !rhost.is_empty() {
        return Err(Answer::Reject("remote"));
    }
    let mut seen = shared.seen_nonces.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.insert(nonce.to_string()) {
        return Err(Answer::Reject("replayed_nonce"));
    }
    Ok(())
}

/// Default socket path; the unit creates the directory.
pub fn default_socket() -> PathBuf {
    PathBuf::from("/run/nirlock/sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_clamped_to_the_protocol_range() {
        assert_eq!(0u32.clamp(BUDGET_MIN, BUDGET_MAX), 1_000);
        assert_eq!(2_500u32.clamp(BUDGET_MIN, BUDGET_MAX), 2_500);
        assert_eq!(60_000u32.clamp(BUDGET_MIN, BUDGET_MAX), 4_000);
    }

    #[test]
    fn answers_map_to_the_protocol_outcomes() {
        assert_eq!(finish(Answer::Accept), (Outcome::Accept, "match"));
        assert_eq!(finish(Answer::Reject("no_match")).0, Outcome::Reject);
        assert_eq!(finish(Answer::Unavailable("busy")).0, Outcome::Unavailable);
    }
}
