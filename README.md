# nirlock

```
         ███╗   ██╗██╗██████╗ ██╗      ██████╗  ██████╗██╗  ██╗
         ████╗  ██║██║██╔══██╗██║     ██╔═══██╗██╔════╝██║ ██╔╝
         ██╔██╗ ██║██║██████╔╝██║     ██║   ██║██║     █████╔╝
         ██║╚██╗██║██║██╔══██╗██║     ██║   ██║██║     ██╔═██╗
         ██║ ╚████║██║██║  ██║███████╗╚██████╔╝╚██████╗██║  ██╗
         ╚═╝  ╚═══╝╚═╝╚═╝  ╚═╝╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝

                              ████████████
                            ████████████████
                          ████████████████████
                         ██████████████████████
                        ████████████████████████

                       ██████████████████████████

                      ████████████████████████████

────────────────────────────────────────────────────────────────────────
                            \ \ \ \ │ / / / /
                    \   \   \   \   │   /   /   /   /
            \     \     \     \     │     /     /     /     /
    \       \       \       \       │       /       /       /       /

                    l o o k   a t   y o u r   l a p t o p
```

Windows Hello-style face unlock for Linux laptops that have a near-infrared
camera — the separate IR sensor next to the webcam, not the webcam itself.

Open the lid, look at the screen, you are in. Your password always works in
parallel; face never replaces it.

**Status: working, lightly travelled.** It unlocks this author's machine every
day in 0.5–1.7 s. It has been verified end to end on exactly **one** camera
(see [Does my laptop work?](#does-my-laptop-work)). If yours is different,
`nirlockctl probe` will tell you in one command whether it can work, and
generate the profile that makes it.

## What it is

- **`nirlockd`** — a sandboxed, non-root Rust daemon, the only process that
  opens the IR camera. It labels every frame with the firmware's
  `FrameIllumination` metadata, recognises with AuraFace, and decides.
  It writes no UVC controls, ever.
- **`pam_nirlock.so`** — a ~300-line C PAM module that relays the decision.
  It never returns `PAM_IGNORE`; every failure path ends in `pam_deny`.
- **An Omarchy lock-screen plugin** that wraps the stock lock without
  modifying it and adds a face lane beside the password one, plus a
  synthwave scan indicator that follows your theme.

Enrolled face templates live in `/var/lib/nirlock`, owned by the daemon's own
unprivileged user and unreadable by your desktop session.

## What it is not

Face is a **convenience factor for the lock screen only**. It does not unlock
LUKS, keyrings, `sudo`, `polkit`, or the display-manager login. That is a
deliberate scope limit, not a missing feature —
[`docs/POR-QUE-NO-EL-LOGIN.md`](docs/POR-QUE-NO-EL-LOGIN.md) explains why.

Two things a reader should not assume:

- **The printed-photo spoof test has not been run yet.** The defence against
  it is real and structural — recognition requires frames the firmware
  labelled as IR-illuminated, and a photo on paper does not respond to an IR
  emitter the way skin does — but *structural* is not *measured*. See
  [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md).
- **The match threshold is still under live validation.** It was set from
  measured impostor distributions on one face.

## Requirements

| | |
|---|---|
| Kernel | **6.17 or newer** — needs `V4L2_META_FMT_UVC_MSXU_1_5` for the per-frame illumination metadata |
| Camera | A Windows Hello IR camera whose emitter strobes **on its own**. Windows face unlock is necessary but not sufficient: Windows writes a vendor control to light the emitter, and nirlock never writes one (see [Why your camera might not qualify](#why-your-camera-might-not-qualify)). `nirlockctl probe` measures this. |
| Desktop | The lock-screen plugin is [Omarchy](https://omarchy.org)-specific. The daemon and PAM module are not. |
| Runtime | ONNX Runtime (`sudo pacman -S onnxruntime-cpu` on Arch), loaded dynamically |

## Does my laptop work?

```
nirlockctl probe
```

It lists every USB camera, says what each node is, and gives a verdict:
supported, usable (with the profile printed, ready to save), or unusable
**with the concrete reason**.

```
USB 3277:0055 'USB2.0 FHD UVC WebCam' (fixed)
  /dev/video0    iface 0 index 0  MJPG 1280x720
  /dev/video1    iface 0 index 1  UVCH (metadata)
  /dev/video2    iface 2 index 0  GREY 640x360
  /dev/video3    iface 2 index 1  UVCM (metadata)
  -> supported: profile 'shinetech-3277-0055' from /usr/share/nirlock/hw/3277-0055.toml
```

Three things decide it, and probe checks all three: a `GREY` capture node (the
IR sensor), a `UVCM` metadata node beside it on the same interface, and the
`strobe:` line, which comes from actually streaming the camera for two seconds
and watching the per-frame illumination label.

### Why your camera might not qualify

nirlock writes no UVC controls, ever ([ADR-0005](docs/adr/) — sibling cameras
have been bricked by probing extension units). So the emitter has to strobe by
itself, from firmware.

Many Windows Hello cameras do not: they sit dark until the host writes a vendor
control, which is what Windows does and what nirlock refuses to do. In the
published record of measured cameras this is the common case, not the rare one,
and it tracks the module vendor more than the laptop brand.

This is why probe streams instead of reading descriptors. A camera can have the
IR sensor, the metadata node, the right formats, and still never light up.

## Install

```
sudo pacman -S onnxruntime-cpu        # Arch; `onnxruntime` is a virtual provide, and the
                                      # menu it opens includes CUDA builds you do not need
git clone https://github.com/lordyoyi/nirlock && cd nirlock
cargo build --release --locked
target/release/nirlockctl probe       # must say "supported" — stop here if it does not
make -C pam
scripts/nirlock-fetch-models          # 286 MB, SHA-256 pinned, no root
sudo scripts/nirlock-install
```

The installer runs the same checks first (ONNX Runtime present, a camera
with a profile) and installs nothing if either fails. Then it sets up the
daemon's user, the sandbox, the PAM lane, the Omarchy plugin and the menu
entry, and walks you through enrolment.
`scripts/nirlock-uninstall` undoes all of it and verifies your lock screen is
back before removing anything.

Enrol (or re-enrol) at any time — it needs root, because the templates
belong to the daemon's user:

```
sudo nirlockctl enroll            # first time
sudo nirlockctl enroll --force    # re-enrol
```

It shows a live IR preview in the terminal and guides you through five looks,
in one continuous capture that never throws away work you have already done.

## Contributing a camera

This is the most useful thing you can send. Hardware profiles are plain TOML
in `hw/`, matched against your camera's USB vendor:product:

```
nirlockctl probe --toml > hw/04f2-b6d0.toml
```

Verify it for real — install it, enrol, unlock — then send it, either as a
[pull request](https://github.com/lordyoyi/nirlock/compare) or through the
[Add a camera](https://github.com/lordyoyi/nirlock/issues/new?template=camera-profile.yml)
issue form if you would rather not open a PR. A profile that parses but does
not work is worse than none.
[`docs/HARDWARE.md`](docs/HARDWARE.md) has the details, including what makes a
camera qualify and what the profile may and may not change.

## Security

- [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) — what this defends against,
  and what it does not
- [`docs/DESIGN.md`](docs/DESIGN.md) (Spanish) and
  [`docs/adr/`](docs/adr/) — the decisions and why
- [`docs/PROTOCOL.md`](docs/PROTOCOL.md) — the daemon's wire protocol

Found a security problem? Open an issue, or mail the address in `LICENSE` if
it should not be public first.

## Development

```
cargo build --release --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
make -C pam test          # pam_nirlock.so + dlopen + lane harness through libpam
omarchy-plugin-validate plugin/lock
/usr/lib/qt6/bin/qmllint -I /usr/lib/qt6/qml plugin/lock/*.qml
```

`[workspace.lints]` denies undocumented `unsafe` and warns on `unwrap`
outside tests. `nirlock-wire`, `nirlockd` and `nirlockctl` are
`#![forbid(unsafe_code)]`; `nirlock-vision` is `#![deny(unsafe_code)]` with
one documented exemption; `nirlock-cam` carries the V4L2 ABI and its ioctl
surface is enumerated and tested.

One note if you edit the Omarchy plugin: changed QML needs
`omarchy restart shell`. Disabling and re-enabling the plugin re-instantiates
the *old* compiled code — Quickshell caches compiled types by file URL, and
the "Local plugin changed, reloading" log line does not mean it recompiled.

Measurements, and the parity check against the Phase-0 OpenCV reference:
[`docs/BENCH.md`](docs/BENCH.md). Frames, templates and bench dumps are
biometric data: they are git-ignored here and belong in a separate private
repository.

## Licence

MIT — see [`LICENSE`](LICENSE). The models keep their own (YuNet MIT; SFace
and AuraFace Apache-2.0): [`THIRD-PARTY.md`](THIRD-PARTY.md).
