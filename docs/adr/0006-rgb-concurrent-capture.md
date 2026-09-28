# ADR-0006 — RGB stream started concurrently as an exposure lever

Status: accepted; default `rgb_assist = "always"`, revisited in M2.

## Context
In a near-dark room the firmware AE targets the whole-frame mean and burns the emitter-lit face (11–22 % saturated) for seconds. With the RGB node streaming 1280x720 MJPG concurrently the firmware exposes IR lower: face mean 155→112, saturation 0.11→0.001, 8/8 accepts, K=2 596 ms vs 634 ms (n=8). RGB+IR simultaneous streaming drops zero frames. Windows Hello also requires an RGB face since 2025 (future cross-spectral check).

## Decision
On every `verify` the daemon starts the RGB thread **first** (as fuprobe's `cmd_latency` did: `rgb.start()` before `cap.open()`), DQBUF/QBUF without decoding, then opens IR meta and video. RGB `EBUSY` (a browser holds `/dev/video0`) → continue IR-only, report `rgb_assist=denied` in the result and journal. Knob `capture.rgb_assist = always | auto | never` (`auto` = ALS < 20 lux). RGB frames never participate in the decision in v1.

## Alternatives
IR-only (measured timeouts in the dark); RGB only when dark (cost in lit rooms unmeasured — the M2 A/B decides between `always` and `auto`); mandatory RGB face (removes dark-room unlock; deferred to the anti-spoof work).

## Consequences
The measured configuration is reproduced exactly (start order matters because the AE regime is bistable and unexplained). Extra USB bandwidth and a second open per unlock.

**Correction (M1, refined in M2 with n = 20 per arm, 2026-09-27):** "zero
latency cost" was measured warm in Phase 0. Cold — the path every unlock
takes, since the camera autosuspends 2.6 s after close — the RGB lever is
free in the median and costs a fixed ~198 ms in a fifth of the attempts:

| Arm (20 cold starts each, 7 s apart) | Median | p90 | Max | Slow runs |
|---|---|---|---|---|
| IR only | 253 ms | 255 | 255 | 0 of 20 |
| IR + RGB assist | 254 ms | 451 | 452 | **4 of 20** (448–452 ms) |

The distribution is bimodal: every run is either ~253 ms or ~451 ms, never in
between, and 451 − 253 = 198 ms is exactly three 66.7 ms frame slots.
`first_sequence` is 1 in both cases, so no frames are dropped — the IR stream
simply starts three slots late when the RGB interface is already streaming,
which points at USB bandwidth negotiation or the uvcvideo start path rather
than at anything we do. Worst case moves a K=2 decision from ~600 ms to
~800 ms.

**Consequence for the policy, not for the mechanism:** `rgb_assist = always`
buys a better IR exposure in the dark at the price of a 20 % chance of a
200 ms slower unlock in every light. Two things now argue for `auto` (engage
only when the ALS reads dark):

1. The benefit is only there in the dark. In a lit room the AE converges on
   its own.
2. ADR-0016 relaxed the saturation gate to 15 % on measured evidence, which
   covers much of the same dark-room failure on its own (E4's clipped faces
   read 11–22 %). The two mitigations overlap, so the lever's remaining value
   in v1 is smaller than when this ADR was written.

M3 decides the default with the request loop in hand; the cross-spectral
RGB+IR agreement check that would make the lever mandatory is a v2 concern
(DESIGN §12 risk 1), not a v1 one.