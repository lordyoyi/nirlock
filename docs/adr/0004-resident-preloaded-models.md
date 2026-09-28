# ADR-0004 — Socket-activated daemon, resident while locked, models preloaded

Status: accepted.

## Context
Measured: AuraFace load 570 ms (OpenCV; 0.6–0.9 s under ORT in earlier research) is not hideable behind the 253 ms camera start; warm K=2 634 ms vs cold 985 ms. A permanently resident daemon costs ~500 MB RSS all day; a pure exit-on-idle daemon pays the cold cost on the most common return path (idle lock at 300 s, return minutes later).

## Decision
`nirlockd.socket` + `Type=notify` service. The lock plugin sends `hello`, `subscribe`, `lock_session locked` and `prewarm` at lock time; the daemon loads models then and stays resident while a lock session is registered and for 300 s after the session ends, then exits (`ExitType=main`). A lock session ends with `lock_session unlocked` or 300 s after its connection closes; a `lock` connection with a `locked` session is exempt from the 30 s idle close, and events (`resumed`, `lid_open`, `availability_changed`, `lockout_changed`, `verify_finished`) reach only subscribed connections (PROTOCOL §3). Revised 2026-09-23: the first sketch never subscribed, so the connection would have been closed after 30 s idle and residency plus resume/lid triggers would have been lost on any lock longer than that. A `verify` arriving before models are ready opens the camera **concurrently** and drops frames until ready (fuprobe `--cold-models` behaviour). SHA-256 of the models is verified once per process lifetime, before session creation. `prewarm` never opens the camera (LED behaviour unmeasured; on kernel 7.2.5 an open without STREAMON does not keep the USB device awake).

## Alternatives
Always resident (memory); exit-on-idle without prewarm (985 ms); model TTL of 600 s from last use (unloads exactly before the user returns — rejected by review); camera pre-open on resume (v1.1 experiment once LED behaviour is known).

## Consequences
`lock_session` is forgeable by uid 1000, which only affects residency (RAM), never policy. A crashed wrapper makes the next unlock cold (985 ms), not broken.
