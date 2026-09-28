# ADR-0007 — AuraFace glintr100 as base embedder; provisional thresholds 0.45 / 0.55

Status: accepted, thresholds provisional until M8.

## Context
E5 (proxy impostors: LFW visible, DROZY/MR-NIRP NIR): AuraFace impostor scores barely shift from visible to NIR (max 0.400 LFW with 123 rows, 0.355 NIR) against a genuine minimum of 0.708; SFace's NIR impostors rise +0.12 (its LFW-derived thresholds admit 2–6 of 14 NIR identities). AuraFace costs ~150 ms/frame on battery (OpenCV), above the 133 ms lit cadence; SFace ~16 ms. Both are Apache-2.0; no InsightFace weights anywhere (non-commercial).

## Decision
AuraFace `auraface_glintr100.onnx` (512-d, RGB, (x−127.5)/127.5) is the only gating embedder. Threshold **0.45** (above every impostor seen, near the GPD 1e-5 tail plus per-attempt inflation 0.46–0.49, 0.26 below the genuine minimum). SFace runs in shadow mode only (threshold 0.55 if ever gated), scored after the decision, logged only to the 0600 audit file. **SFace input contract** (added 2026-09-23; the E5 shadow numbers depend on it): the same aligned 112×112 crop, channels swapped to RGB, **raw 0..255 float32, no mean subtraction, no scaling**, NCHW, output L2-normalised by us — exactly what OpenCV's `FaceRecognizerSF::feature` does (`blobFromImage(aligned, 1, Size(112,112), Scalar(0,0,0), swapRB=true, crop=false)`, verified in `modules/objdetect/src/face_recognize.cpp`). A port that reused AuraFace's (x−127.5)/127.5 for SFace would silently change the shadow scores; the replay suite compares the Rust SFace score with `fuprobe score` on identical crops (cos ≥ 0.99). Enrollment uses the phase-0 gate (0.75 / 0.05) by default: the templates that justify 0.45 were built with it; the stricter 0.80 / 0.03 gate is an off-by-default option pending the replay check in DESIGN §10. Templates pin model SHA-256s; a model change requires re-enrollment, never conversion.

## Alternatives
SFace as base (fast, unsafe in NIR); AND-fusion (kept as a config option, off); int8 AuraFace (M2 experiment; changes scores, needs E5 re-validation before adoption); InsightFace IR50-class models (licence).

## Consequences
FAR ≤ 1e-4 is not demonstrable with the available material → Class-2 convenience factor (ADR-0009). Latency floor is camera + 2× embedding; ORT numbers must be measured in M0.
