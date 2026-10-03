# Contributing

There is one camera and no printer behind this project. Every number in
`docs/` comes from a single Shinetech `3277:0055` in an ASUS Zenbook UX3405CA,
measured by one person with one face. That bounds what can be known here, and
it is why the most useful thing you can send is not code.

## The two-second one

```
nirlockctl probe
```

No install, no root, no 286 MB download. It lists your cameras, streams the
infrared ones for two seconds, and says what it measured.

**Send the output whatever it says.** A camera that cannot work is as valuable
as one that can — more so, in fact. Today someone with a Chicony or a Bison
module reads "cannot be used", shrugs, and the next person with that laptop
spends an evening finding out the same thing. A recorded negative turns that
into one line of documentation.

Use the [Add a camera](https://github.com/lordyoyi/nirlock/issues/new?template=camera-profile.yml)
form. It asks for the probe output, your laptop model, your kernel, and
whether you verified anything — it takes about a minute.

## If your camera works

```
nirlockctl probe --toml > hw/<vendor>-<product>.toml
```

Then **verify it for real**: install it, enrol, unlock your screen with your
face. A profile that parses but does not work is worse than none, because it
is published as supported and sends the next person to debug a file we
generated.

Send it as a [pull request](https://github.com/lordyoyi/nirlock/compare) or
through the same form. The PR template asks whether you verified it; **"not
yet" is an explicit option and an honest answer.** It will be merged as
untested and labelled that way rather than published as fact.

One borrowed rule worth stating, from the
[visage](https://github.com/sovren-software/visage) project: before you
measure, disable any other infrared emitter tool and power-cycle the camera. A
camera left illuminated by something else makes a non-working profile look
correct.

## The things that cannot be done here at all

In the order they would change this project most. Any one of these is a
genuine contribution, and a negative result gets published next to a positive
one.

### 1. Hold a printed photo in front of it

The single biggest gap. No printer here, so it has never been tried.

Print a good frontal photo of the enrolled face, hold it where the face would
be, and report what the daemon says:

```
journalctl -u nirlockd -n 5 --no-pager
```

Three outcomes and all three are worth sending: `no_face` (the photo is
invisible in infrared, which is the expected and good result), a score below
threshold, or — the one that matters — a match. **If it matches, say so
immediately**, through
[private vulnerability reporting](https://github.com/lordyoyi/nirlock/security/advisories/new).

### 2. Try an LCD screen

Screen spoofing was measured against a max-brightness **OLED** phone: zero
detector candidates across 59 lit frames. An LCD has never been tried, and its
white LED backlight has a near-infrared tail an OLED lacks, so it may behave
differently. A laptop screen, an older tablet or a monitor showing the
enrolled face would close this.

### 3. Enrol a second face

The 0.45 threshold sits above every impostor score seen across 4,875
identities used as a proxy — but it was set from one real face. A second
enrolled person reporting their own genuine scores (`nirlockd bench` prints
them) is how that stops being an extrapolation.

### 4. A kernel or distribution that is not this one

Everything is measured on Arch with kernel 7.x. The metadata this depends on
needs 6.17 or newer; what happens on 6.17, 6.19 or a Debian backport is
unknown.

### 5. The PAM lane under another lock screen

The daemon and the PAM module are not Omarchy-specific; only the plugin is.
Nobody has wired the lane into swaylock, hyprlock, or a display manager.

## Code

```
cargo build --release --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
make -C pam test
```

All four must pass. `cargo fmt --check` is **not** clean on master (older
code); do not reformat the tree inside a PR about something else.

Two rules that are not style preferences:

- **The password lane always works.** No change may leave the lock screen
  without a password field.
- **No UVC control writes, ever** (`ADR-0005`). Sibling cameras have been
  bricked by probing extension units; a project in this space wedged one badly
  enough that the laptop went in for service. The emitter must strobe on its
  own or the camera is not supported.

Commit messages: a short title in natural language and a body that explains
**why**, including the incident that motivated it. The existing history is the
style guide.

## Security

Do not open a public issue for a vulnerability. See
[`SECURITY.md`](SECURITY.md), which also lists what is known to be unmeasured.
