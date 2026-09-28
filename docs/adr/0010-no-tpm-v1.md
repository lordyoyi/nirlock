# ADR-0010 — No TPM sealing, no template encryption beyond LUKS in v1

Status: accepted.

## Context
Secure Boot is disabled on this machine and the bootloader reports no active PCR banks; sealing to PCR 7 would be theatre. LUKS already covers offline theft. A TPM unseal on every socket-activated start would add unmeasured cold-start latency. Templates are embeddings, not images.

## Decision
Templates live in `/var/lib/nirlock` (0700, user `nirlock`, files 0600), `mlock`ed and zeroized in memory, never logged. No TPM, no `systemd-creds`, no per-template encryption in v1. Deletion overwrites with zeros + fsync + unlink (best effort on btrfs, documented).

## Alternatives
`systemd-creds` host+tpm2 (meaningless without measured boot); user-password-derived key (needs the password at unlock time, which the daemon never sees).

## Consequences
An offline attacker who defeats LUKS obtains embeddings. Revisit only if the user enables Secure Boot/UKI later.
