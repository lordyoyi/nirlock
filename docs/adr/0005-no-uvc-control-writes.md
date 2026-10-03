# ADR-0005 — The daemon writes no UVC controls

Status: accepted (DECISIONS.md 2026-09-22, confirmed here).

## Context
E6 measured the two spec-documented writes: UVC ROI auto-exposure barely helps (saturation 0.16→0.10, 2/5 accepts) and Microsoft XU FACE_AUTHENTICATION D1 is indistinguishable from D0 on this firmware and is reset by USB autosuspend 2.6 s after close. A sibling camera module was bricked by careless control writes; the Realtek vendor XU (units 4/10/11) and a USB DFU interface exist on this device.

## Decision
`nirlockd` issues only `S_FMT / REQBUFS / QUERYBUF / QBUF / DQBUF / STREAMON / STREAMOFF` and read-only `QUERYCAP / G_FMT`. No `UVCIOC_CTRL_QUERY` at all in the daemon (not even GET; the read-only whitelist survives only in `nirlockctl doctor --hw`). No `S_CTRL`, `S_EXT_CTRLS`, `S_PARM`. Frame labels come only from the `FrameIllumination` metadata (MetadataId 6) on the `UVCM` node, which the firmware emits in D0 with zero writes (480/480 agreement). Missing or inconsistent labels fail closed (`unavailable metadata`), never a brightness fallback.

## Alternatives
Writing D1 before every open (Windows does it; measured no benefit here); ROI AE (no benefit); selector 9 re-arm after resume (a control write; only if E7 shows metadata loss — would need a new ADR).

## Consequences
Dark-room exposure is handled by the RGB lever (ADR-0006) and the per-frame gate, not by controls. If a firmware/kernel change stops the D0 strobe or metadata, face unlock stops working and `doctor` explains; it never guesses.

## Addendum, 2026-10-03: the kernel writes one, and our docs read as if nobody does

This ADR is about what *nirlock* sends, and that is still nothing. But the
stack is not silent: `uvc_meta_detect_msxu()` in
`drivers/media/usb/uvc/uvc_metadata.c` issues a `GET_CUR`, a `GET_MAX` and
then a **`SET_CUR`** on MSXU selector `0x09` when the metadata node is opened,
before `V4L2_META_FMT_UVC_MSXU_1_5` ever appears in the device's formats.

That write happens on any camera exposing the control, with or without
nirlock, and it is the kernel's own. It is recorded here because the previous
wording invited the reading that no MSXU write occurs at all, and anyone with
the kernel source will find otherwise. Stating it ourselves is cheaper than
being corrected.

Nothing follows for the decision: the rule stays "nirlock sends no UVC
control", and the measured reasons behind it are unchanged — the sibling
cameras bricked by extension-unit discovery, the Realtek vendor XU sitting at
units 4/10/11 of the reference camera next to a USB DFU interface, and E6's
finding that the one control worth writing (`FACE_AUTHENTICATION` D1) is
indistinguishable from D0 here and is reset by USB autosuspend anyway.

A further reason surfaced in the 2026-10-03 survey and is worth recording,
because it is the one that would otherwise be argued away: a triggered emitter
is frequently **steady**, not strobing. If every frame is lit, the
`FrameIllumination` bit is constant and carries no information, which deletes
the very signal requirement 2 exists to provide. Relaxing this ADR to light
more cameras would therefore buy hardware coverage by spending the structural
anti-spoofing argument. `nirlockctl probe` now measures that distinction
instead of assuming it.
