# nirlock.lock — Omarchy lock screen + IR face lane

A wrapper plugin for the Omarchy shell (Quickshell). It loads the stock
`omarchy.lock` service unchanged and adds one PAM lane, `nirlock-lock`, read
from the package's own PAM directory `/usr/lib/nirlock/pam.d` and served by
`nirlockd`. The password lane always keeps working; every failure of the face
lane ends in `pam_deny`.

Files: `manifest.json` (schema 1, `clonedFrom: omarchy.lock`, kind
`service`), `Service.qml` (two Loaders: stock service + `FaceLane.qml`),
`FaceLane.qml` (M0: stub), `LICENSE` (MIT).

Install (done by `nirlock-setup`, never by hand while the session is locked):
copy — never symlink — this directory to
`~/.config/omarchy/plugins/nirlock.lock/`, run `omarchy-plugin-validate` on
it, then `omarchy-plugin-enable nirlock.lock`. Disable with
`omarchy-plugin-disable nirlock.lock`; the registry restores `omarchy.lock`.

Status: M0 — the wrapper loads; the face lane is not armed yet.

## Why the manifest declares no capabilities

The stock `omarchy.lock` manifest declares `"omarchy": {"capabilities":
["authentication"]}`. This one deliberately does not, and adding it would be
cargo cult: `PluginRegistry.trustedCapabilities()` returns an empty list for
anything that is not first-party, so a third-party declaration is ignored.
Capabilities reach a clone a different way —
`PluginRegistry.stampHostCapabilities()` copies them from the plugin named in
`clonedFrom`:

```js
function trustedCapabilities(manifest) {
    if (!manifest || !manifest.__isFirstParty) return []   // third-party: none
    ...
}
// third party:
manifest.__hostCapabilities = source.__hostCapabilities.slice()   // from clonedFrom
```

So `"clonedFrom": "omarchy.lock"` is what makes this an authentication
service, with the parenting and lifetime that implies (`shell.qml`
`isAuthenticationService` / `AuthServiceStore`). `omarchy plugin clone`
happens to keep the source's `capabilities` array in the file it writes,
which is why a hand-written clone can look like it is missing something.

## Reloading after a change

Omarchy's own guide says saving a file under `~/.config/omarchy/plugins/`
reloads it, with `omarchy-shell shell rescanPlugins` as the fallback. That
was **not** observed for this plugin: after copying new QML the shell kept
running the instance it had parsed at load time, and only
`omarchy-plugin-disable` + `omarchy-plugin-enable` re-instantiated it (a
fresh `nirlock: FaceLane ready` line in `journalctl --user` is the proof).
Likely because this is a `keepLoaded: true` service rather than a bar widget.
`scripts/nirlock-install` does the off/on cycle for that reason. Worth
re-testing against future Omarchy versions before simplifying it.
