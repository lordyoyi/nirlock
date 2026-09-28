# ADR-0012 — PAM lane skeleton and a package-owned PAM confdir (`/usr/lib/nirlock/pam.d`)

Status: accepted; **rewritten 2026-09-23** (the first version installed the lane in the libpam vendor directory `/usr/lib/pam.d`, which the lock screen never reads — see Context). Skeleton verified on pam 1.7.2 (`research/prototypes/pamtest`).

## Context
Verified locally: a module returning `PAM_IGNORE` followed by `auth optional pam_permit.so` (present in this machine's `system-auth`) authenticates anyone; `sufficient` + `pam_deny` flattens `MAXTRIES`/`AUTHINFO_UNAVAIL` to `AUTH_ERR`; a missing module or service file denies. `omarchy-apply-lock` rewrites only `-password`/`-fingerprint`. Upstream PRs use the name `omarchy-lock-face` in `/etc/pam.d`.

**The host that matters uses `pam_start_confdir`, not `pam_start`.** Quickshell 0.3.1's `PamContext` always calls `pam_start_confdir(config, user, conv, configDirectory)` with `configDirectory` defaulting to `/etc/pam.d` (`/usr/bin/quickshell` imports `pam_start_confdir` only and carries the literal `/etc/pam.d`; the qmltypes expose `configDirectory`). In libpam's confdir path the lookup is **only** `<confdir>/<service>` then `<confdir>/other` — there is no fallback to the vendor directory `/usr/lib/pam.d`. Verified on this machine with an `fopen`-logging `LD_PRELOAD` shim: `pam_start_confdir("polkit-1", …, "/etc/pam.d")` opens `/etc/pam.d/polkit-1` (ENOENT) then `/etc/pam.d/other`, whereas `pam_start("polkit-1")` opens `/usr/lib/pam.d/polkit-1`. The previous version of this ADR ("libpam here searches `/etc/pam.d/%s` then `/usr/lib/pam.d/%s`") was true for `pam_start()` and irrelevant for the lock screen: every face burst would have fallen to `other` → deny while the wrapper and `doctor` reported the lane as armed.

## Decision
The package installs its **own PAM configuration directory** `/usr/lib/nirlock/pam.d/` (root 0755, files 0644, `pacman -Qkk`-verifiable) and every nirlock PAM host selects it explicitly:

- The lock wrapper's `PamContext` sets `config: "nirlock-lock"` and `configDirectory: "/usr/lib/nirlock/pam.d"`.
- `nirlockctl` (root) calls `pam_start_confdir("nirlock-admin", user, conv, "/usr/lib/nirlock/pam.d")` for the attestation check (ADR-0013), so a foreign `/etc/pam.d/nirlock-admin` can never take precedence either (it would with `pam_start()`).

Files (verbatim in DESIGN.md §4.4):

`nirlock-lock`
```
#%PAM-1.0
auth     [success=done maxtries=die default=ignore]  pam_nirlock.so lane=lock timeout=7000
auth     required                                    pam_deny.so
```
`nirlock-admin`
```
#%PAM-1.0
auth     [success=done default=die]                  pam_unix.so
auth     required                                    pam_deny.so
```
`other`: all four types `required pam_deny.so`.

Rules: **no `include` lines** (includes resolve against the confdir; the old `account include system-local-login` was inert under Quickshell — no `pam_acct_mgmt` — and would now point at a missing file); no `system-auth` (fail-open via `optional pam_permit`); no `pam_faillock` in `nirlock-admin` (root attestation attempts must not lock the user's password); no `nullok`. The service is named `nirlock-lock`, not `omarchy-lock-face`, so nobody looks for it in `/etc/pam.d`. The module never returns `PAM_IGNORE` (`#pragma GCC poison`), reads the user with `pam_get_item(PAM_USER)` (never `pam_get_user`, which opens the conversation), and maps `locked_out` → `PAM_MAXTRIES`, `unavailable` (incl. `stale`) → `PAM_AUTHINFO_UNAVAIL`, `reject` → `PAM_AUTH_ERR`. The wrapper watches exactly `/usr/lib/nirlock/pam.d/nirlock-lock` and arms only when its normalized text matches; `nirlockctl doctor` checks that the installed plugin's `PamContext` carries that `configDirectory` (its absence is the error, not its presence). The pamtest suite gains a confdir canary: a service present only in a vendor-style directory falls to `other` under `pam_start_confdir("/etc/pam.d")`.

## Alternatives
`/usr/lib/pam.d/omarchy-lock-face` (never read by Quickshell — the bug this rewrite fixes); `/etc/pam.d/omarchy-lock-face` with `backup=` and content-based conflict handling against upstream #8336 (three sources of truth, weekly-rewrite risk, name collision with Howdy's setup); `auth required pam_nirlock.so` alone (a later include would fail open); `sufficient` (loses MAXTRIES); `stale` as `MAXTRIES` (plugin enters an undefined "until" state); `nirlockctl` using `pam_start()` (works for root, but `/etc/pam.d/nirlock-admin` could override the vendor file).

## Consequences
The lane the lock screen reads is byte-for-byte the packaged one; nothing in `/etc/pam.d` can override or disable it, and `omarchy-apply-lock`'s rewrites are irrelevant to it. A future Quickshell that dropped `configDirectory` or switched to `pam_start()` would make the face lane fall to `other` → deny (fail-closed, no face) and `doctor` would say so. The `account` phase is absent by design (T21 unchanged). `PamResult.MaxTries` reaching the wrapper is a measurement (E8b), not a dependency: lockout UX is driven by daemon events. Fail-open canaries stay in CI as negative tests.
