# ADR-0002 — Rust daemon, C PAM module, QML plugin

Status: accepted.

## Context
Three PAM hosts on this machine force a daemon + thin client shape: Quickshell's fork-without-exec child (uid 1000), setuid sudo, and the polkit-agent-helper sandbox (no devices, AF_UNIX only). The daemon owns the camera, models (260 MB AuraFace) and templates; the PAM module runs inside foreign, fragile processes; the lock UI is Quickshell QML.

## Decision
- `nirlockd` in Rust (stable via rustup/mise; no cmake/meson): V4L2/UVCM layer, YuNet/AuraFace via `ort` load-dynamic, policy, IPC. Ported from the phase0 C++ reference (`v4l2cap.*`, `pipeline.*`, `main.cpp latency`) with replay parity tests.
- `pam_nirlock.so` in C11, ~300 lines, libc + libpam only, no threads/NSS/dlopen/signal handlers/malloc/conversation. Safe inside a forked child of a multithreaded Qt process and, later, sudo/polkit.
- `nirlock.lock` in QML: a wrapper that Loader-loads the stock Omarchy `Service.qml` and adds one `PamContext` lane.
- `nirlockctl` in Rust sharing the `nirlock-wire` crate.

## Alternatives
Howdy-style spawn-per-auth (AuraFace load cannot hide behind the 253 ms camera start; PAM hosts are unprivileged/sandboxed); C++ daemon with OpenCV (52-library closure in an auth daemon, weekly rebuild hazard); Python anywhere on the privileged path (rejected by DECISIONS.md).

## Consequences
One extra toolchain (rustup). Parity with fuprobe is a test obligation, not an assumption. No OpenCV in the daemon; alignment and YuNet post-processing are reimplemented.
