// nirlock.lock — the face lane (DESIGN §5.3).
//
// A third PamContext beside the stock password and fingerprint lanes. It
// never replaces them: whichever lane reaches PamResult.Success first calls
// the stock finishUnlock(), and the password field stays usable throughout.
// If this file fails to load, or the daemon is absent, the lock screen is
// exactly the stock one.
import QtQuick
import Quickshell.Services.Pam
import Quickshell.Wayland

Item {
  id: lane

  // The stock lock Service instance, injected by the wrapper's Loader.
  property var stock: null

  // Quickshell only ever calls pam_start_confdir(), with /etc/pam.d by
  // default (verified in the installed binary). Pointing configDirectory at
  // our own directory is what makes the lane ours: no file under /etc/pam.d
  // can shadow it, and omarchy's weekly rewrite of its own lock PAM files
  // cannot touch it either (ADR-0012).
  property string pamConfigDirectory: "/usr/lib/nirlock/pam.d"
  property string pamConfig: "nirlock-lock"

  // Attempts per ARRIVAL, not per lock session. Counting per session was
  // wrong and it showed on the first idle lock: the lid tests had spent the
  // five attempts while nobody was in front of the camera, the screen then
  // stayed locked (an idle lock on an already-locked screen is a no-op, so
  // no new session began), and when the user came back the budget was gone
  // and the camera never even tried. Attempts burned with nobody there must
  // not be charged to the person who returns — the same principle the daemon
  // already applies by not counting `no_face` as a failure.
  //
  // The real bound on a physical attacker is the daemon's persistent
  // counter of failures that actually saw a face; this number is only
  // politeness towards the camera within one arrival.
  property int maxAttempts: 5
  // Long enough to let the keystroke that locked the screen pass, short
  // enough that locking and staying does not feel like waiting. It was
  // 3000 ms and the first live unlock felt slow for exactly that reason:
  // recognition took 652 ms and the grace took the rest.
  property int graceMs: 1000
  // Seconds of no input after which the user is considered away. Coming
  // back out of that state is the trigger that matters.
  property int idleSeconds: 2
  // A wall-clock jump larger than this while locked means the machine was
  // suspended and has just come back — almost always the lid being opened.
  property int resumeGapMs: 3000

  readonly property bool locked: stock ? stock.lockRequested : false
  readonly property bool busy: facePam.active
  property int attempts: 0
  property bool armed: false
  // An activity edge that arrives while a check is already running would
  // otherwise be lost, and the user would have to move again to get a
  // second try. Remember it and honour it when the check ends.
  property bool pendingTrigger: false
  /// What the overlay is showing: idle | scanning | ok | fail.
  property string visualState: "idle"

  onLockedChanged: {
    attempts = 0
    pendingTrigger = false
    grace.stop()
    if (locked) {
      // Not on the lock transition itself: the keystroke that locked the
      // screen is still in flight and the user may be walking away.
      grace.restart()
    } else {
      armed = false
      visualState = "idle"
      settle.stop()
      if (facePam.active) facePam.abort()
    }
  }

  Timer {
    id: grace
    interval: lane.graceMs
    // One speculative attempt for the common case of locking and staying
    // (a quick lock, a colleague walking past). After that the lane goes
    // quiet: retrying on a timer would keep the emitter on for tens of
    // seconds while nobody is there.
    onTriggered: { lane.armed = true; lane.tryFace() }
  }

  // Closing the lid suspends this machine (logind's default), so "open the
  // lid and you are in" is really "resume and you are in". Timers do not
  // run while suspended, so a wall-clock jump is a reliable, self-contained
  // way to notice it — no D-Bus, no privileged helper. Measured on
  // 2026-09-27: after an s2idle resume the first IR frame arrives at 145 ms
  // instead of the usual 253, because the USB device is already awake.
  Timer {
    id: resumeWatch
    running: lane.locked
    interval: 1000
    repeat: true
    property double last: 0
    onRunningChanged: last = Date.now()
    onTriggered: {
      var now = Date.now()
      var gap = now - last
      last = now
      if (gap > lane.resumeGapMs && lane.armed) {
        // A fresh approach to the machine deserves a fresh budget; the
        // daemon's persistent counter still bounds the total.
        lane.attempts = 0
        lane.tryFace()
      }
    }
  }

  // The other trigger: the user coming back without a suspend in between.
  // An idle→active edge is a keypress or the mouse moving, which is exactly
  // the moment they are in front of the screen again.
  IdleMonitor {
    id: idle
    enabled: lane.locked
    timeout: lane.idleSeconds
    respectInhibitors: false
    onIsIdleChanged: {
      if (isIdle || !lane.armed) return
      // Somebody arrived: a fresh budget for this arrival.
      lane.attempts = 0
      lane.tryFace()
    }
  }

  function tryFace() {
    if (!locked || !armed) return
    if (facePam.active) { pendingTrigger = true; return }
    // The password lane wins ties: if a check is already in flight, do not
    // compete for it. No retry timer here — the next activity edge will
    // call us again.
    if (stock && stock.authenticatingPassword) return
    if (attempts >= maxAttempts) {
      // Say it once, through the only channel the stock lock gives us.
      if (stock && attempts === maxAttempts) {
        attempts += 1
        stock.failureMessage = "Face unlock paused — move the mouse to retry, or use your password"
      }
      return
    }
    pendingTrigger = false
    attempts += 1
    settle.stop()
    visualState = "scanning"
    if (!facePam.start()) visualState = "idle"
  }

  PamContext {
    id: facePam
    config: lane.pamConfig
    configDirectory: lane.pamConfigDirectory
    user: lane.stock ? lane.stock.userName : ""

    onCompleted: function (result) {
      if (!lane.locked) return
      if (result === PamResult.Success) {
        lane.visualState = "ok"
        settle.restart()
        lane.stock.finishUnlock()
        return
      }
      lane.visualState = "fail"
      settle.restart()
      // Every non-success is the same to us: the daemon already decided and
      // the lane ends in pam_deny, so nothing here can open the lock. We do
      // not retry on a timer; the next idle→active edge will — or the one
      // that arrived while this check was running.
      if (lane.pendingTrigger) Qt.callLater(lane.tryFace)
    }

    onError: function (error) {
      // A missing daemon, a missing lane file or a module error all land
      // here. The password lane is untouched either way. No "fail" shown:
      // nothing was compared, so saying "not recognised" would be a lie.
      lane.visualState = "idle"
      settle.stop()
      if (lane.pendingTrigger) Qt.callLater(lane.tryFace)
    }
  }

  // The scan indicator. Purely decorative and focus-free: everything that
  // decides anything lives in the daemon, and the password field keeps the
  // keyboard at all times.
  ScanOverlay {
    id: overlay
    state: lane.visualState
  }

  // Holds "ok"/"fail" on screen briefly, then goes quiet. Without it the
  // result would vanish in the same frame the lock does, and a failure
  // would never be seen at all.
  Timer {
    id: settle
    interval: 1400
    onTriggered: lane.visualState = "idle"
  }

  Component.onCompleted: console.log("nirlock: FaceLane ready (" + pamConfigDirectory + "/" + pamConfig + ")")
}
