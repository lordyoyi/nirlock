//! `nirlockctl probe` — report the cameras on this machine and, when one
//! qualifies, print the profile that would make it work.

use clap::Args;
use nirlock_cam::RealSys;
use nirlock_cam::ProfileSet;
use nirlock_cam::probe::{self as probe_dev, NodeKind, Verdict};

#[derive(Args, Debug)]
pub struct ProbeArgs {
    /// Print only the candidate profile (nothing else), for redirecting
    /// straight into a file.
    #[arg(long)]
    pub toml: bool,
}

pub fn run(a: &ProbeArgs) -> i32 {
    let mut sys = RealSys;
    let set = ProfileSet::load();
    let devices = probe_dev::probe_in(std::path::Path::new("/sys"), &set, &mut sys);

    // A profile file that does not parse is skipped. Saying so is the whole
    // reason the loader collects these: a profile someone wrote for their own
    // camera that silently never takes effect is the one failure they cannot
    // diagnose from outside.
    for problem in set.problems() {
        eprintln!("warning: ignoring {problem}");
    }

    if a.toml {
        // One profile per file. Concatenating two would produce a TOML
        // document with duplicate top-level keys, which the loader rejects —
        // and the documented usage is a redirection into a file, so it would
        // fail only later, at install time, far from the cause.
        let candidates: Vec<&probe_dev::DeviceReport> = devices
            .iter()
            .filter(|d| d.candidate.is_some())
            .collect();
        match candidates.as_slice() {
            [] => {
                eprintln!(
                    "no camera on this machine can be profiled; run `nirlockctl probe` for why"
                );
                return 1;
            }
            [one] => {
                print!("{}", one.candidate.as_deref().unwrap_or(""));
                return 0;
            }
            many => {
                eprintln!("more than one camera here can be profiled:");
                for d in many {
                    eprintln!("  {}", d.usb.label());
                }
                eprintln!("Run `nirlockctl probe` and copy the one you want.");
                return 1;
            }
        }
    }

    if devices.is_empty() {
        println!("No uvcvideo cameras found on this machine.");
        println!("If the laptop has one, it may be disabled in firmware or held by another driver.");
        return 1;
    }

    let mut usable = 0;
    for d in &devices {
        println!();
        println!("USB {} ({})", d.usb.label(), d.usb.removable);
        for n in &d.nodes {
            let what = match n.kind {
                NodeKind::Video if n.width > 0 => {
                    format!("{} {}x{}", n.format, n.width, n.height)
                }
                NodeKind::Video => format!("{} (geometry unknown)", n.format),
                NodeKind::Meta => format!("{} (metadata)", n.format),
                NodeKind::Other => "not a capture node".into(),
                NodeKind::Unknown => "could not be queried".into(),
            };
            let note = n
                .error
                .as_deref()
                .map(|e| format!("  [{e}]"))
                .unwrap_or_default();
            println!(
                "  {:<14} iface {} index {}  {}{}",
                n.dev.display(),
                n.iface,
                n.index,
                what,
                note
            );
        }
        match &d.verdict {
            Verdict::Supported { id, source } => {
                usable += 1;
                println!("  -> supported: profile '{id}' from {source}");
            }
            Verdict::Candidate => {
                usable += 1;
                println!("  -> usable, but no profile claims it yet.");
                println!("     Save the profile below and it will work:");
                println!();
                for line in d.candidate.as_deref().unwrap_or("").lines() {
                    println!("       {line}");
                }
                println!();
                println!(
                    "     sudo install -Dm644 /dev/stdin /etc/nirlock/hw/{}-{}.toml <<'EOF'",
                    d.usb.vendor, d.usb.product
                );
                println!("     ... paste the above, then EOF");
                println!();
                println!("     Then please send it as a pull request so the next person");
                println!("     with this camera does not have to work it out again.");
            }
            Verdict::Unusable(why) => {
                println!("  -> cannot be used: {why}");
            }
        }
    }

    println!();
    if usable == 0 {
        println!("No camera on this machine can do infrared face unlock.");
        return 1;
    }
    0
}
