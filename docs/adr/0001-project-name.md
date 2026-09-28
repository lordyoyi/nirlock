# ADR-0001 — Project name: nirlock

Status: accepted, 2026-09-23.

## Context
Three proposals converged on `nirlock` independently. A publishable project needs a name that is free on crates.io, AUR, GitHub and PyPI and that derives cleanly into daemon/module/CLI/plugin names.

## Decision
`nirlock`: crates.io 404, AUR 0 results, GitHub 0 repositories named `nirlock`, PyPI 404 (re-checked 2026-09-23). Derived: `nirlockd`, `pam_nirlock.so`, `nirlockctl`, `nirlock.lock` (Omarchy plugin id), `/run/nirlock/sock`, `/var/lib/nirlock`, `/etc/nirlock`, packages `nirlock` and `nirlock-models`.

## Alternatives
`strobeface` (free, but jokey for an auth component), `zenface` (ties to one laptop; minor GitHub collisions), `nirgate`/`facelane` (free, less self-explanatory), `facegate` (taken on AUR), `mirada`/`semblance` (crates reserved in 2026), `omarchy-face` (existing project).

## Consequences
No `/usr/bin/omarchy-*` files ship from this package (all belong to the `omarchy` package); setup/remove are `nirlock-setup`/`nirlock-remove`.
