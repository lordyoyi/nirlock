# Third-party components

nirlock itself is MIT (see `LICENSE`). This file covers everything it ships,
downloads or links against.

## Models

The three ONNX models are **not** in this repository — together they are
286 MB. `scripts/nirlock-fetch-models` downloads them and checks each one
against the SHA-256 pinned below before keeping it. The URLs point at a
branch, so upstream can move; a mismatch aborts the install rather than
quietly substituting a different model into the thing that decides who gets
to log in.

All three are permissively licensed. Verified at source on 2026-09-27.

### YuNet — face detection

| | |
|---|---|
| File | `face_detection_yunet_2026may.onnx` (228 KB) |
| Upstream | [opencv/opencv_zoo](https://github.com/opencv/opencv_zoo), `models/face_detection_yunet/` |
| Licence | MIT, Copyright (c) 2020 Shiqi Yu `<shiqi.yu@gmail.com>` |
| SHA-256 | `ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0` |

Finds the face and its five landmarks in each IR frame. Runs on every frame,
so its size matters more than its accuracy ceiling.

### AuraFace (glintr100) — face embedding, default

| | |
|---|---|
| File | `auraface_glintr100.onnx` (249 MB) |
| Upstream | [fal/AuraFace-v1](https://huggingface.co/fal/AuraFace-v1), `glintr100.onnx` |
| Licence | Apache-2.0 |
| SHA-256 | `a7933ea5330113b01c9b60351d8f4c33003f145d8470ac5f0e52ee2effe25c60` |

Turns an aligned face into the vector that is compared against the enrolled
template. This is the model whose score the 0.45 threshold refers to.

### SFace — face embedding, alternative

| | |
|---|---|
| File | `face_recognition_sface_2021dec.onnx` (37 MB) |
| Upstream | [opencv/opencv_zoo](https://github.com/opencv/opencv_zoo), `models/face_recognition_sface/` |
| Licence | Apache-2.0 |
| SHA-256 | `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79` |

Smaller and faster, measurably weaker at separating faces in the IR band.
Kept as a selectable embedder, not the default. A template embedded with one
model is meaningless to the other, which is why switching embedders forces a
re-enrolment (`model_mismatch`).

## Runtime

**ONNX Runtime** (MIT, Microsoft) is loaded dynamically at run time through
the `ort` crate's `load-dynamic` feature. It is not vendored here; it comes
from the system (`onnxruntime` on Arch).

## Rust dependencies

Direct dependencies: `clap`, `image`, `libc`, `libloading`, `ort`, `serde`,
`serde_json`, `thiserror`, `toml`. All are MIT, Apache-2.0, or dual MIT/
Apache-2.0. The full transitive set, with exact versions, is in `Cargo.lock`;
`cargo license` or `cargo deny check licenses` will enumerate it.

`libc` is used in exactly one file (`crates/nirlockd/src/sys.rs`, for
`SO_PEERCRED`); the V4L2 layer issues its ioctls directly rather than through
a binding crate, which is what lets the ioctl surface be enumerated and
tested (`unsafe_and_ioctl_surface_is_the_declared_one`).

## PAM

`pam/pam_nirlock.c` links against the system PAM library (`libpam`,
BSD-style licence). No PAM source is vendored here.
