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

- **`nirlockd`** — a sandboxed, non-root Rust daemon. It is the only thing
  that **decides**: it labels every frame with the firmware's
  `FrameIllumination` metadata, recognises with AuraFace, and answers. Nothing
  else can authenticate you. (`nirlockctl enroll`, `preview` and `probe` open
  the camera too, as you, to show you a preview or measure your hardware —
  they never produce an authentication.)
  nirlock sends no UVC control writes, ever.
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

## What has not been tested

This is the list the author would want to read before trusting something with
their lock screen, so it is on the front page rather than buried.

- **Printed photographs: never tested.** Nobody here owns a printer. The
  argument against them is structural — recognition only scores frames the
  firmware labelled as emitter-lit, and paper does not respond to an infrared
  emitter the way skin does — but structural is not measured.
- **Screens: measured against one screen technology.** A max-brightness OLED
  phone showing the enrolled face gave zero detector candidates across 59 lit
  frames. An **LCD was never tried**, and its white LED backlight has a
  near-infrared tail an OLED lacks. This one is open, not settled.
- **3D masks: never tested.** There is no presentation-attack detection beyond
  the illumination label.
- **The 0.45 threshold rests on one face.** It sits above every impostor score
  seen across 4,875 identities used as a proxy, but multi-day validation
  across more faces has not happened.
- **Verified end to end on exactly one camera.** Every number in `docs/` comes
  from a single Shinetech `3277:0055` in an ASUS Zenbook UX3405CA.

One measurement disagrees with ours and belongs here rather than buried:
[sovren-software/visage](https://github.com/sovren-software/visage) reports a
hand-held phone screen matching an enrolled identity at **0.9013 on this same
camera module**. Their pipeline does not use the per-frame illumination
metadata to select emitter-lit frames, so it measures a system without the
gate nirlock's argument rests on — but you should see it and judge.

[`SECURITY.md`](SECURITY.md) and
[`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) have the rest.

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

It lists every USB camera, says what each node is, streams the infrared ones
for two seconds, and gives a verdict: supported, usable (with the profile
printed, ready to save), unusable **with the concrete reason**, or — when
something else held the camera — no verdict at all, which is explicitly not a
statement about your hardware.

Streaming lights the camera: the white LED comes on and the emitter pulses.
probe says so before it does it.

```
USB 3277:0055 'USB2.0 FHD UVC WebCam' (fixed)
  /dev/video0    iface 0 index 0  MJPG 1280x720
  /dev/video1    iface 0 index 1  UVCH (metadata)
  /dev/video2    iface 2 index 0  GREY 640x360
  /dev/video3    iface 2 index 1  UVCM (metadata)
  strobe:        firmware strobe measured: 28 frames in 2 s, 14 lit / 14 dark, alternating
  -> supported: profile 'shinetech-3277-0055' from /usr/share/nirlock/hw/3277-0055.toml
```

Three things decide it, and probe checks all three: a `GREY` capture node (the
IR sensor), a `UVCM` metadata node beside it on the same interface, and the
`strobe:` line, which comes from actually streaming the camera for two seconds
and watching the per-frame illumination label.

### Why your camera might not qualify

nirlock sends no UVC control writes, ever ([ADR-0005](docs/adr/) — sibling
cameras have been bricked by probing extension units). So the emitter has to
strobe by itself, from firmware.

(The *kernel* does send one on its own behalf: `uvc_meta_detect_msxu()` issues
a `SET_CUR` on MSXU selector `0x09` when the metadata node is opened. That
happens with or without nirlock, and it is recorded in ADR-0005 rather than
left for a reader to discover.)

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
NIRLOCK_HW_DIRS=hw target/release/nirlockctl probe   # read this before going on
make -C pam
scripts/nirlock-fetch-models          # 286 MB, SHA-256 pinned, no root
sudo scripts/nirlock-install
```

If probe says **supported**, continue. If it says **usable** it printed a
profile for your camera: save it where it tells you and run probe again. If it
says it **cannot be used**, the reason is the answer, and
[Contributing](#contributing) explains why that report is still worth sending.

The installer runs the same checks first (ONNX Runtime present, the models
fetched, a camera whose strobe it can measure) and installs nothing if any
fails. Then it sets up the
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

## Contributing

There is one camera and no printer behind this project. That bounds what can
be known here, and it is the whole reason the list above is as long as it is.
[`CONTRIBUTING.md`](CONTRIBUTING.md) has the detail; this is the short version.

**Run `nirlockctl probe` and send the output, whatever it says.** Two seconds,
no install, no root. A camera that **cannot** work is as valuable as one that
can: it turns somebody else's dead end into a diagnosis instead of a week of
debugging. There is a
[form](https://github.com/lordyoyi/nirlock/issues/new?template=camera-profile.yml)
that asks for exactly what is needed.

**If your camera does work**, `nirlockctl probe --toml` prints the profile.
Install it, enrol, unlock — then send it as a
[pull request](https://github.com/lordyoyi/nirlock/compare) or through the same
form. Say whether you verified it; "not yet" is an explicit option and an
honest answer, and it will be labelled that way rather than published as fact.

**And the things that cannot be done here at all**, in the order they would
change this project most:

| | Why it is not done here |
|---|---|
| Hold a **printed photo** in front of it and report what happens | No printer |
| Try an **LCD** screen, not an OLED | Only an OLED to hand, and the physics differ |
| Enrol a **second face** and report the scores | One face |
| Run it on a kernel or distribution other than Arch 7.x | One machine |
| Use the PAM lane under a lock screen that is not Omarchy's | Same |

A negative result gets published next to a positive one. If you break it,
that is the most useful message this project can receive.

## Security

- [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) — what this defends against,
  and what it does not
- [`docs/DESIGN.md`](docs/DESIGN.md) (Spanish) and
  [`docs/adr/`](docs/adr/) — the decisions and why
- [`docs/PROTOCOL.md`](docs/PROTOCOL.md) — the daemon's wire protocol

Found a security problem? **Do not open a public issue.** Use
[private vulnerability reporting](https://github.com/lordyoyi/nirlock/security/advisories/new),
which is enabled on this repository. [`SECURITY.md`](SECURITY.md) says what is
in scope, what is not, and what is known to be unmeasured.

## Development

```
cargo build --release --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
make -C pam test          # pam_nirlock.so + dlopen + lane harness through libpam
omarchy-plugin-validate plugin/lock
/usr/lib/qt6/bin/qmllint -I /usr/lib/qt6/qml plugin/lock/*.qml
```

`[workspace.lints]` denies undocumented `unsafe` and warns on `unwrap` outside
tests. `nirlock-wire` and `nirlockctl` are `#![forbid(unsafe_code)]`, which
cannot be locally overridden. `nirlockd` and `nirlock-vision` are
`#![deny(unsafe_code)]` with one documented exemption each — `SO_PEERCRED` via
`libc::getsockopt` (`peer_cred` is still unstable) and reading the ONNX Runtime
version string. `nirlock-cam` carries the V4L2 ABI and its ioctl surface is
enumerated and tested.

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
