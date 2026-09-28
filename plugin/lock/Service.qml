// nirlock.lock — wrapper service (DESIGN §5.2, ADR-0011).
//
// Loader A loads the stock Omarchy lock Service.qml unchanged (password and
// fingerprint lanes, WlSessionLock, LockView, IpcHandler "lock"). Loader B
// adds FaceLane.qml on top. An error in FaceLane only removes the face lane;
// an error in the stock service disables this plugin so the registry
// restores omarchy.lock.
import QtQuick
import Quickshell
import Quickshell.Io

Item {
  id: root
  property var shell: null                 // injected by the host after createObject (scoped facade)
  property string omarchyPath: ""          // injected by the host
  // Resolved at declaration, as shell.qml does: never changes after load.
  readonly property string stockUrl: "file://" + (Quickshell.env("OMARCHY_PATH") || "/usr/share/omarchy") + "/shell/plugins/lock/Service.qml"

  Loader {
    id: stock
    source: root.stockUrl
    asynchronous: false
    onStatusChanged: if (status === Loader.Error) { console.warn("nirlock: stock lock failed to load"); selfDisable.running = true }
  }
  Binding { target: stock.item; property: "shell"; value: root.shell; when: stock.status === Loader.Ready && stock.item && ("shell" in stock.item) }
  Binding { target: stock.item; property: "omarchyPath"; value: root.omarchyPath; when: stock.status === Loader.Ready && stock.item && ("omarchyPath" in stock.item) }

  Loader {
    id: lane
    source: "FaceLane.qml"
    active: stock.status === Loader.Ready
    onStatusChanged: if (status === Loader.Error) console.warn("nirlock: FaceLane failed to load; stock lock unaffected")
    onLoaded: item.stock = stock.item
  }

  // Self-healing: if the stock lock does not load, hand omarchy.lock back (restoreCloneSource) without waiting for the doctor.
  Process { id: selfDisable; command: ["omarchy-shell", "shell", "setPluginEnabled", "nirlock.lock", "false"] }
}
