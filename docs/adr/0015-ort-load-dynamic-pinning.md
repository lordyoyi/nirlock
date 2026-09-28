# ADR-0015 — ONNX Runtime via `ort` load-dynamic against Arch `onnxruntime-cpu`, pinned

Status: accepted; measured in M0.

## Context
`ort` 2.0.0-rc.13 (2026-07-28, MSRV 1.88) exposes `api-17`…`api-28` with `api-27` default and enables `download-binaries`/`tls-native`/`copy-dylibs` by default; `load-dynamic` refuses an ORT older than the crate's API level and warns on a newer one. Arch ships `onnxruntime-cpu` 1.29.0-3 with a stable `libonnxruntime.so.1` soname and rebuilds it on protobuf/abseil bumps. ort's main branch already defaults to `api-30` (ORT 1.30, not yet in Arch). OpenCV in the daemon would drag a 50-library closure; `tract` is unbenchmarked on ResNet-100.

## Decision
`ort = { version = "=2.0.0-rc.13", default-features = false, features = ["std", "load-dynamic", "api-27"] }`, `Cargo.lock` committed, `--locked` builds. `ort::init_from("/usr/lib/libonnxruntime.so")` (also `ORT_DYLIB_PATH` in the unit); a failed load → state `Cold`, `unavailable models_unavailable`, retried on the next `prewarm`. `depends=('onnxruntime-cpu>=1.28')`; rule in `docs/COMPAT.md`: the crate's `api-XX` must be ≤ the Arch ORT minor. One global thread pool (intra 4, no spinning), YuNet with intra 1, `RunOptions::terminate` on cancel. `doctor` prints `OrtGetApiBase()->GetVersionString()`.

## Alternatives
Bundled pyke binaries (supply chain, no network in `makepkg`); `opencv` crate (closure); `tract` (plan C if ORT packaging fails; expected 2–4× slower, unmeasured); int8 export (M2 experiment, needs threshold re-validation).

## Consequences
All latency figures in DESIGN.md §1.5 were OpenCV numbers until M0 measured ORT on this machine (`docs/BENCH.md`); the M2/M3 gates are expressed against measured references, not against the OpenCV numbers. M0 found that ORT buys no per-frame speed over OpenCV 5 `cv::dnn` for AuraFace on this CPU (equal within run-to-run noise at 1, 2 and 8 threads, 0–15 ms slower at 4) and is ~2.5× slower for SFace; the rationale for ORT stays the dependency closure and packaging, not performance.

Loader caveat found in M0: in `ort` 2.0.0-rc.13 a failed `init_from` leaves the crate's private `OnceLock` marked complete but uninitialised (`Once::call_once_force` completes even when the closure returned `Err`), so a second `init_from` reports success without loading anything and the next API call is undefined behaviour. `nirlock-vision::runtime` therefore probes the library itself (`dlopen`, `OrtGetApiBase`, version) before handing the path to `ort`: a probe failure is the retryable `Cold` state of DESIGN §2.6; an `ort`-level failure after a good probe is sticky (`Poisoned`) and the daemon must restart to retry. Every `ort` entry point in the crate runs the probe/init first, so `ort`'s own fallback (panic + bare-soname `dlopen` through the loader search path) is never reached.
