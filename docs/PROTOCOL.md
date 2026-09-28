# nirlock wire protocol — version 1 (normative)

Status: design, 2026-09-23. This document alone defines what crosses `/run/nirlock/sock`. Where DESIGN.md and this file disagree, this file wins for the wire; DESIGN.md wins for policy.

Key words MUST / MUST NOT / SHOULD are used in the RFC 2119 sense.

## 1. Transport

- Path `/run/nirlock/sock`, `AF_UNIX`, `SOCK_STREAM`, created by `nirlockd.socket` (`0666 root:root`, directory `0755`).
- Authorization is derived exclusively from `SO_PEERCRED` (`uid`, `gid`, `pid` at `accept()` time) and from the `client` role declared in `hello`. The daemon MUST NOT trust any other field for authorization. `pid` is logged, never used for policy.
- Rationale for a stream socket: `Quickshell.Io.Socket` is a `QLocalSocket` (`write(QString)`, `SplitParser`); `SOCK_SEQPACKET` is unreachable from QML.

## 2. Framing: NDJSON

- One JSON object per line, terminated by `\n` (0x0A), UTF-8, no raw control characters (JSON escapes them).
- Hard limits: inbound line ≤ 8 192 bytes, outbound line ≤ 16 384 bytes. A longer inbound line → `error too_large` and close. A line MUST be complete within 1 000 ms of its first byte or the connection is closed.
- Every object carries `"v":1` and `"t":"<type>"`. Unknown `t` → `error bad_request` + close. Unknown fields MUST be ignored (additive evolution). Unknown `v` → `error version` + close.
- No length prefix (QML only knows UTF-16 `String.length`). The line limit gives the same DoS bound.

## 3. Connection lifecycle

1. `connect()` → daemon reads peer credentials.
2. Client sends `hello` within 2 000 ms, else the daemon closes.
3. Daemon answers `welcome` **immediately** (before models load, before hashing; `ready` may be `false`).
4. Client sends requests; the daemon answers each with `ack`, a reply of the same family, `result` or `error`.
5. Closing the socket cancels any in-flight `verify` from that connection (EOF is the cancel path; SIGKILL of a PAM child produces it).

Limits: ≤ 8 connections per uid ≠ 0, ≤ 64 total, one slot always reserved for uid 0 and one for each enrolled uid; per-connection token bucket 20 messages/s with burst 50 (excess → `error rate_limited` + close); `pam` connections are closed by the daemon after their `result`.

Idle close: a connection with no in-flight request, no `subscribe` **and no lock session in state `locked`** is closed after 30 s idle. A `lock` connection that has sent `lock_session locked` is exempt from the idle close for as long as the session is `locked`, whether or not it subscribed (the reference wrapper always subscribes right after `hello`, so it also receives events).

Lock-session lifetime (normative): a lock session is per connection and per uid. It starts with `lock_session locked`, ends with `lock_session unlocked` on the same connection, or **300 s after that connection closes** without having sent `unlocked` (a crashed wrapper does not keep the daemon resident forever). A new `lock_session locked` from another connection of the same uid replaces the old one (and resets the per-lock convenience budget, `verifies_this_lock`). The daemon's residency rule (DESIGN §2.1) counts from the end of the last lock session.

Events are delivered **only** on connections that sent `subscribe`. A client that needs `resumed`, `lid_open`, `availability_changed`, `lockout_changed` or `verify_finished` MUST subscribe; there is no implicit subscription for `lock` clients.

## 4. Roles and authority matrix

`client` ∈ {`pam`, `lock`, `ctl`}. `U` = uid resolved from the `user` field with `getpwnam_r`; `P` = peer uid.

| Message | `pam`, P == U | `pam`, P == 0 | `lock`, P == U | `ctl`, P == U | `ctl`, P == 0 | anything else |
|---|---|---|---|---|---|---|
| `hello`, `ping` | yes | yes | yes | yes | yes | yes |
| `verify` | yes | yes, MUST carry `ruser` == `user` (v2 hosts) | no | no | no | `error forbidden` |
| `cancel` | own nonce | own nonce | own nonce | own nonce | own nonce | forbidden |
| `prewarm`, `lock_session`, `subscribe` | no | no | yes | yes | yes | forbidden |
| `status` | no | no | yes (own, reduced) | yes (own, reduced) | yes (full, any user) | forbidden |
| `enroll`, `enroll_abort`, `list_templates`, `delete`, `attest`, `reset_lockout`, `set_enabled`, `camera_repin`, `config_reload` | no | no | no | no | yes, only on a connection that has never sent `verify` | forbidden |

Additional conditions on `verify` for P ≠ 0: P MUST own a logind session with `Active=true`, `Seat=seat0`, class `user`; otherwise `result unavailable no_session`. The daemon MUST NOT read `service`, `trigger`, `client` or `tty` for policy; `budget_ms` is clamped to `[1000, 4000]` for lane `lock`. `rhost` MUST be empty or the daemon answers `result reject` (module also enforces).

The enrollment self-check is **not** a `verify`: `enroll` runs an internal verification phase (DESIGN §7) on the candidate template before committing it, reports it as `event enroll_progress {phase:"verify"}` and in `enroll_done {verified, verify_ms}`, and touches no failure counter or per-user quota. A `ctl` connection therefore never sends `verify`, which keeps it eligible for the admin verbs above.

## 5. Messages — client → daemon

| `t` | Fields | Notes |
|---|---|---|
| `hello` | `client` (`pam`\|`lock`\|`ctl`), `ver` (string) | MUST be the first line |
| `verify` | `nonce` (32 lowercase hex), `user` (`^[a-z_][a-z0-9_-]{0,31}$`), `lane` (`lock`), `service`, `tty`, `rhost`, `ruser` (optional), `budget_ms` (int; the module computes it from its remaining deadline, see §8), `progress` (bool, default false) | one per connection; a second → `error bad_request` + close |
| `cancel` | `nonce` | cancels the `verify` with that nonce on any connection of the same uid; unknown nonce → `ack` (idempotent) |
| `prewarm` | — | loads models; never opens the camera; ≤ 1 per 30 s per uid, else `ack {"deferred":true}` |
| `lock_session` | `state` (`locked`\|`unlocked`) | keeps the daemon resident and exempts this connection from the idle close while `locked`; resets the per-lock convenience budget; lifetime in §3; forgeable by design |
| `status` | `user` (optional for P == U) | see reply |
| `subscribe` | `events` (array ⊆ `["resumed","lid_open","lid_close","availability_changed","lockout_changed","verify_finished","enroll_progress"]`, default all) | events are delivered on this connection until it closes; the reference wrapper sends it immediately after `hello` |
| `ping` | — | → `pong` |
| `enroll` | `user`, `label`, `phases` (array, default 6-phase plan), `target_rows` (int, default 60) | streams `event enroll_progress` (capture phases, then `phase:"verify"`), ends with `enroll_done` or `error` (`enroll_verify_failed` when the internal self-check does not accept; nothing is written then) |
| `enroll_abort` | — | |
| `list_templates` | `user` | → `templates` |
| `delete` | `user`, `template` (uuid \| `"all"`) | → `ack` |
| `attest` | `user`, `source` (`nirlockctl`) | records a root SAE → `ack` |
| `reset_lockout` | `user` | records a root SAE and clears lockout fields → `ack` |
| `set_enabled` | `user`, `template` (uuid), `enabled` (bool) | → `ack` |
| `camera_repin` | `user` | re-records `usb_sysfs`/`bcdDevice` → `ack` |
| `config_reload` | — | → `ack` or `error bad_request` with the parse message |

## 6. Messages — daemon → client

| `t` | Fields |
|---|---|
| `welcome` | `daemon` (version), `proto` (1), `ready` (bool: models loaded), `face` `{available: bool, reason: "ok"\|"stale"\|"locked_out"\|"not_enrolled"\|"template_stale"\|"lid_closed"\|"disabled"\|"account_locked"\|"model_mismatch"\|"camera_missing"\|"no_session"}`, `lockout_until_ms` (int, BOOTTIME ms, 0 if none) |
| `ack` | `of` (the request `t`), optional `deferred` (bool) |
| `progress` | `nonce`, `phase` (`opening`\|`streaming`\|`scoring`), `frames` (int). No gate reasons, no hits, no scores. Only when `progress:true`, ≤ 5/s |
| `result` | `nonce`, `user`, `outcome` (`accept`\|`reject`\|`locked_out`\|`unavailable`\|`cancelled`), `reason`, `ms` (int) |
| `status` | `enrolled`, `templates` (int), `face` (as in `welcome`), `consecutive_failures`, `lockout_until_ms`, `camera` `{present, pinned}`, `models` `{ok}`. For P == 0 additionally: `failures_since_sae`, `last_sae` (`boot`\|`root`\|`audit`), `sae_age_s`, `models.auraface_sha`, `rgb_assist` |
| `templates` | `user`, `templates` (array of `{uuid, label, rows, created, enabled, stale}`) |
| `event` | `name`, `data` (object), `ts_ms` (BOOTTIME). `verify_finished` data: `{nonce, outcome, reason, retry_after_ms?}` (`retry_after_ms` present when `reason` is `rate_limited`: BOOTTIME-relative wait until the exhausted quota frees; also present for `locked_out` as the soft-lockout remainder, 0 for a hard lockout); `lockout_changed` data: `{until_ms, kind: "soft"\|"hard"\|"none"}` (`until_ms` 0 when cleared); `availability_changed` data: the `face` object; `enroll_progress` data: `{phase, accepted, needed, reason, hint, box_w}` (`phase` is a capture phase name or `"verify"`) |
| `enroll_done` | `template` (uuid), `rows` (int), `per_embedder` (object), `verified` (bool, always `true` when this message is sent; a failed self-check is an `error enroll_verify_failed` instead), `verify_ms` (int) |
| `pong` | — |
| `error` | `code` (`forbidden`\|`bad_request`\|`version`\|`busy`\|`rate_limited`\|`too_many_connections`\|`too_large`\|`enroll_verify_failed`\|`internal`), `msg` |

### 6.1 `result.reason` values

| outcome | reason |
|---|---|
| `accept` | `k2` |
| `reject` | `no_match` (≥ 1 gate-passed frame scored below threshold, regardless of how the request ended), `no_face` (no gate-passed frame within the budget), `rhost` |
| `locked_out` | `soft`, `hard` |
| `unavailable` | `stale`, `not_enrolled`, `template_stale`, `disabled` (all of the user's templates disabled via `set_enabled`/`enabled.json`), `account_locked` (login shell is `nologin`/`false`, DESIGN §6.3), `lid_closed`, `camera_busy`, `camera_missing`, `camera_mismatch`, `camera_format`, `camera_ambiguous`, `metadata`, `models_unavailable`, `rate_limited`, `budget`, `busy`, `no_session`, `suspending`, `internal` |
| `cancelled` | `eof`, `cancel` — only when zero frames were scored; otherwise the outcome is `reject no_match`/`no_face` |

### 6.2 Fixed format of `result` for `pam` clients

The daemon MUST emit the `result` line for a `pam` connection with a hand-written template, byte-exact field order and no extra whitespace:

```
{"v":1,"t":"result","nonce":"<32 hex>","user":"<user>","outcome":"<outcome>","reason":"<reason>","ms":<int>}\n
```

The C module matches the prefix `{"v":1,"t":"result","nonce":"` literally, reads exactly 32 hex characters, then `","user":"`, the user bytes, `","outcome":"`, and the outcome token up to the next `"`. Both sides are unit-tested against the same golden lines and the scanner is fuzzed. Any other line shape from the daemon on a `pam` connection is treated by the module as a protocol error (`PAM_SERVICE_ERR`).

## 7. `verify` lifecycle

```
client                     daemon
  hello ---------------->
  <---------------- welcome                 (immediately; ready may be false)
  verify --------------->                   policy checks ≤ 5 ms
  <---------------- result (on policy refusal: camera never opened)
                         | RGB thread start → open video → open meta → S_FMT video → S_FMT meta
                         | → STREAMON meta → STREAMON video      (exact order: DESIGN §2.3 point 4)
  <---------------- progress ...            (only if progress:true)
                         | per lit frame: label → gate → score → window → charge-first
  <---------------- result                  (≤ budget_ms + 200 ms after verify, always)
  close  <---------------                   (pam connections)
```

- Exactly one `result` per `verify`, even on cancel, lid close, suspend or internal error.
- Everything the daemon does before the first frame (waiting up to 2 000 ms for the pinned device to reappear after resume, 3 × 150 ms `EBUSY` retries, RGB start) is **inside** `budget_ms`; the `result` deadline never extends for it.
- Cancel by EOF: the daemon polls the connection inside the streaming loop and MUST stop the camera within one frame interval (< 70 ms) and terminate the running inference (`RunOptions::terminate`).
- Charge-first: the daemon MUST persist the recognition-failure counters before writing a `result` that is not `accept` whenever ≥ 1 frame was scored below threshold.
- `nonce` is single-use per boot; a reused nonce → `error bad_request`.

## 8. Timeouts (normative)

| Who | What | Value |
|---|---|---|
| daemon | `hello` after connect | 2 000 ms |
| daemon | complete line after first byte | 1 000 ms |
| daemon | `result` after `verify` | ≤ `budget_ms` + 200 ms (post-resume wait and `EBUSY` retries included) |
| daemon | idle connection without request/subscription/locked lock session | 30 s |
| daemon | lock session after its connection closed without `unlocked` | 300 s |
| module | connect | 500 ms |
| module | `welcome` after `hello` | 2 500 ms |
| module | `budget_ms` sent in `verify` | `deadline − now − 300 ms`, clamped to [1 000, 4 000]; if less than 1 300 ms remain after `welcome` the module returns `PAM_AUTHINFO_UNAVAIL` without sending `verify` |
| module | total | `timeout=` option (default 7 000 ms), absolute from entry into `pam_sm_authenticate`. Worst case with the default: 500 + 2 500 + (3 700 + 200) = 6 900 ms < 7 000, so the module never gives up while a `result` is still owed |
| lock client | reconnect backoff | 5 s while locked |

## 9. Versioning

`proto` is an integer. The daemon serves version 1; a `hello` on a connection whose lines carry another `v` gets `error version` and is closed; the module treats that as `PAM_SERVICE_ERR`. Additive fields never bump `v`; a change in the meaning of `result`, in the authority matrix or in framing bumps `v` and the module's compiled constant together (same package). `welcome.daemon` and `hello.ver` mismatches are logged at `warning` and surfaced by `nirlockctl doctor`.

## 10. Data hygiene

No message on this socket ever carries pixels, crops, landmarks, boxes, embeddings, cosine scores or score buckets. `progress` carries only `phase` and `frames`. `event enroll_progress` carries gate reasons and position hints (quality feedback, not match scores). Scores exist only in `/var/lib/nirlock/audit.jsonl` (0600).
