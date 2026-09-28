// nirlock — the scan indicator drawn over the lock screen.
//
// Why a layer-shell window instead of drawing inside the lock: the stock
// lock owns its `WlSessionLockSurface` and exposes it as a Component, not
// as an instance, so there is nothing to parent onto. The alternative was
// forking the whole lock UI, which would cut this plugin off from every
// upstream fix. Hyprland's `above_lock` layer rule lets a normal layer
// surface sit over a session lock, so the wrapper stays a wrapper.
//
// It never takes keyboard focus: typing must keep reaching the password
// field underneath at all times. The overlay is decoration and nothing else.
//
// Colours come from Omarchy's own `Color` singleton, so it follows whatever
// theme is active without knowing anything about themes.
//
// Honest caveat: `above_lock` matches by namespace, so any process that
// picks the same namespace gets the same privilege to draw over the lock.
// That is a property of Hyprland's rule matching, not something this plugin
// can tighten.
//
// The first version had a sliced 80s sun over the grid. On screen it was
// noise: the disc reached the password box, and the status word sat on top
// of it in the same neon and could not be read. What survives is the part
// that carries the meaning — a horizon, a floor receding towards it, a line
// sweeping down, and one word.
import QtQuick
import Quickshell
import Quickshell.Wayland
import qs.Commons

Variants {
  id: root

  // "idle" | "scanning" | "ok" | "fail"
  property string state: "idle"

  model: Quickshell.screens

  PanelWindow {
    id: win
    required property var modelData
    screen: modelData

    visible: root.state !== "idle"
    color: "transparent"
    anchors { top: true; bottom: true; left: true; right: true }
    exclusionMode: ExclusionMode.Ignore
    WlrLayershell.namespace: "nirlock-scan"
    WlrLayershell.layer: WlrLayer.Overlay
    // Never steal the keyboard: the password field lives underneath.
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

    readonly property color neon: root.state === "fail" ? Color.urgent : Color.accent

    // ---- the floor -------------------------------------------------------
    // Low enough that the horizon never climbs towards the password field.
    Item {
      id: band
      anchors { left: parent.left; right: parent.right; bottom: parent.bottom }
      height: Math.round(parent.height * 0.26)
      opacity: root.state === "idle" ? 0 : 1
      Behavior on opacity { NumberAnimation { duration: 220; easing.type: Easing.OutCubic } }

      // Darkens towards the bottom, so the grid has something to sit on and
      // the word below stays readable over any wallpaper.
      Rectangle {
        anchors.fill: parent
        gradient: Gradient {
          GradientStop { position: 0.0; color: "transparent" }
          GradientStop { position: 1.0; color: Qt.rgba(Color.background.r, Color.background.g, Color.background.b, 0.80) }
        }
      }

      // The receding grid. `phase` is what makes the lines travel towards
      // the viewer while a check is running.
      Canvas {
        id: grid
        anchors.fill: parent
        property real phase: 0
        onPhaseChanged: requestPaint()
        onPaint: {
          var ctx = getContext("2d")
          ctx.reset()
          var w = width, h = height
          ctx.lineWidth = 1
          ctx.strokeStyle = Qt.rgba(win.neon.r, win.neon.g, win.neon.b, 0.42)

          var vpx = w / 2, vpy = 0
          for (var i = -9; i <= 9; i++) {
            ctx.beginPath()
            ctx.moveTo(vpx, vpy)
            ctx.lineTo(w / 2 + i * (w / 9), h)
            ctx.stroke()
          }
          for (var k = 0; k < 10; k++) {
            var t = (k + phase) / 10
            var y = h * t * t
            ctx.globalAlpha = Math.min(1, t * 2)
            ctx.beginPath()
            ctx.moveTo(0, y)
            ctx.lineTo(w, y)
            ctx.stroke()
          }
          ctx.globalAlpha = 1
        }
      }

      // The horizon itself, the one bright line in the whole thing.
      Rectangle {
        anchors { left: parent.left; right: parent.right; top: parent.top }
        height: 2
        // A single stop at the centre faded away long before it read as a
        // line. It needs a plateau, not a peak.
        gradient: Gradient {
          orientation: Gradient.Horizontal
          GradientStop { position: 0.0; color: "transparent" }
          GradientStop { position: 0.20; color: win.neon }
          GradientStop { position: 0.80; color: win.neon }
          GradientStop { position: 1.0; color: "transparent" }
        }
      }
    }

    // ---- the sweep, only while actually scanning -------------------------
    Rectangle {
      id: scanline
      visible: root.state === "scanning"
      anchors { left: parent.left; right: parent.right }
      height: 3
      y: 0
      opacity: 0.95
      gradient: Gradient {
        orientation: Gradient.Horizontal
        GradientStop { position: 0.0; color: "transparent" }
        GradientStop { position: 0.5; color: win.neon }
        GradientStop { position: 1.0; color: "transparent" }
      }
    }

    // ---- the word --------------------------------------------------------
    Text {
      anchors { horizontalCenter: parent.horizontalCenter; bottom: parent.bottom }
      anchors.bottomMargin: Math.round(band.height * 0.28)
      visible: root.state !== "idle"
      text: root.state === "scanning" ? "SCANNING"
          : root.state === "ok" ? "RECOGNISED"
          : root.state === "fail" ? "NOT RECOGNISED" : ""
      // `Style.font` does not exist: the singleton exposes fontFamily and
      // fontPx(). Reaching for Style.font.heading threw, and the word came
      // out at the default size.
      color: win.neon
      font.family: Style.fontFamily
      font.pixelSize: Style.fontPx(1.8)
      font.letterSpacing: 8
      font.bold: true
      opacity: root.state === "scanning" ? pulse.value : 1
    }

    // Animations live in one place so the states cannot fight each other.
    QtObject {
      id: pulse
      property real value: 1
    }
    NumberAnimation {
      target: pulse; property: "value"
      running: root.state === "scanning"
      from: 0.45; to: 1; duration: 900
      easing.type: Easing.InOutSine
      loops: Animation.Infinite
      onStopped: pulse.value = 1
    }
    NumberAnimation {
      target: grid; property: "phase"
      running: root.state === "scanning"
      from: 0; to: 1; duration: 1800
      loops: Animation.Infinite
    }
    NumberAnimation {
      target: scanline; property: "y"
      running: root.state === "scanning"
      from: Math.round(win.height * 0.20); to: Math.round(win.height * 0.74)
      duration: 1500
      easing.type: Easing.InOutQuad
      loops: Animation.Infinite
    }
  }
}
