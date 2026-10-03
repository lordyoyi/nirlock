//! `nirlockd`: the face-unlock daemon (DESIGN §2).
//!
//! M3 in progress: the request engine is wired and measurable through
//! `nirlockd bench`; the socket server and the policy engine follow.

// One `getsockopt` for SO_PEERCRED lives in `sys`; nothing else here is
// unsafe, and the workspace lints require a SAFETY comment on that block.
#![deny(unsafe_code)]

mod engine;
mod policy;
mod server;
mod sys;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use nirlock_vision::embed::Kind;
use nirlock_wire::PROTO;

/// nirlock daemon: opens the IR camera, runs YuNet + AuraFace, answers
/// `pam_nirlock.so` over `/run/nirlock/sock`.
#[derive(Parser, Debug)]
#[command(name = "nirlockd", version, about, long_about = None)]
struct Cli {
    /// Configuration file.
    #[arg(long, default_value = "/etc/nirlock/config.toml")]
    config: PathBuf,
    /// Print the wire protocol version and exit.
    #[arg(long)]
    proto: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Time the request engine end to end, one cold camera per trial.
    Bench(Bench),
    /// Serve the IPC socket (what the unit starts).
    Serve(Serve),
}

#[derive(Parser, Debug)]
struct Serve {
    /// Socket to bind. The packaged unit uses the default; a test run can
    /// point this anywhere the user can write.
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long, default_value = "/usr/share/nirlock/models")]
    models: PathBuf,
    /// Directory of enrolments, one sub-directory per user name.
    #[arg(long, default_value = "/var/lib/nirlock/templates")]
    templates: PathBuf,
    #[arg(long, default_value = "auraface")]
    embedder: String,
    #[arg(long, default_value_t = 4)]
    threads: usize,
    /// Start the RGB exposure lever with every request.
    #[arg(long)]
    rgb: bool,
    /// Where the consecutive-failure counter is persisted.
    /// A leading `%h` is expanded against `$HOME`, for running the daemon
    /// as yourself during development.
    #[arg(long, default_value = nirlock_wire::DEFAULT_STATE_PATH)]
    state: PathBuf,
    /// Consecutive recognition failures before the face is switched off
    /// until a password, a reboot or `reset-lockout`.
    #[arg(long, default_value_t = 10)]
    max_failures: u32,
}

#[derive(Parser, Debug)]
struct Bench {
    #[arg(long, default_value_t = 8)]
    trials: u32,
    /// Directory holding the ONNX models.
    #[arg(long, default_value = "/usr/share/nirlock/models")]
    models: PathBuf,
    /// Directory of enrolments, one sub-directory per user name.
    #[arg(long, default_value = "/var/lib/nirlock/templates")]
    templates: PathBuf,
    /// User whose enrolment to time.
    #[arg(long)]
    user: Option<String>,
    #[arg(long, default_value = "auraface")]
    embedder: String,
    #[arg(long, default_value_t = 4)]
    threads: usize,
    /// Per-request budget; everything, camera start included, fits inside.
    #[arg(long, default_value_t = 4000)]
    budget_ms: u32,
    /// Start the RGB exposure lever with every request.
    #[arg(long)]
    rgb: bool,
    /// Rest after the USB device reports `suspended`, so each trial starts
    /// from the cold path an unlock really takes.
    #[arg(long, default_value_t = 7)]
    gap_seconds: u64,
}

fn main() {
    let cli = Cli::parse();
    if cli.proto {
        println!("{}", nirlock_wire::PROTO);
        return;
    }
    match cli.cmd {
        Some(Cmd::Bench(b)) => bench(&b),
        Some(Cmd::Serve(s)) => serve(&s),
        None => println!(
            "nirlockd {} (proto {})",
            env!("CARGO_PKG_VERSION"),
            nirlock_wire::PROTO
        ),
    }
}

fn embedder_kind(name: &str) -> Kind {
    match name {
        "auraface" => Kind::AuraFace,
        "sface" => Kind::SFace,
        other => {
            eprintln!("unknown embedder {other}");
            std::process::exit(2)
        }
    }
}

/// Exit status for "this machine cannot run nirlock as installed": no
/// hardware profile claims any camera, or ONNX Runtime is missing. Retrying
/// fixes neither, so `nirlockd.service` lists it in `RestartPreventExitStatus`
/// instead of restarting every two seconds forever (EX_CONFIG, sysexits.h).
const EXIT_UNSUPPORTED: i32 = 78;

fn is_unsupported(e: &engine::Error) -> bool {
    matches!(
        e,
        engine::Error::Cam(nirlock_cam::Error::NoProfile { .. })
            | engine::Error::Vision(nirlock_vision::Error::OrtLoad(_))
    )
}

fn serve(s: &Serve) {
    // Models first: the point of a resident daemon is that a `verify`
    // never pays the ~940 ms AuraFace load (ADR-0004).
    let eng = match engine::Engine::load(
        &s.models,
        &s.templates,
        embedder_kind(&s.embedder),
        s.threads,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("nirlockd: {e}");
            let code = if is_unsupported(&e) {
                EXIT_UNSUPPORTED
            } else {
                1
            };
            std::process::exit(code)
        }
    };
    let state = if s.state.starts_with("%h") {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/var/lib".into()))
            .join(s.state.strip_prefix("%h").unwrap_or(&s.state))
    } else {
        s.state.clone()
    };
    let policy = policy::Policy::load(
        &state,
        policy::Limits {
            max_consecutive_failures: s.max_failures,
        },
    );
    if policy.allow() == policy::Allow::LockedOut {
        eprintln!(
            "nirlockd: face is switched off ({} consecutive failures); a password, a reboot or `nirlockctl reset-lockout` brings it back",
            policy.failures()
        );
    }
    let path = s.socket.clone().unwrap_or_else(server::default_socket);
    let srv = match server::Server::bind(&path, eng, policy, &s.templates, s.rgb) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("nirlockd: bind {}: {e}", path.display());
            std::process::exit(1)
        }
    };
    println!("nirlockd listening on {} (proto {PROTO})", path.display());
    srv.serve();
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return f64::NAN;
    }
    v[v.len() / 2]
}

fn bench(b: &Bench) {
    let kind = embedder_kind(&b.embedder);
    let ac = std::fs::read_to_string("/sys/class/power_supply/AC0/online")
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
    let load = Instant::now();
    let user = b
        .user
        .clone()
        .unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
    let mut eng = match engine::Engine::load(&b.models, &b.templates, kind, b.threads) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("nirlockd bench: {e}");
            std::process::exit(1)
        }
    };
    let load_ms = load.elapsed().as_secs_f64() * 1e3;
    let (ir, meta, rgb) = eng.camera_paths();
    println!(
        "camera    {} + {} (rgb {})",
        ir.display(),
        meta.display(),
        rgb.display()
    );
    println!(
        "models    {} loaded in {load_ms:.0} ms (threads {}, warm for every trial)",
        b.embedder, b.threads
    );
    println!(
        "power     {} | threshold {:.2} | K={} of W={}",
        if ac { "AC" } else { "battery" },
        eng.decision.threshold,
        eng.decision.k,
        eng.decision.window
    );
    println!(
        "\n{:>5} {:>8} {:>9} {:>9} {:>9} {:>9} {:>8}  outcome",
        "trial", "streamon", "frame1", "gate1", "embed1", "K2", "score"
    );

    let (mut k2, mut gate1, mut embed1, mut first) = (vec![], vec![], vec![], vec![]);
    let mut accepted = 0u32;
    for i in 0..b.trials {
        std::thread::sleep(Duration::from_secs(b.gap_seconds));
        match eng.verify(&user, b.budget_ms, b.rgb) {
            Ok(v) => {
                println!(
                    "{i:>5} {:>8.0} {:>9.0} {:>9.0} {:>9.0} {:>9.0} {:>8.3}  {} ({} lit, {} scored{})",
                    v.streamon_ms,
                    v.first_frame_ms,
                    v.first_gate_pass_ms,
                    v.first_embedding_ms,
                    v.decision_ms,
                    v.best_score,
                    v.decision.reason(),
                    v.lit_frames,
                    v.scored_frames,
                    if v.rejects.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; {}",
                            v.rejects
                                .iter()
                                .map(|(k, n)| format!("{k}={n}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    }
                );
                if v.decision == engine::Decision::Accept {
                    accepted += 1;
                    k2.push(v.decision_ms);
                }
                if !v.first_frame_ms.is_nan() {
                    first.push(v.first_frame_ms);
                }
                if !v.first_gate_pass_ms.is_nan() {
                    gate1.push(v.first_gate_pass_ms);
                }
                if !v.first_embedding_ms.is_nan() {
                    embed1.push(v.first_embedding_ms);
                }
            }
            Err(e) => println!("{i:>5} error: {e}"),
        }
    }
    println!("\naccepted  {accepted}/{}", b.trials);
    for (name, v) in [
        ("first frame", &mut first),
        ("first gate pass", &mut gate1),
        ("first embedding", &mut embed1),
        ("decision K=2", &mut k2),
    ] {
        if v.is_empty() {
            println!("{name:<17} -");
            continue;
        }
        let m = median(v);
        println!(
            "{name:<17} median {m:>7.0} ms   min {:>6.0}   max {:>6.0}   n={}",
            v[0],
            v[v.len() - 1],
            v.len()
        );
    }
}
