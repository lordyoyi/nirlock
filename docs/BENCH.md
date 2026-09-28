# nirlock — measurements

Every number here is **[M]** measured on the reference machine (ASUS Zenbook
UX3405CA, Intel Core Ultra 9 285H, 16 hardware threads, Omarchy / Linux
7.2.5) unless a row says otherwise. Reproduce with the commands shown.

## M0 — ONNX Runtime on this machine (2026-09-23, **AC power**)

**Power source: AC** (`AC0/online = 1` before, during and after every run;
the machine could not be unplugged in this session). CPU state recorded by
the bench: `scaling_governor=powersave`, `energy_performance_preference=performance`,
`platform_profile=performance`, affinity `0-15`. **The battery row the M0
acceptance asks for is still pending**; Omarchy switches `platform_profile`
on battery, so the AC rows below are an upper bound on speed, not the
battery figure. Re-run the same script on battery and add the row.

### Setup

| item | value |
|---|---|
| runtime | ONNX Runtime **1.29.1**, official CPU tarball `onnxruntime-linux-x64-1.29.1.tgz` (`/usr/lib/libonnxruntime.so` from Arch `onnxruntime-cpu` is not installed here); tarball SHA-256 `a28d7d65acafc06fb0f416cb409998773f5314c7eebf77caf907812831cdfc67`, `lib/libonnxruntime.so` SHA-256 `ff3de2363a0cb79e5ee79ef59594eae04930c6d7a6b6654c399e7977a5404a4b`; `OrtGetApiBase()->GetVersionString()` = `1.29.1`, build info `git-commit-id=d9d3b2fc25` |
| crate | `ort` 2.0.0-rc.13, `load-dynamic`, `api-27` (ORT 1.29 serves API 27); dlopened via `ORT_DYLIB_PATH` |
| sessions | one thread pool per session, sequential executor, graph optimisation `Level3`, memory pattern on, **intra-op spinning off** (ADR-0015); **YuNet intra-op 1** in every row (the daemon configuration), AuraFace/SFace intra-op = the row's thread count |
| build | release (`lto=thin`, `codegen-units=1`, `panic=abort`), `cargo build --release --locked` |
| models | YuNet `face_detection_yunet_2026may.onnx` `ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0`; AuraFace `auraface_glintr100.onnx` `a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60`; SFace `face_recognition_sface_2021dec.onnx` `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79` (same hashes as the lab template's `manifest.json`) |
| input | the 75 lit frames (`meta_lit=true`) of session `enroll-a` (640x360 GREY); YuNet at native resolution (padded to 640x384 as OpenCV does); AuraFace/SFace on the aligned 112x112 crop |
| protocol | sessions loaded **up front in daemon order** (YuNet, AuraFace, SFace; ADR-0004), then per model: one cold first inference (no warm-up), 40 steady iterations in a tight loop; median = mean of the two middle values, p90 = nearest rank (fuprobe's `summarise`); 3 repeats per thread count, **interleaved with the OpenCV harness** (ORT t=1, OpenCV t=1, ORT t=2, …) so both see the same thermal/frequency state |

```
ORT_DYLIB_PATH=target/ort/onnxruntime-linux-x64-1.29.1/lib/libonnxruntime.so \
target/release/nirlock-bench --models models \
  --frames '<lab>/data/sessions/20260919-234104_enroll-a/ir_*.pgm' \
  --template <lab>/data/templates/rodrigo --threads N --iters 40 --out target/bench/final_tN_rR.bench.json
tools/bench-opencv/build/bench-opencv --models models \
  --frames <lab>/data/sessions/20260919-234104_enroll-a --threads N --iters 40
```

### ORT (nirlock-bench), three runs per row, ms

| embedder threads | YuNet (intra 1) load / first / median / p90 (ranges) | AuraFace load / first / median / p90 (r1 / r2 / r3) | SFace load · first · median · p90 (r1 / r2 / r3) |
|---|---|---|---|
| 1 | 26–49 / 4.4 / 3.9–4.3 / 4.1–4.4 | 307 / 353 / 374 · 173 / 187 / 192 · **173.9 / 187.2 / 191.8** · 174.8 / 190.0 / 193.0 | 13–15 · 20.6 / 21.5 / 21.5 · 20.9 / 21.8 / 21.8 · 21.4 / 22.3 / 22.5 |
| 2 | 24–25 / 4.1–4.4 / 4.0–4.1 / 4.1–4.4 | 344 / 331 / 330 · 116 / 129 / 116 · **95.8 / 101.3 / 94.6** · 100.2 / 121.6 / 98.6 | 14–15 · 13.8 / 14.7 / 13.8 · 13.7 / 14.7 / 12.9 · 15.0 / 15.4 / 13.9 |
| 4 | 25–27 / 4.5–4.7 / 4.1–4.5 / 4.2–4.6 | 325 / 343 / 333 · 72.9 / 74.1 / 69.7 · **55.2 / 72.1 / 68.9** · 61.8 / 83.7 / 73.7 | 14–15 · 10.4 / 12.4 / 10.6 · 9.3 / 10.9 / 9.6 · 10.5 / 12.0 / 10.4 |
| 8 | 29–70 / 4.4–4.5 / 4.1–4.5 / 4.3–4.5 | 354 / 343 / 331 · 54.8 / 54.0 / 56.8 · **51.0 / 50.8 / 53.8** · 54.3 / 55.5 / 56.9 | 14–15 · 10.9 / 12.5 / 8.3 · 10.7 / 10.2 / 7.9 · 11.1 / 11.4 / 8.3 |

Reading: AuraFace fp32 at the daemon's 4 intra-op threads costs **55–72 ms
per frame on AC** (median over three runs 69 ms, p90 62–84), the cold first
inference **+1–20 ms** over steady (70–74 ms), load **325–343 ms** (no
warm-up included). 8 threads buys ~15 ms of median but engages the E-cores
and the whole package. YuNet at intra 1 is **3.9–4.5 ms** (first 4.1–4.7),
SFace (shadow, every frame) **9–11 ms** at 4 threads. The run-to-run spread
on this hybrid P/E CPU is real (55 → 72 ms at 4 threads within eight minutes,
AC, same profile); quote ranges, not a single number.

**Paced loop** (`--pace-ms 133`, one inference per lit-frame period, 4
threads): AuraFace 64.7 ms median / 67.7 p90, SFace 10.7, YuNet 4.2 — inside
the tight-loop range, so the tight loop is a fair proxy at the lit cadence.
It is not a proxy for the first frame after seconds of idle (frequency ramp,
migration back to P-cores); that cost belongs to the `prewarm` forward of
DESIGN §2.6 and is measured with the real request loop in M3.

### Memory (ORT, daemon order, all three sessions resident)

| state | VmRSS | VmHWM |
|---|---|---|
| after YuNet load | 48 MiB | — |
| after AuraFace load (Δ +440 MiB) | 488 MiB | — |
| after SFace load (Δ +0–1 MiB) | **488 MiB** | **550 MiB** |
| end of bench (loops + 75 frames × 3 models) | 489–492 MiB | 550 MiB |
| same, `MALLOC_MMAP_THRESHOLD_=131072` | 338 MiB after loads, 393 at end | 526 MiB |

Identical at 1/2/4/8 threads. The earlier "~800 MiB" figure was an artefact
of the old bench's order (YuNet detections before the AuraFace load: the
freed 2.9 MB input blobs raised glibc's dynamic mmap threshold, so the ~260 MB
transient protobuf/initializer buffers of the load were carved from the heap
and never returned). With the daemon order the steady footprint is **~490 MiB
resident, ~550 MiB peak** with fp32 AuraFace; `MemoryHigh=900M` in DESIGN
§2.2 has ~350 MiB of headroom and can come down to ~700M. Pinning the mmap
threshold trims 150 MiB of resident memory at the price of +150 ms of
AuraFace load (page faults on mmapped chunks; 493 vs 333 ms) and is not worth
it while the load is off the critical path (ADR-0004); `malloc_trim(0)` after
the loads is the cheaper option if the resident number matters later.

### OpenCV 5.0.0 `cv::dnn` on the same frames (like-for-like), ms

`tools/bench-opencv` links the lab's `phase0/pipeline.cpp` unchanged
(`cv::FaceDetectorYN`, `cv::dnn` AuraFace, `cv::FaceRecognizerSF`),
`cv::setNumThreads(N)` (one global pool for all three models, unlike ORT's
per-session pools), same frames/crops/loop/statistics, runs interleaved with
the ORT runs above. Two differences that cannot be removed: fuprobe's
factories run one warm-up forward inside load, so OpenCV "load" **includes**
a first inference and "first" is a warm second call; and YuNet shares the
N-thread pool instead of running at 1 thread.

| threads | YuNet load / first / median / p90 (ranges) | AuraFace load / first / median / p90 (r1 / r2 / r3) | SFace load / first / median / p90 (ranges) |
|---|---|---|---|
| 1 | 17–19 / 7.8–8.2 / 7.3–8.0 / 7.4–8.4 | 421 / 450 / 430 · 178 / 185 / 183 · **177.6 / 189.8 / 183.7** · 178.5 / 192.2 / 186.2 | 38–42 / 11.4–12.0 / 11.3–11.9 / 11.7–12.4 |
| 2 | 13–14 / 4.8–5.6 / 4.6–5.3 / 4.7–5.4 | 341 / 376 / 346 · 95.6 / 99.2 / 96.1 · **96.5 / 101.4 / 96.1** · 100.4 / 103.3 / 100.2 | 32–37 / 6.5–7.2 / 6.3–6.6 / 6.4–6.8 |
| 4 | 10–12 / 4.1–5.2 / 3.6–4.3 / 4.1–4.6 | 297 / 311 / 307 · 53.4 / 54.9 / 54.1 · **54.5 / 62.4 / 56.2** · 55.7 / 63.4 / 57.5 | 30–33 / 4.0–4.4 / 3.8–4.1 / 3.8–4.1 |
| 8 | 9–10 / 3.9–7.1 / 3.3–3.8 / 3.4–4.6 | 302 / 289 / 283 · 44.2 / 39.0 / 43.4 · **50.5 / 51.4 / 51.2** · 51.8 / 55.1 / 53.8 | 30–34 / 3.6–3.9 / 3.6–3.8 / 3.8–3.9 |

Memory: VmRSS 480 MiB after the three loads, 481 at the end, VmHWM 611 MiB.

**Comparison.** AuraFace under ORT is **equal to OpenCV within run-to-run
noise** at 1, 2 and 8 threads (174–192 vs 178–190; 95–101 vs 96–101; 51–54
vs 51 ms) and **0–15 ms slower** at 4 threads (55–72 vs 55–62). SFace is
**~2.5× slower under ORT** (9–11 vs 3.8–4.1 ms at 4 threads; 21 vs 11 at 1
thread). YuNet at ORT intra 1 (3.9–4.5 ms) is faster than OpenCV at 1 thread
(7.3–8.0) and equal to OpenCV at 4–8 threads (3.5–4.3). Load: ORT 325–374 ms
without a warm-up vs OpenCV 283–450 ms with one (at 4 threads: ~333 vs ~305,
of which ~55 is the warm-up forward, so pure load is ~330 vs ~250 ms; ORT
pays more at load, the daemon preloads anyway). Peak RSS: ORT 550 MiB vs
OpenCV 611 MiB.

The Phase-0 numbers in `research/PHASE0-RESULTS.md` (AuraFace ~150 ms/frame,
load 570 ms; YuNet ~10 ms; SFace ~16 ms) were measured **on battery**, inside
fuprobe's live camera loop, with `cv::setNumThreads(4)` and the camera
threads competing; they are not comparable with a tight loop on AC. Read
together with the rows above they say two things: (1) the runtime does not
change the per-frame cost of AuraFace on this CPU (OpenCV and ORT agree to
within 15 ms at every thread count on AC); (2) the ~2.5–3× gap between the
Phase-0 150 ms and the 55–72 ms here is the **power state and the loop
shape**, not ORT. The battery figure for §1.5 is therefore expected to land
near the Phase-0 number, and ORT's justification (ADR-0015) is the dependency
closure and packaging, not speed. `nirlock-bench` and `bench-opencv` record
the power source, governor, EPP and platform profile so the battery rows,
when taken, are interpretable.

### Sandbox (W^X)

The same bench (4 threads, 30 iterations) under a transient user unit with
`MemoryDenyWriteExecute=yes`, `ProtectSystem=strict`, `ProtectHome=read-only`,
`PrivateDevices=yes`, `NoNewPrivileges=yes`, `RestrictAddressFamilies=AF_UNIX`,
`SystemCallArchitectures=native`: **exit 0**, ORT loads and infers
(no JIT / RWX mapping needed), AuraFace 60.0 ms median / 70.3 p90, first
72.0, load 324 ms; YuNet 4.1 ms; SFace 9.4 ms; VmRSS 488 MiB, VmHWM 550 MiB;
parity output byte-identical to the unsandboxed run.

```
systemd-run --user --wait --pipe --collect -p MemoryDenyWriteExecute=yes -p ProtectSystem=strict \
  -p ProtectHome=read-only -p PrivateDevices=yes -p NoNewPrivileges=yes -p RestrictAddressFamilies=AF_UNIX \
  -p SystemCallArchitectures=native -p ReadWritePaths=$PWD/target/bench -E ORT_DYLIB_PATH=… \
  $PWD/target/release/nirlock-bench --models $PWD/models --frames … --template … --threads 4 --iters 30
```

Hardening of the binaries themselves (`readelf`): `GNU_RELRO` segment,
`FLAGS BIND_NOW`, `FLAGS_1 NOW PIE`, `GNU_STACK RW` — rustc's defaults on
`x86_64-unknown-linux-gnu`; `.cargo/config.toml` restates them.

### Parity with fuprobe (OpenCV)

Two checks, both reproducible from the repository:

1. **Own-row cosine** (`nirlock-bench`, column `*_own_cos`): for each lit
   frame that fuprobe accepted into the `lit` template (54 of the 75, per
   `auraface_lit.src.tsv` / `sface_lit.src.tsv`), the cosine between the
   Rust pipeline's embedding of that frame (YuNet → 5-point Umeyama
   alignment → hand-written bilinear warp → embedder under ORT) and the row
   fuprobe stored for that very frame:

   | embedder | own rows | min | median | ≥ 0.99 |
   |---|---|---|---|---|
   | AuraFace | 54 | **0.999996** | 1.000000 | 54/54 |
   | SFace | 54 | **0.999999** | 1.000000 | 54/54 |

   Max-over-rows (the old column, kept in the JSON): 57/75 frames ≥ 0.95;
   the 18 below are frames fuprobe's quality gate rejected (saturated start
   frames, head-turn frames 60–66), whose nearest template row is a
   different pose (min 0.281 AuraFace, 0.506 SFace).

2. **Per-frame diff against OpenCV** (`nirlock-bench --dump` +
   `bench-opencv --dump` + `tools/bench-opencv/compare.py`, all 75 lit
   frames, run on tmpfs and deleted afterwards): detection agrees on 75/75
   frames; landmarks differ by **≤ 0.001 px**, scores by ≤ 0.0001, boxes
   IoU **≥ 0.99998**; embeddings of the same frame: AuraFace cos
   **≥ 0.9999962**, SFace cos **≥ 0.9999982** (median 1.0000000 both).
   OpenCV's 5-bit fixed-point warp and the float warp differ by less than the
   embedders can see. This is the M2 gate (IoU ≥ 0.98, landmarks ≤ 0.5 px,
   cos ≥ 0.99 per frame) already passing on this session; M2 repeats it on
   `enroll-b` and the replay sessions.

```
nirlock-bench … --dump /tmp/x && tools/bench-opencv/build/bench-opencv … --dump /tmp/x/cv.jsonl
python3 tools/bench-opencv/compare.py /tmp/x/<session>.rust.dump.jsonl /tmp/x/cv.jsonl
```

## M0b — ONNX Runtime **on battery**, Arch `onnxruntime-cpu` 1.29.0-3 (2026-09-23)

Same binary and command as M0, `ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so.1`
(the Arch package, ONNX Runtime 1.29.0, `api-27` loaded without warnings).
**Power: battery** (`AC0/online = 0`, 100 %), `governor=powersave`,
`epp=balance_power`, `platform_profile=balanced`. 75 lit enroll-a frames,
40 iterations, YuNet intra 1 in every row. Load average 0.1–1.9 (M1 agents
were idle or reviewing; no builds during the 4-thread run).

| embedder threads | YuNet load / first / median / p90 | AuraFace load / first / median / p90 | SFace load / first / median / p90 |
|---|---|---|---|
| 1 | 53 / 9.4 / 8.35 / 8.42 | 638 / 378 / **378.1** / 379.1 | 29 / 45.7 / 46.2 / 47.1 |
| 2 | 63 / 9.7 / 8.50 / 8.56 | 652 / 258 / **250.9** / 251.9 | 30 / 37.9 / 34.2 / 34.9 |
| 4 | 60 / 9.5 / 8.41 / 8.57 | 656 / 184 / **148.9** / 178.6 | 29 / 23.1 / 25.4 / 27.2 |
| 8 | 53 / 9.4 / 8.38 / 8.42 | 647 / 133 / **114.8** / 132.4 | 30 / 21.5 / 22.6 / 23.7 |

RSS after loads 490 MiB, VmHWM 552 MiB (identical to AC). Parity identical
(own-row cosine 54/54 = 1.000 for both embedders).

Reading: on battery everything is ~2.5–3× slower than on AC (AuraFace 149 vs
56 ms at 4 threads; load 656 vs 297 ms), which matches the Phase-0 OpenCV
figure of ~150 ms/frame and 570 ms load measured on battery. So the ORT
runtime is not the variable; the power state is. At 4 threads AuraFace
(149 ms) is slightly slower than the 133 ms lit-frame cadence; 8 threads
brings it to 115 ms but on this hybrid CPU 8 intra-op threads compete with the
UI and were 2–3× worse under desktop load in Phase 0, so 4 stays the default
and M3 measures the real request loop (P-core migration, spinning) before
touching it. Cold AuraFace load on battery (~650 ms) confirms ADR-0004:
models must be pre-warmed on lock, never loaded inside a verify.

### Earlier M0 runs (superseded)

Intra-op spinning A/B (4 threads, 60 iterations, old bench order): spinning
on gave 55–58 ms medians and ~10 ms lower p90 at the cost of 24 % more CPU
time over the same wall time; kept **off** (ADR-0015), revisit in M3 with the
real request loop. The first M0 table (YuNet at N threads, AuraFace session
dropped before SFace load, VmHWM ~800 MiB) is replaced by the tables above.

## M2 — paridad de visión con `fuprobe` (2026-09-27)

El pipeline completo en Rust (YuNet → filtro de calidad → alineación →
AuraFace/SFace → coseno máximo contra la plantilla) reproducido sobre
sesiones ya grabadas con `nirlock-replay`, y comparado fila por fila con el
CSV que produjo `fuprobe score` sobre la **misma** sesión y plantilla. Este
es el criterio de aceptación de DESIGN §10: el daemon debe aceptar y
rechazar exactamente los mismos frames que el prototipo del que salieron
todos los números de `PHASE0-RESULTS.md`.

```
target/release/replay --session <lab>/data/sessions/20260919-234211_enroll-b \
  --template <lab>/data/templates/rodrigo_a --models models --csv rust_b_vs_a.csv
```

**Sesión `enroll-b` contra la plantilla `rodrigo_a`, 75 frames iluminados:**

| Comparación | Resultado |
|---|---|
| Decisión del filtro (`accepted` / `pose` / `saturated` / …) | **0 discrepancias en 75** (69 aceptados, 4 pose, 2 saturados, idéntico a `fuprobe`) |
| Caja del rostro (x, y, w, h) | idéntica a la precisión impresa; 4 valores de 300 caen en empates exactos `.5`, donde C++ y Python redondean al revés |
| `best_score` del detector | máx. \|Δ\| 0,000049 |
| `roll_deg` / `yaw` / `pitch` | máx. \|Δ\| 0,050° / 0,00050 / 0,00049 |
| `sat_in_box` / `box_mean` | máx. \|Δ\| 0,000005 / 0,005 |
| Coseno máximo SFace (`lit` / `diff`) | máx. \|Δ\| 0,000092 / 0,000162 |
| Coseno máximo AuraFace (`lit` / `diff`) | máx. \|Δ\| 0,000606 / 0,000573 |

El umbral de M2 pedía coseno ≥ 0,99 por frame entre ambas implementaciones;
la diferencia real de los puntajes está tres órdenes de magnitud por debajo
de eso. Las diferencias residuales son de aritmética en punto flotante y de
la interpolación bilineal propia frente a la de punto fijo de 5 bits de
OpenCV, no de lógica.

**Sesión `spoof-phone` (teléfono OLED mostrando una selfie del usuario):**
59 frames iluminados, **0 candidatos del detector** incluso con el piso bajo
de 0,30, `no_face` en los 59. Reproduce el resultado de la compuerta G1: en
infrarrojo la pantalla no existe.

Pendiente de M2: rehacer los máximos de impostores de E5 con los embeddings
de Rust (±0,005), que exige pasar ~20 000 imágenes de LFW y DROZY por el
pipeline; el A/B del RGB de asistencia con n ≥ 20; el umbral de saturación
(¿5 % → 10–15 %?); y E13, que necesita luz de día.

### Umbral de saturación: relajado de 5 % a 15 % (ADR-0016)

Pregunta abierta desde E4: a oscuras la auto-exposición quema el rostro y no
se corrige, y el filtro lo rechazaba, costando 4 de 40 desbloqueos. Medido
ahora con los datos que ya teníamos, sin cámara.

**Impostores** (26.327 frames de E5, agrupados por la razón del filtro):

| Población | n | Máx. SFace | Máx. AuraFace |
|---|---|---|---|
| Aceptados por el filtro de 5 % | 19.748 | 0,4716 | 0,3921 |
| Rechazados por saturación | 63 | 0,3630 | 0,2281 |
| Techo combinado | | 0,4716 (**Δ +0,0000**) | 0,3921 (**Δ +0,0000**) |

**Genuinos** (dos sesiones a oscuras replicadas contra la plantilla
`rodrigo`; los frames que el filtro de 5 % rechazaba):

| Saturación en la caja | AuraFace | SFace |
|---|---|---|
| 0,066 – 0,176 | 0,64 – 0,71 | 0,76 – 0,82 |
| 0,299 | 0,74 | 0,78 |
| 0,461 – 0,497 | 0,53 – 0,55 | 0,64 – 0,70 |

Todos superan el umbral de 0,45. Hasta ~0,20 el peor caso es 0,64 (margen
0,19); pasado 0,30 el margen baja a 0,08, así que el límite nuevo es 0,15 y
no "sin límite". Es la única corrección medida para el régimen de exposición
que quema: las escrituras de control están descartadas (ADR-0005) y E6 mostró
que ni la ROI ni el modo D1 ayudan.

### A/B del RGB de asistencia, n = 20 por rama (2026-09-27)

20 arranques en frío alternados, 2 s cada uno, 7 s de separación para que el
USB autosuspenda. Primer frame IR entregado, en ms desde antes del `open()`:

| Rama | Mediana | p90 | Máx. | Corridas lentas |
|---|---|---|---|---|
| Solo IR | 253 | 255 | 255 | 0 de 20 |
| IR + RGB | 254 | 451 | 452 | **4 de 20** (448–452) |

La distribución es **bimodal**: cada corrida es ~253 o ~451 ms, nunca algo
intermedio, y la diferencia son exactamente tres slots de 66,7 ms. Con
`first_sequence = 1` en ambos casos, no se pierden frames: el stream IR
arranca tres slots tarde cuando la interfaz RGB ya está transmitiendo. Ver
ADR-0006 para la consecuencia de política (pasar a `auto` por el sensor de
luz, sobre todo ahora que ADR-0016 cubre buena parte del mismo fallo).

## M3 — motor de petición de extremo a extremo (2026-09-27, **batería**)

`nirlockd bench --trials 8 --template rodrigo --models models`, AuraFace a 4
hilos, umbral 0,45, regla K=2 de W=4. Modelos cargados una vez (942 ms en
frío, en batería) y **tibios para todas las pruebas**, que es exactamente lo
que hará el daemon tras el `prewarm` del lock. Cada prueba parte con la
cámara autosuspendida (7 s de reposo).

| Etapa (ms desde antes del `open()`) | Mediana | Mín. | Máx. |
|---|---|---|---|
| Primer frame (siempre iluminado) | 254 | 251 | 255 |
| Primer rostro que pasa el filtro | 254 | 251 | 255 |
| Primer embedding | 459 | 416 | 532 |
| **Decisión K=2** | **629** | 573 | 687 |

Aceptadas 6 de 8, con los mejores cosenos entre 0,900 y 0,933. Las dos
pruebas fallidas son los momentos en que el usuario miró hacia otro lado
durante los dos minutos de la corrida: 11 y 28 rechazos por pose, ningún
frame llegó a puntuarse. Es el filtro haciendo su trabajo, no un fallo del
motor.

**Criterio de aceptación de M3: mediana K=2 ≤ 800 ms tibio → cumplido con
629 ms, y en batería**, que es el caso lento (en AC AuraFace cuesta 56 ms por
frame en vez de 149). El número coincide con los 634 ms que midió `fuprobe`
con OpenCV en las mismas condiciones, lo que confirma una vez más que el
runtime no es la variable.

Sobre el reparto: 254 ms son de la cámara y son irreducibles sin escrituras
de control; los ~205 ms hasta el primer embedding son AuraFace en batería; y
el segundo frame iluminado no puede llegar antes de 133 ms después del
primero. El piso teórico de esta máquina con esta regla es ~590 ms.

### Latencia vista por el cliente, a través del socket (batería)

`nirlockd serve` con los modelos residentes, y un cliente que hace
`hello` → `verify` y cronometra hasta el `result`. Seis peticiones con 7 s de
reposo entre ellas, para que cada una arranque con la cámara autosuspendida:

| | Cliente | Daemon |
|---|---|---|
| Mediana | **627 ms** | 626 ms |
| Mínimo / máximo | 623 / 690 | 622 / 689 |

Aceptadas 6 de 6. El IPC cuesta ~1 ms sobre lo que el propio daemon mide, así
que el número que sentirá el lock screen es el del motor. La primera petición
tras arrancar el daemon dio 822 ms (la cámara venía de otro estado); a partir
de la segunda se estabiliza.

### Matriz de autoridad (PROTOCOL §4), ejercitada de extremo a extremo

| Caso | Respuesta | Tiempo |
|---|---|---|
| Usuario correcto, de frente | `accept/match` | 623 ms |
| Nonce repetido | `reject/replayed_nonce` | 0 ms |
| Pedir ser `root` desde uid 1000 | `reject/wrong_user` | 0 ms |
| Cliente `lock` pidiendo `verify` | `unavailable/forbidden` | 0 ms |
| Cliente `ctl` pidiendo `verify` | `unavailable/forbidden` | 0 ms |
| `lane` distinto de `lock` | `unavailable/forbidden` | 0 ms |
| `rhost` no vacío (sesión remota) | `reject/remote` | 0 ms |

Lo importante es la última columna: **todos los rechazos cuestan 0 ms**, o sea
que la cámara no se abre ni el emisor se enciende para una petición que la
política va a negar. La comprobación de identidad es `SO_PEERCRED`, que da el
kernel en el `connect(2)` y el par no puede falsificar después; lo que el
cliente dice de sí mismo solo sirve para el registro.
