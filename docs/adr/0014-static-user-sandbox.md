# ADR-0014 — Static system user `nirlock`, full systemd sandbox, no ProcSubset=pid

Status: accepted; validated as a real system unit in M3 (E10).

## Context
The daemon must not run as root, must reach `/dev/video*` (`root:video 0660`), the system bus, journald, `/proc/sys/kernel/random/boot_id` and `/proc/cpuinfo` (used by `cpuinfo`, a dependency of Arch's `onnxruntime-cpu`). Reviews found `ProcSubset=pid` hides the last two, `RuntimeDirectory=` on the service deletes the socket unit's path on every stop, `~@resources` breaks an in-process `setrlimit`, and `DynamicUser` interacts badly with `RuntimeDirectoryPreserve`.

## Decision
`User=nirlock` (sysusers.d) + `SupplementaryGroups=video systemd-journal`; `StateDirectory=nirlock` 0700; **no** `RuntimeDirectory=` (the socket unit owns `/run/nirlock`); `ProcSubset=all` with `ProtectProc=invisible`; `DevicePolicy=closed` + `DeviceAllow=char-video4linux rw`; `PrivateNetwork`, `RestrictAddressFamilies=AF_UNIX`, `MemoryDenyWriteExecute`, `NoNewPrivileges`, empty capability set, `SystemCallFilter=@system-service` minus `@privileged @resources @mount @obsolete @debug @cpu-emulation @module @raw-io @reboot @swap`, `LimitCORE=0`, `LimitMEMLOCK=64M`, `MemoryHigh=900M`/`MemoryMax=1200M` (to be tuned after measuring ORT RSS), `WatchdogSec=30`, `StartLimitIntervalSec=0`, `RestartPreventExitStatus=78`, no `Nice`. In-process: `PR_SET_DUMPABLE=0`, `mlock` of templates, no `setrlimit`.

## Alternatives
`DynamicUser=yes` (equally hardened; rejected for predictable uid, simpler diagnostics and no `RuntimeDirectory` interaction); root daemon (rejected from the start); `ProcSubset=pid` (breaks boot_id/lid/cpuinfo).

## Consequences
`/var/lib/nirlock` is owned by a stable uid; templates unreadable by uid 1000. Any directive that breaks ORT under the real unit is removed with the reason recorded in `docs/HARDENING.md`.
