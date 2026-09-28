# ADR-0003 — IPC: AF_UNIX stream socket, NDJSON, SO_PEERCRED

Status: accepted. Normative text: `design/PROTOCOL.md`.

## Context
`Quickshell.Io.Socket` is a `QLocalSocket` (stream) with `write(QString)` and `SplitParser`; SEQPACKET is unreachable from QML, and QML cannot compute a UTF-8 byte length. A C module in a PAM child needs a bounded, allocation-free parser. D-Bus would add a bus library to the module and is not needed for the lock lane.

## Decision
`/run/nirlock/sock`, `SOCK_STREAM`, mode 0666 created by the socket unit. Authorization solely by `SO_PEERCRED` plus the `client` role declared in `hello` (pam/lock/ctl); uid N may only verify user N; admin verbs need uid 0 **and** `client=ctl` on a connection that never sent `verify`. Framing: one JSON object per `\n`-terminated line, ≤ 8 KiB inbound. The PAM `result` line is emitted with a fixed byte-exact template so the C scanner matches a literal prefix. Cancel = close the socket; explicit `cancel` requires the nonce.

## Alternatives
Length-prefixed frames (unreliable from QML); hybrid `<len> <json>\n` (footgun for non-ASCII); SEQPACKET (unreachable from QML); system D-Bus (a possible second front-end later; not v1).

## Consequences
Authority does not collapse when a PAM host runs as euid 0 (sudo/polkit in v2): a `pam` client never gets admin verbs. Line limit gives the same DoS bound a length prefix would.
