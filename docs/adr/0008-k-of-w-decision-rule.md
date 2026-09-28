# ADR-0008 — Decision rule: K=2 of the last W=4 processed lit frames, max cosine

Status: accepted.

## Context
fuprobe `latency` logs K=1/2/3. Per-attempt score inflation on DROZY video clips: best-of-attempt +0.038 over the mean frame; requiring 2-of-4 gives back only 0.011–0.013, so K=2 costs +133 ms and buys little FAR, but K=1 is a single-frame decision at the moment the AE may still be ramping. K=3 adds another 133 ms by construction. Consecutive lit frames are near-duplicates, so W larger than 4 adds no independence.

## Decision
Score of a lit frame = max cosine over all rows of the user's enabled templates. Hit if ≥ threshold. Accept when ≥ 2 hits among the last 4 *processed* lit frames (a gate failure is a miss; frames dropped by the depth-1 latest-wins queue create no entry). A hit expires after 800 ms so a window cannot stretch across slow retries. The decision index must equal fuprobe's on replayed sessions (`enroll-b` 391 ms, `enroll-a` 1 323 ms).

## Alternatives
K=1 (faster, weaker); K=3 (slower); mean-of-window scores (hides a single strong impostor frame less than max but changes the validated statistics); template averaging (rejected: the live rule and E5 both use max over rows).

## Consequences
Row count inflates impostor max (54→123 rows: 0.392→0.400), hence the caps of 150 rows per template and 400 per user. Every scored miss is charged to the failure counters (ADR-0009).
