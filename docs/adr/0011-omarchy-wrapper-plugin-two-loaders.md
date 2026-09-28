# ADR-0011 — Omarchy integration: clonedFrom wrapper with two Loaders

Status: accepted.

## Context
Everything under `/usr/share/omarchy` is package-owned and updated weekly. The supported override is a user plugin with `"omarchy": {"clonedFrom": "omarchy.lock"}` (inherits the `authentication` capability, disables the stock plugin). A clone that fails to instantiate leaves the machine with **no lock service**: `omarchy-system-lock` discards the failure and `omarchy-sleep-lock` suspends unsecured after 12 s. Upstream face PRs converge on a third `PamContext` on `omarchy-lock-face`; the live one (#8336) is Howdy-based and arms only with Howdy models present.

## Decision
`nirlock.lock` ships a minimal `Service.qml` (imports `QtQuick`, `Quickshell`, `Quickshell.Io` only) with two `Loader`s: one for the stock `Service.qml` (URL resolved from `OMARCHY_PATH` at declaration time, `Binding`s forward `shell`/`omarchyPath`), one for `FaceLane.qml` (active only when the stock is `Ready`). A stock load failure triggers self-disable (`omarchy-shell shell setPluginEnabled nirlock.lock false` → `restoreCloneSource`). The lane arms only after `secure=true` + 3 s, only if the effective PAM lane text matches the shipped one, only if the daemon reports availability, and yields when `stock.faceConfigured === true`. Post-update hook: static check now + `systemd-run --user --on-active=60s nirlockctl doctor --repair-plugin` (hooks run before the shell restart); post-boot hook polls up to 30 s. Repairs never call `omarchy-refresh-shell`, never run while locked, and edit the three `shell.json` keys atomically. Removal disables **before** deleting the directory.

## Alternatives
Cloning `Service.qml` (a fork; loses upstream fixes weekly); patching `/usr/share/omarchy` (overwritten); a separate lock client (two ext-session-lock clients); a PanelWindow overlay for feedback (invisible above ext-session-lock without `above_lock`; potential compositor weakness).

## Consequences
The password and fingerprint paths are byte-identical to upstream. Feedback is limited to the stock `failureMessage` (short strings, error styling). The stock runs under the third-party shell facade; `doctor` flags new `shell.` usages. `keepLoaded` means plugin updates need a shell restart.
