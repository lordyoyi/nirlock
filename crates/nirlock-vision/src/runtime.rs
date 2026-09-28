//! ONNX Runtime bootstrap and session loading (ADR-0015).
//!
//! The runtime is never linked: [`init`] `dlopen`s `libonnxruntime.so`
//! through `ort`'s `load-dynamic` feature. Resolution order for the library
//! path: explicit argument, then `ORT_DYLIB_PATH`, then
//! `/usr/lib/libonnxruntime.so` (the Arch `onnxruntime-cpu` package).
//!
//! Every entry point of this crate that touches `ort` goes through
//! [`ensure`] first, so a caller that forgets [`init`] gets the same
//! `Err(OrtLoad)` a failed `dlopen` produces instead of `ort`'s own fallback
//! (which panics and, on Linux, dlopens the bare soname through the loader's
//! search path, bypassing the pinning).
//!
//! Retry semantics (DESIGN §2.6: a failed load is `Cold`, retried on the
//! next `prewarm`): the library is **probed first** by this crate
//! (`dlopen`, `OrtGetApiBase`, version string) without involving `ort`; a
//! probe failure leaves the state `Cold` and the next call retries. Only a
//! path that passed the probe is handed to `ort::init_from`. That order
//! matters: in `ort` 2.0.0-rc.13 a failed `init_from` leaves its private
//! `OnceLock` marked complete but uninitialised (`Once::call_once_force`
//! completes even when the closure returned `Err`), after which a second
//! `init_from` reports success without loading anything and the next API
//! use is undefined behaviour. So an `ort`-level failure after a successful
//! probe is recorded as `Poisoned` and every later call returns the same
//! error without touching `ort` again; the daemon then needs a restart.
//!
//! `ort` is built with `api-27`; any runtime whose `OrtGetApiBase()->GetApi(27)`
//! is non-null works (ORT ≥ 1.27; 1.29.1 answers API version 29 and still
//! serves 27).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

use crate::{Error, Result};

/// Default location of the Arch package's shared library.
pub const DEFAULT_DYLIB: &str = "/usr/lib/libonnxruntime.so";

/// Loader state. A mutex (not a `OnceLock`) so that concurrent first callers
/// serialise on one probe + `dlopen` + `commit`, a probe failure leaves
/// `Cold` for a retry, and a later call with a *different* explicit path is
/// refused instead of ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    /// Nothing loaded; a call may try (again).
    Cold,
    /// `ort` loaded and committed this library.
    Ready(PathBuf),
    /// `ort::init_from` failed after a successful probe; `ort`'s loader
    /// cannot be used again in this process (see the module docs).
    Poisoned(String),
}

static STATE: Mutex<State> = Mutex::new(State::Cold);

fn state() -> std::sync::MutexGuard<'static, State> {
    // A poisoned lock only means another thread panicked while holding it;
    // the value inside is written last and stays consistent.
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Resolves which library file [`init`] will load.
pub fn dylib_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    match std::env::var_os("ORT_DYLIB_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(DEFAULT_DYLIB),
    }
}

/// Loads ONNX Runtime once per process and commits the global environment
/// (telemetry off). Repeated calls return the path used the first time; a
/// repeated call with a different explicit path is an error.
///
/// A failed `dlopen` is an ordinary error: the daemon maps it to the `Cold`
/// state and `unavailable models_unavailable` (DESIGN §2.6) and the next
/// call retries.
pub fn init(explicit: Option<&Path>) -> Result<PathBuf> {
    let mut st = state();
    match &*st {
        State::Ready(current) => {
            if let Some(wanted) = explicit
                && wanted != current.as_path()
            {
                return Err(Error::OrtLoad(format!(
                    "already initialised from {}; cannot switch to {}",
                    current.display(),
                    wanted.display()
                )));
            }
            return Ok(current.clone());
        }
        State::Poisoned(msg) => return Err(Error::OrtLoad(msg.clone())),
        State::Cold => {}
    }
    let path = dylib_path(explicit);
    // Probe without `ort`: a missing/foreign file fails here and stays Cold.
    let version = apibase::version_string(&path)?;
    let minor: u32 = version
        .split('.')
        .nth(1)
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if minor < ort::MINOR_VERSION {
        return Err(Error::OrtLoad(format!(
            "{}: ONNX Runtime {version} is older than the api-{} this build needs",
            path.display(),
            ort::MINOR_VERSION
        )));
    }
    match ort::init_from(&path) {
        Ok(builder) => {
            // `commit` returns false when an environment already exists
            // (it cannot, under this lock; not an error either way).
            let _ = builder.with_name("nirlock").with_telemetry(false).commit();
            *st = State::Ready(path.clone());
            Ok(path)
        }
        Err(e) => {
            let msg = format!(
                "{}: {e} (ort loader unusable until restart)",
                path.display()
            );
            *st = State::Poisoned(msg.clone());
            Err(Error::OrtLoad(msg))
        }
    }
}

/// The path [`init`] committed to, if it has succeeded.
pub fn initialised() -> Option<PathBuf> {
    match &*state() {
        State::Ready(p) => Some(p.clone()),
        _ => None,
    }
}

/// Makes sure the runtime is loaded (default resolution) before any `ort`
/// call. Every `ort` entry point in this crate calls this.
pub fn ensure() -> Result<PathBuf> {
    init(None)
}

/// What the loaded library says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// `OrtGetApiBase()->GetVersionString()`, e.g. `1.29.1`.
    pub version: String,
    /// `OrtApi::GetBuildInfoString()`, e.g. `ORT Build Info: git-branch=…`.
    pub build_info: String,
    /// The file that was `dlopen`ed.
    pub dylib: PathBuf,
}

/// Version and build information of the loaded runtime (`doctor`, `bench`).
/// Loads the runtime first when needed; a failed load is an `Err`, never a
/// panic.
pub fn version() -> Result<Version> {
    let dylib = ensure()?;
    let version = apibase::version_string(&dylib)?;
    Ok(Version {
        version,
        build_info: ort::info().to_string(),
        dylib,
    })
}

/// `OrtGetApiBase()->GetVersionString()`, also the pre-`ort` probe.
///
/// `ort` keeps its `dlopen` handle private and only exposes
/// `GetBuildInfoString`; the semver of the runtime lives in `OrtApiBase`,
/// so it is read here through our own `dlopen` of the file (before `ort`
/// has it, as the probe; afterwards the loader returns the handle `ort`
/// already holds, reference-counted). This is the one place in the crate
/// that needs `unsafe`, hence `#![deny(unsafe_code)]` rather than `forbid`
/// at the crate root.
#[allow(unsafe_code)]
mod apibase {
    use std::ffi::CStr;
    use std::path::Path;

    use crate::{Error, Result};

    type GetApiBase = unsafe extern "system" fn() -> *const ort::sys::OrtApiBase;

    pub(super) fn version_string(dylib: &Path) -> Result<String> {
        let err = |msg: String| Error::OrtLoad(format!("{}: {msg}", dylib.display()));
        // SAFETY: `dlopen` of the ONNX Runtime shared object. Its ELF
        // constructors have no preconditions a caller could violate, opening
        // it a second time returns the same mapping with its reference count
        // incremented, and when `ort` holds the library dropping `lib` never
        // unmaps it. When `ort` does not (the probe), unmapping on drop is
        // exactly what is wanted.
        let lib = unsafe { libloading::Library::new(dylib) }.map_err(|e| err(e.to_string()))?;
        // SAFETY: `OrtGetApiBase` is the documented C entry point of the
        // runtime, exported with exactly this signature (`OrtApiBase*
        // OrtGetApiBase(void)`, `onnxruntime_c_api.h`); the symbol name is
        // NUL-terminated.
        let get_base: libloading::Symbol<'_, GetApiBase> =
            unsafe { lib.get(b"OrtGetApiBase\0") }.map_err(|e| err(e.to_string()))?;
        // SAFETY: calling the resolved entry point with no arguments, as the
        // C API specifies; it returns a pointer to a static table (or null).
        let base = unsafe { get_base() };
        if base.is_null() {
            return Err(err("OrtGetApiBase() returned null".into()));
        }
        // SAFETY: `base` is non-null and points at the runtime's static
        // `OrtApiBase` table, which outlives the library mapping; the table
        // layout is the C ABI struct `ort_sys` mirrors. `GetVersionString`
        // returns a NUL-terminated static string that must not be freed;
        // it is copied into an owned `String` before `lib` is dropped.
        let version = unsafe {
            let f = (*base).GetVersionString;
            let p = f();
            if p.is_null() {
                return Err(err("GetVersionString() returned null".into()));
            }
            CStr::from_ptr(p).to_string_lossy().into_owned()
        };
        Ok(version)
    }
}

/// A loaded model plus the time it took to load, for `bench` and `doctor`.
pub struct Loaded {
    pub session: Session,
    pub load_ms: f64,
}

/// Session knobs beyond the thread count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    /// Intra-op threads (≥ 1).
    pub intra_threads: usize,
    /// Let the intra-op pool spin-wait between runs. Faster back-to-back
    /// inference, but the pool burns CPU while idle; ADR-0015 says no
    /// spinning for the daemon. `NIRLOCK_ORT_SPIN=1` flips it for experiments.
    pub spinning: bool,
}

impl SessionOptions {
    pub fn threads(intra_threads: usize) -> Self {
        let spinning = std::env::var("NIRLOCK_ORT_SPIN").is_ok_and(|v| v == "1");
        Self {
            intra_threads,
            spinning,
        }
    }
}

/// Loads an ONNX model with an explicit intra-op thread count and no
/// inter-op parallelism (sequential executor). Graph optimisation is left at
/// ORT's `Level3` (all); memory pattern on.
pub fn load_session(model: &Path, intra_threads: usize) -> Result<Loaded> {
    load_session_with(model, SessionOptions::threads(intra_threads))
}

/// [`load_session`] with explicit [`SessionOptions`]. Loads the runtime
/// first when the caller has not; a missing runtime is `Err(OrtLoad)`.
pub fn load_session_with(model: &Path, opts: SessionOptions) -> Result<Loaded> {
    ensure()?;
    let t0 = Instant::now();
    // Builder errors carry the builder back for recovery; we do not recover,
    // so flatten them to plain `ort::Error`s.
    fn b(e: ort::Error<ort::session::builder::SessionBuilder>) -> Error {
        Error::Model {
            model: "session options".into(),
            msg: e.to_string(),
        }
    }
    let session = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(b)?
        .with_intra_threads(opts.intra_threads.max(1))
        .map_err(b)?
        .with_intra_op_spinning(opts.spinning)
        .map_err(b)?
        .with_inter_op_spinning(false)
        .map_err(b)?
        .with_inter_threads(1)
        .map_err(b)?
        .with_parallel_execution(false)
        .map_err(b)?
        .with_memory_pattern(true)
        .map_err(b)?
        .with_log_id("nirlock")
        .map_err(b)?
        .commit_from_file(model)
        .map_err(|e| Error::Model {
            model: model.display().to_string(),
            msg: e.to_string(),
        })?;
    Ok(Loaded {
        session,
        load_ms: t0.elapsed().as_secs_f64() * 1e3,
    })
}

/// Names and static shapes of a session's inputs/outputs (-1 = dynamic).
pub fn describe(session: &Session) -> Vec<String> {
    let mut out = Vec::new();
    for i in session.inputs() {
        out.push(format!("input  {} {:?}", i.name(), i.dtype()));
    }
    for o in session.outputs() {
        out.push(format!("output {} {:?}", o.name(), o.dtype()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicit path that does not exist is an `Err`, not a panic, and
    /// leaves the state untouched for a retry.
    #[test]
    fn missing_library_is_an_error_and_retryable() {
        let bogus = Path::new("/nonexistent/nirlock-test/libonnxruntime.so");
        let r = init(Some(bogus));
        // Either the dlopen failed, or another test in this process already
        // initialised the runtime from a real library and the mismatch is
        // what must be refused. Never a panic, never cached.
        assert!(matches!(r, Err(Error::OrtLoad(_))), "{r:?}");
        assert_ne!(
            initialised().as_deref(),
            Some(bogus),
            "a failed init must not be cached"
        );
    }

    /// A session load on a bogus model without a prior `init` is an `Err`
    /// either way: `OrtLoad` when the default resolution finds no runtime,
    /// `Model` when it does and the file is not a model.
    #[test]
    fn load_session_without_init_does_not_panic() {
        let r = load_session(Path::new("/nonexistent/nirlock-test/model.onnx"), 1);
        assert!(
            matches!(r, Err(Error::OrtLoad(_)) | Err(Error::Model { .. })),
            "{}",
            r.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }

    /// `version()` before `init` is an `Err` or a real version, never a panic.
    #[test]
    fn version_without_init_does_not_panic() {
        match version() {
            Ok(v) => {
                assert!(v.version.starts_with("1."), "{v:?}");
                assert!(v.build_info.contains("ORT Build Info"), "{v:?}");
            }
            Err(Error::OrtLoad(_)) => {}
            Err(e) => panic!("unexpected {e}"),
        }
    }

    #[test]
    fn dylib_resolution_order() {
        assert_eq!(
            dylib_path(Some(Path::new("/x/lib.so"))),
            PathBuf::from("/x/lib.so")
        );
        let d = dylib_path(None);
        match std::env::var_os("ORT_DYLIB_PATH") {
            Some(v) if !v.is_empty() => assert_eq!(d, PathBuf::from(v)),
            _ => assert_eq!(d, PathBuf::from(DEFAULT_DYLIB)),
        }
    }
}
