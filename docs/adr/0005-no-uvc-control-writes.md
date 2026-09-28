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
