# ADR-0009 — Class-2 policy numbers and charge-first attempt accounting

Status: accepted; numbers provisional, see DESIGN.md §6 and questions 3/7.

## Context
Face is a convenience factor: printed-photo attack untested, FAR undemonstrated. The only defence against a print is the attempt budget, so the accounting must not be evadable by a purely physical attacker (lid cycling, tilting the artefact, cutting bursts short) — all three proposals had this hole (blocker in two reviews). No trustworthy "password succeeded at the lock screen" signal exists (uid-1000 PAM host, rewritten lane).

## Decision
- **Recognition failure** = a request with ≥ 1 gate-passed frame scored below threshold and no accept, whatever ended it. Charged at the first such frame, persisted with fsync **before** the `result`; refunded on accept. `no_face`/`unavailable`/`cancelled` with zero scored frames never count.
- Soft lockout after 5 consecutive failures: 60 s × 2^(n−1), cap 900 s; expiry does **not** reset the counter. Hard lockout after 15 failures since the last strong-auth event (SAE), until an SAE.
- **A face `accept` resets all three failure counters** (`consecutive_failures`, `failures_since_sae`, `soft_lockouts_since_sae`) but is never an SAE (it does not touch freshness). Revised 2026-09-23: the first version reset only `consecutive_failures`, so an honest user scoring under 0.45 now and then (glasses, daylight) could accumulate 15 `no_match` across days and hit a hard lockout that, without the audit oracle, only a reboot or `sudo nirlockctl reset-lockout` clears. The security argument for keeping the counter was weak: an attacker who can produce an `accept` has already unlocked the session; the bound "at most 15 scored-miss bursts between SAEs" becomes "at most 15 between SAEs or genuine accepts", which is the same bound against an attacker who never succeeds. The remaining UX consequence (15 consecutive misses without any accept in between → hard lockout, cleared only by reboot/root without the oracle) is stated in DESIGN §6.3, §12.14 and in the setup text.
- Strong-auth events: boot (only if `boot_is_strong_auth`, set by setup after `cryptsetup luksDump` shows a passphrase-only LUKS), root attestation through `nirlockctl` with its own PAM check (`nirlock-admin`, ignoring sudo's cache), and optionally the kernel audit record of `unix_chkpwd` success (`_TRANSPORT=audit`, current `_BOOT_ID`, monotonic timestamp after the last failure and after a watermark set to "now" whenever the cursor is unknown). A face accept is never an SAE.
- Password freshness 24 h (72 h written by setup if the oracle is declined; never > 7 days).
- Quotas (not failures): 1 verify / 1.5 s, ≤ 30 / 10 min, ≤ 8 per lock, ≤ 36 s camera / 5 min (≥ 8 bursts × 4 s so a full lock's budget fits; `rate_limited` carries `retry_after_ms` in `verify_finished`), ≤ 600 scored frames / h, prewarm ≤ 1 / 30 s, one in-flight request, no camera-open state without a request (no `Hold`). Verify requires an active local seat0 session and an open lid. The enrollment self-check is an internal phase of `enroll` and touches no counter or quota.
- Two counter classes: security class (only daemon events, SAEs, boot) and lock-session class (`verifies_this_lock`, resettable by forgeable uid-1000 hints). A daemon restart changes nothing. Corrupt state or unreadable boot_id ⇒ hard-locked until a root SAE.

## Alternatives
Lockout on 3 failures (harsher UX, same order of protection); failure counter decaying to zero hourly (≈100 attempts/day for a paced attacker — rejected); trusting the wrapper's "password ok" (forgeable); scoring-frame-based lockout only (adopted as the hourly cap, not as the primary rule, to keep lockouts predictable for the user).

## Consequences
At most 15 scored-miss bursts between two SAEs or genuine accepts; lid cycling and artefact tilting are charged. Without the audit oracle the user clears a hard lockout by reboot (LUKS) or `sudo nirlockctl reset-lockout` — and it takes 15 misses in a row with no face accept in between to get there. Scores never leave the daemon, so counters cannot be hill-climbed.
