# Security

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:
**[Report a vulnerability](https://github.com/lordyoyi/nirlock/security/advisories/new)**
(the Security tab of this repository). It is enabled, and it reaches the
maintainer without the report being public first.

Please do not open a public issue for a vulnerability.

The README used to say "mail the address in `LICENSE`". `LICENSE` has no
address in it, so that sentence left a reporter with no private route at all.
Fixed on 2026-10-03.

## What is in scope

- Anything that authenticates someone who should not be authenticated.
- Anything that leaks an enrolled template, a face image, or a similarity
  score off the machine or to another local user.
- Any path where the PAM lane fails open, or where the lock screen can be
  dismissed without a successful authentication.
- The installer and uninstaller, which run as root.

## What is NOT a vulnerability here

- **Face unlock being weaker than a password.** It is a convenience factor for
  the lock screen only. It never unlocks LUKS, keyrings, `sudo`, `polkit` or
  the display-manager login, and the password lane always works in parallel.
  See [`docs/POR-QUE-NO-EL-LOGIN.md`](docs/POR-QUE-NO-EL-LOGIN.md).
- A camera that cannot work. That is hardware; `nirlockctl probe` says why.

## What we know we have not measured

A security policy that only lists what it defends against is advertising. This
is the other half, and it is the honest reason to read
[`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) before trusting this:

- **Printed photographs have never been tested.** Nobody here owns a printer.
  The structural argument is that recognition only scores frames the firmware
  labelled as emitter-lit, and paper does not respond to an infrared emitter
  the way skin does — but structural is not measured.
- **Screen spoofing was measured against one screen technology.** A
  max-brightness OLED phone showing the enrolled face produced zero detector
  candidates across 59 lit frames. An **LCD** was never tried, and its white
  LED backlight has a near-infrared tail an OLED lacks, so it may behave
  differently. This is an open question, not a settled one.
- **The match threshold (0.45) rests on one face.** It is above every impostor
  score seen across 4,875 identities used as a proxy, but multi-day validation
  on more faces has not happened.
- **Verified end to end on exactly one camera.** Everything measured in
  `docs/` comes from a single Shinetech `3277:0055` in an ASUS Zenbook
  UX3405CA.
- **A 3D mask has never been tested**, and no presentation-attack detection
  beyond the illumination label exists.

If you can close any of these, see
[`CONTRIBUTING.md`](CONTRIBUTING.md) — a negative result is as useful as a
positive one and will be published either way.

## A measurement that disagrees with ours

[sovren-software/visage](https://github.com/sovren-software/visage) reports a
hand-held phone screen matching an enrolled identity at 0.9013 on this same
camera module. Their pipeline does not use the per-frame illumination metadata
to select emitter-lit frames, so it measures a system without the gate that
nirlock's argument rests on — but it is recorded here rather than left out,
because a reader deciding whether to trust this should see it.
