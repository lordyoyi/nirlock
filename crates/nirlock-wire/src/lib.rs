//! nirlock-wire: message types and NDJSON codec for `/run/nirlock/sock`.
//!
//! Normative reference: `docs/PROTOCOL.md` (version 1). M0 scope: the
//! `hello` / `welcome` / `verify` / `result` shapes, the framing limits
//! (inbound ≤ 8 KiB, outbound ≤ 16 KiB, no raw control characters), and the
//! byte-exact `result` line the C module scans (§6.2). Other message types
//! land with the daemon in M3.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Protocol version carried in every object as `"v"`.
/// Where the consecutive-failure counter lives under the packaged service.
///
/// This was two different literals in two crates: the daemon defaulted to
/// `%h/.local/state/nirlock/state.json` (expanded against the service user's
/// home, `/var/lib/nirlock`) while `nirlockctl reset-lockout` deleted
/// `/var/lib/nirlock/state.json`. The recovery command therefore removed a
/// file that was never there, printed "no lockout was recorded", and left
/// the real lockout in place — a documented, advertised recovery path that
/// silently did nothing. One constant, used by both.
pub const DEFAULT_STATE_PATH: &str = "/var/lib/nirlock/state.json";

pub const PROTO: u32 = 1;
/// Longest inbound line the daemon accepts, terminator included (§2).
pub const MAX_INBOUND_LINE: usize = 8 * 1024;
/// Longest outbound line the daemon emits, terminator included (§2).
pub const MAX_OUTBOUND_LINE: usize = 16 * 1024;

/// The peer's declared role (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Client {
    Pam,
    Lock,
    Ctl,
}

/// `welcome.face.reason` (§6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceReason {
    Ok,
    Stale,
    LockedOut,
    NotEnrolled,
    TemplateStale,
    LidClosed,
    Disabled,
    AccountLocked,
    ModelMismatch,
    CameraMissing,
    NoSession,
}

/// The `face` object of `welcome` and `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceAvailability {
    pub available: bool,
    pub reason: FaceReason,
}

/// `result.outcome` (§6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accept,
    Reject,
    LockedOut,
    Unavailable,
    Cancelled,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Accept => "accept",
            Outcome::Reject => "reject",
            Outcome::LockedOut => "locked_out",
            Outcome::Unavailable => "unavailable",
            Outcome::Cancelled => "cancelled",
        }
    }
}

/// Every message carried on the socket that M0 knows about, discriminated
/// by `"t"`. Unknown fields are ignored on input (additive evolution, §2);
/// an unknown `t` fails to parse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    Hello {
        v: u32,
        client: Client,
        ver: String,
    },
    Welcome {
        v: u32,
        daemon: String,
        proto: u32,
        ready: bool,
        face: FaceAvailability,
        lockout_until_ms: i64,
    },
    Verify {
        v: u32,
        /// 32 lowercase hex characters.
        nonce: String,
        user: String,
        lane: String,
        #[serde(default)]
        service: String,
        #[serde(default)]
        tty: String,
        #[serde(default)]
        rhost: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ruser: Option<String>,
        budget_ms: u32,
        #[serde(default)]
        progress: bool,
    },
    Result {
        v: u32,
        nonce: String,
        user: String,
        outcome: Outcome,
        reason: String,
        ms: u64,
    },
}

impl Message {
    pub fn version(&self) -> u32 {
        match self {
            Message::Hello { v, .. }
            | Message::Welcome { v, .. }
            | Message::Verify { v, .. }
            | Message::Result { v, .. } => *v,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    /// `error too_large`: the line (with `\n`) exceeds the limit.
    #[error("line too large: {len} > {max} bytes")]
    TooLarge { len: usize, max: usize },
    /// Raw control character (< 0x20 or 0x7f) outside the terminator.
    #[error("raw control character 0x{0:02x} in line")]
    ControlChar(u8),
    #[error("line is not UTF-8")]
    Utf8,
    /// `error bad_request`: not a JSON object of a known `t`.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// `error version`: `v` is not [`PROTO`].
    #[error("unsupported protocol version {0}")]
    Version(u32),
    /// An encoder input that would not survive the byte-exact `result`
    /// grammar (§6.2): `nonce` not 32 lowercase hex, `user` outside
    /// `^[a-z_][a-z0-9_-]{0,31}$`, `reason` outside `^[a-z0-9_]+$`.
    #[error("invalid {0} for the pam result line")]
    InvalidField(&'static str),
}

/// Validates framing (§2) and returns the payload without its terminator:
/// length ≤ `max` including the trailing `\n` (which may be absent when the
/// caller already split on it), no raw control characters, valid UTF-8.
pub fn check_line(line: &[u8], max: usize) -> Result<&str, CodecError> {
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let len = body.len() + 1;
    if len > max {
        return Err(CodecError::TooLarge { len, max });
    }
    if let Some(&c) = body.iter().find(|&&c| c < 0x20 || c == 0x7f) {
        return Err(CodecError::ControlChar(c));
    }
    std::str::from_utf8(body).map_err(|_| CodecError::Utf8)
}

/// Decodes one inbound line (client → daemon limits).
pub fn decode(line: &[u8]) -> Result<Message, CodecError> {
    decode_with_limit(line, MAX_INBOUND_LINE)
}

/// Decodes one line with an explicit size limit.
pub fn decode_with_limit(line: &[u8], max: usize) -> Result<Message, CodecError> {
    let body = check_line(line, max)?;
    // Check `v` before the shape so an unknown version is reported as such
    // even when `t` is unknown too.
    let probe: serde_json::Value =
        serde_json::from_str(body).map_err(|e| CodecError::BadRequest(e.to_string()))?;
    if !probe.is_object() {
        return Err(CodecError::BadRequest("not a JSON object".into()));
    }
    match probe.get("v").and_then(|v| v.as_u64()) {
        Some(v) if v == PROTO as u64 => {}
        Some(v) => return Err(CodecError::Version(v.min(u32::MAX as u64) as u32)),
        None => return Err(CodecError::BadRequest("missing v".into())),
    }
    serde_json::from_value(probe).map_err(|e| CodecError::BadRequest(e.to_string()))
}

/// Encodes one message as a line with its `\n`, enforcing the outbound limit
/// and the control-character rule (serde_json escapes them, this is a
/// belt-and-braces check).
pub fn encode(msg: &Message) -> Result<String, CodecError> {
    let mut s = serde_json::to_string(msg).map_err(|e| CodecError::BadRequest(e.to_string()))?;
    s.push('\n');
    check_line(s.as_bytes(), MAX_OUTBOUND_LINE)?;
    Ok(s)
}

/// Is `s` a valid `nonce` (32 lowercase hex characters)?
pub fn valid_nonce(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Is `s` a valid `user` (`^[a-z_][a-z0-9_-]{0,31}$`)?
pub fn valid_user(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 32 {
        return false;
    }
    let first_ok = b[0].is_ascii_lowercase() || b[0] == b'_';
    first_ok
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Is `s` a valid `reason` token (`^[a-z0-9_]+$`)?
pub fn valid_reason(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The byte-exact `result` line for `pam` connections (§6.2):
/// `{"v":1,"t":"result","nonce":"<32 hex>","user":"<user>","outcome":"<outcome>","reason":"<reason>","ms":<int>}\n`.
///
/// No quoting is applied, so every field is validated here, in release
/// builds too: the C module compares `nonce` and `user` byte for byte, and
/// an unchecked `user` of `a"b` would produce a line the scanner could be
/// made to match. Invalid input is [`CodecError::InvalidField`].
pub fn pam_result_line(
    nonce: &str,
    user: &str,
    outcome: Outcome,
    reason: &str,
    ms: u64,
) -> Result<String, CodecError> {
    if !valid_nonce(nonce) {
        return Err(CodecError::InvalidField("nonce"));
    }
    if !valid_user(user) {
        return Err(CodecError::InvalidField("user"));
    }
    if !valid_reason(reason) {
        return Err(CodecError::InvalidField("reason"));
    }
    Ok(format!(
        "{{\"v\":{PROTO},\"t\":\"result\",\"nonce\":\"{nonce}\",\"user\":\"{user}\",\"outcome\":\"{}\",\"reason\":\"{reason}\",\"ms\":{ms}}}\n",
        outcome.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn hello_welcome_roundtrip() {
        let h = Message::Hello {
            v: 1,
            client: Client::Pam,
            ver: "0.1.0".into(),
        };
        let line = encode(&h).unwrap();
        assert_eq!(
            line,
            "{\"t\":\"hello\",\"v\":1,\"client\":\"pam\",\"ver\":\"0.1.0\"}\n"
        );
        assert_eq!(decode(line.as_bytes()).unwrap(), h);
        let w = Message::Welcome {
            v: 1,
            daemon: "0.1.0".into(),
            proto: 1,
            ready: false,
            face: FaceAvailability {
                available: false,
                reason: FaceReason::NotEnrolled,
            },
            lockout_until_ms: 0,
        };
        let line = encode(&w).unwrap();
        assert!(line.contains("\"reason\":\"not_enrolled\""));
        assert_eq!(decode(line.as_bytes()).unwrap(), w);
    }

    #[test]
    fn verify_defaults_and_unknown_fields() {
        let line = format!(
            "{{\"v\":1,\"t\":\"verify\",\"nonce\":\"{NONCE}\",\"user\":\"rodrigo\",\"lane\":\"lock\",\"budget_ms\":4000,\"future_field\":[1,2]}}\n"
        );
        let m = decode(line.as_bytes()).unwrap();
        match m {
            Message::Verify {
                nonce,
                user,
                lane,
                service,
                rhost,
                ruser,
                budget_ms,
                progress,
                ..
            } => {
                assert!(valid_nonce(&nonce));
                assert_eq!(user, "rodrigo");
                assert_eq!(lane, "lock");
                assert_eq!(service, "");
                assert_eq!(rhost, "");
                assert_eq!(ruser, None);
                assert_eq!(budget_ms, 4000);
                assert!(!progress);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pam_result_line_is_byte_exact_and_parses() {
        let line = pam_result_line(NONCE, "rodrigo", Outcome::Accept, "k2", 612).unwrap();
        assert_eq!(
            line,
            format!(
                "{{\"v\":1,\"t\":\"result\",\"nonce\":\"{NONCE}\",\"user\":\"rodrigo\",\"outcome\":\"accept\",\"reason\":\"k2\",\"ms\":612}}\n"
            )
        );
        let m = decode(line.as_bytes()).unwrap();
        assert_eq!(
            m,
            Message::Result {
                v: 1,
                nonce: NONCE.into(),
                user: "rodrigo".into(),
                outcome: Outcome::Accept,
                reason: "k2".into(),
                ms: 612
            }
        );
        // The generic encoder is NOT byte-identical (serde writes the tag `t`
        // first); pam connections must use `pam_result_line`. Both decode to
        // the same message.
        let generic = encode(&m).unwrap();
        assert_ne!(generic, line);
        assert!(generic.starts_with("{\"t\":\"result\",\"v\":1,"));
        assert_eq!(decode(generic.as_bytes()).unwrap(), m);
    }

    #[test]
    fn line_limit_8kib_inbound() {
        // Exactly 8192 bytes including the terminator is accepted.
        let pad_len = MAX_INBOUND_LINE
            - 1
            - "{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\"}".len();
        let ver = "x".repeat(pad_len);
        let line = format!("{{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"{ver}\"}}\n");
        assert_eq!(line.len(), MAX_INBOUND_LINE);
        assert!(decode(line.as_bytes()).is_ok());
        // One more byte is `too_large`.
        let line = format!("{{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"{ver}x\"}}\n");
        assert_eq!(line.len(), MAX_INBOUND_LINE + 1);
        assert_eq!(
            decode(line.as_bytes()),
            Err(CodecError::TooLarge {
                len: MAX_INBOUND_LINE + 1,
                max: MAX_INBOUND_LINE
            })
        );
        // Without the terminator the same body still counts the `\n`.
        let body = format!("{{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"{ver}x\"}}");
        assert!(matches!(
            decode(body.as_bytes()),
            Err(CodecError::TooLarge { .. })
        ));
        // Outbound limit is 16 KiB.
        let big = Message::Hello {
            v: 1,
            client: Client::Ctl,
            ver: "y".repeat(MAX_OUTBOUND_LINE),
        };
        assert!(matches!(encode(&big), Err(CodecError::TooLarge { .. })));
    }

    #[test]
    fn raw_control_chars_rejected_but_escapes_pass() {
        let raw = b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"a\tb\"}\n";
        assert_eq!(decode(raw), Err(CodecError::ControlChar(0x09)));
        let nul = b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\0\"}\n";
        assert_eq!(decode(nul), Err(CodecError::ControlChar(0x00)));
        let del = b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\x7f\"}\n";
        assert_eq!(decode(del), Err(CodecError::ControlChar(0x7f)));
        let cr = b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"x\"}\r\n";
        assert_eq!(decode(cr), Err(CodecError::ControlChar(0x0d)));
        // JSON-escaped control characters are fine and round-trip.
        let esc = b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"a\\tb\\u0000\"}\n";
        match decode(esc).unwrap() {
            Message::Hello { ver, .. } => assert_eq!(ver, "a\tb\0"),
            m => panic!("{m:?}"),
        }
        let out = encode(&Message::Hello {
            v: 1,
            client: Client::Ctl,
            ver: "a\nb".into(),
        })
        .unwrap();
        assert!(out.contains("a\\nb") && !out[..out.len() - 1].contains('\n'));
        assert_eq!(
            decode(b"{\"v\":1,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\xff\"}\n"),
            Err(CodecError::Utf8)
        );
    }

    #[test]
    fn bad_request_and_version() {
        assert!(matches!(decode(b"[1,2]\n"), Err(CodecError::BadRequest(_))));
        assert!(matches!(
            decode(b"{\"v\":1,\"t\":\"nope\"}\n"),
            Err(CodecError::BadRequest(_))
        ));
        assert!(matches!(
            decode(b"{\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\"}\n"),
            Err(CodecError::BadRequest(_))
        ));
        assert_eq!(
            decode(b"{\"v\":2,\"t\":\"hello\",\"client\":\"ctl\",\"ver\":\"\"}\n"),
            Err(CodecError::Version(2))
        );
        assert!(matches!(
            decode(b"not json\n"),
            Err(CodecError::BadRequest(_))
        ));
        assert!(matches!(decode(b"\n"), Err(CodecError::BadRequest(_))));
    }

    #[test]
    fn pam_result_line_refuses_invalid_fields() {
        // A quote in `user` would forge a second field: refused, never emitted.
        assert_eq!(
            pam_result_line(NONCE, "a\"b", Outcome::Accept, "k2", 1),
            Err(CodecError::InvalidField("user"))
        );
        assert_eq!(
            pam_result_line(NONCE, "Rodrigo", Outcome::Accept, "k2", 1),
            Err(CodecError::InvalidField("user"))
        );
        assert_eq!(
            pam_result_line(&NONCE[..31], "rodrigo", Outcome::Accept, "k2", 1),
            Err(CodecError::InvalidField("nonce"))
        );
        assert_eq!(
            pam_result_line(NONCE, "rodrigo", Outcome::Reject, "no match", 1),
            Err(CodecError::InvalidField("reason"))
        );
        assert_eq!(
            pam_result_line(NONCE, "rodrigo", Outcome::Reject, "", 1),
            Err(CodecError::InvalidField("reason"))
        );
        assert!(pam_result_line(NONCE, "rodrigo", Outcome::Reject, "no_match", 1).is_ok());
    }

    #[test]
    fn validators() {
        assert!(valid_nonce(NONCE));
        assert!(
            valid_reason("k2")
                && valid_reason("no_match")
                && !valid_reason("")
                && !valid_reason("a-b")
        );
        assert!(!valid_nonce("0123456789ABCDEF0123456789abcdef"));
        assert!(!valid_nonce(&NONCE[..31]));
        assert!(valid_user("rodrigo") && valid_user("_x") && valid_user("a-b_c9"));
        assert!(
            !valid_user("") && !valid_user("Rodrigo") && !valid_user("9a") && !valid_user("a b")
        );
        assert!(valid_user(&"a".repeat(32)) && !valid_user(&"a".repeat(33)));
    }
}
