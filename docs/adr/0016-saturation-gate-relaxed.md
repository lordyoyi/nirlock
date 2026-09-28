# ADR-0016 — Saturation gate relaxed from 5 % to 15 %

Status: accepted, measured 2026-09-27 (M2). Supersedes the `max_sat_in_box`
value of the Phase-0 gate, not the rest of it.

## Context

The firmware's auto-exposure targets the whole-frame mean. In a dark room the
background drags the exposure up until the emitter-lit face clips, and it
*stays* clipped: E4 on 2026-09-22 lost 4 of 40 unlock attempts to a 5 s
timeout with the face reading 11–22 % saturated pixels inside its box for the
whole attempt, while the frames themselves were otherwise perfect. The gate
(`max_sat_in_box = 0.05`) was rejecting every one of them, correctly by its
own rule, but the question was never measured: **does a clipped face still
recognise, and does clipping help an impostor?**

## Decision

`GateConfig::max_sat_in_box` becomes **0.15** (was 0.05). Nothing else in the
gate changes; the impostor thresholds of ADR-0007 are unaffected (see below).

## Evidence

*Impostor side* — the 26 327 impostor frames of E5 (`e5_far_rodrigo_a.csv.impostor_frames.csv`),
grouped by the reason the gate gave:

| Population | n | Max SFace | Max AuraFace |
|---|---|---|---|
| Accepted by the 5 % gate | 19 748 | 0.4716 | 0.3921 |
| **Rejected as saturated** | 63 | **0.3630** | **0.2281** |
| Combined ceiling | | 0.4716 (Δ +0.0000) | 0.3921 (Δ +0.0000) |

Admitting the saturated impostor frames does not raise the ceiling by a single
digit: the highest-scoring impostor was already getting through. So the
provisional thresholds (AuraFace 0.45, SFace 0.55) keep exactly the margin
they had.

*Genuine side* — two dark-room sessions replayed against the `rodrigo`
template with `nirlock-replay`, looking at the frames the 5 % gate rejected:

| Saturation in box | AuraFace | SFace |
|---|---|---|
| 0.066 – 0.176 | 0.64 – 0.71 | 0.76 – 0.82 |
| 0.299 | 0.74 | 0.78 |
| 0.461 – 0.497 | 0.53 – 0.55 | 0.64 – 0.70 |

Every one of them clears the 0.45 threshold. Up to ~0.20 the worst case is
0.64, a margin of 0.19; past 0.30 the margin thins towards 0.08, which is why
the new limit is 0.15 and not "no limit".

## Consequences

- The dark-room attempts that timed out in E4 would have decided on their
  first or second lit frame instead. This is the only measured fix for the
  saturating AE regime that costs nothing: ADR-0005 rules out control writes
  and E6 showed neither ROI auto-exposure nor mode D1 helps.
- Slightly more embeddings per unlock in bright-background scenes (frames that
  used to be discarded before the embedder now reach it), which shortens the
  decision rather than lengthening it.
- The evidence is 10 genuine saturated frames from one night and 63 impostor
  frames from datasets that are not this camera. M8 re-checks it with the
  multi-day sessions; if a genuine saturated frame is ever seen below
  threshold, the limit comes back down and the ADR is superseded.
- `PHASE0-RESULTS.md` numbers were taken at 0.05. Any replay that must
  reproduce them exactly must set `max_sat_in_box = 0.05` explicitly; the
  parity run of M2 predates this change and used the old value.
