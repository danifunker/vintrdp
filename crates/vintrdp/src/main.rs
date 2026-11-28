//! vintrdp: an RDP 4/5 client. rdesktop's protocol core (vintrdp-core) with
//! a native front-end per OS: AppKit on macOS, Win32 on Windows, GTK 4 on
//! Linux and the BSDs.
//!
//! With arguments, connects straight away (rdesktop-compatible options);
//! without, shows a native connection dialog. `--control` runs a session with
//! no window, driven by text commands (see control.rs).

// A plain double-click must not open a console. `--version` and friends
// attach to the one they were started from instead, or log to a file where
// that isn't possible (win32::set_up_output).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod control;
mod frontend;
mod launch;
#[cfg(windows)]
mod win32;

use std::path::Path;

use vintrdp_core::Config;

const LICENSE: &str = include_str!("../../../LICENSE");
const NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.txt");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    win32::set_up_output(!args.is_empty());
    // No window: a session driven by text commands (control.rs).
    match args.first().map(String::as_str) {
        Some("--control") => std::process::exit(control::run(args[1..].to_vec())),
        Some("--send") => std::process::exit(control::send(args[1..].to_vec())),
        _ => {}
    }
    if args.iter().any(|a| a == "--help") {
        print!("{}", vintrdp_core::USAGE);
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("vintrdp {}", launch::VERSION);
        return;
    }
    // vintrdp's licence and the notices of what its X server bundles, for
    // packages with nowhere to put a file beside the program (NT 4.0's bare
    // .exe, the AppImage).
    if args.iter().any(|a| a == "--licenses" || a == "--licences") {
        print!("{}\n\n{}", LICENSE, NOTICES);
        return;
    }
    // Import a licence file into the global store, then stop.
    if let Some(i) = args.iter().position(|a| a == "--import-licence") {
        match args.get(i + 1) {
            Some(path) => match launch::import_licence(Path::new(path)) {
                Ok(()) => println!("Imported the licence to {}", launch::imported_licence().display()),
                Err(e) => {
                    eprintln!("could not import the licence: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                eprintln!("--import-licence needs a file path");
                std::process::exit(2);
            }
        }
        return;
    }
    let config = if args.is_empty() {
        None
    } else {
        match Config::from_args(args) {
            Ok(mut c) => {
                c.transfer = c.clipboard.then(launch::transfer_dir);
                c.licence = Some(launch::imported_licence());
                Some(c)
            }
            // -h or -?: the usage is what was asked for, not an error.
            Err(e) if e == vintrdp_core::USAGE => {
                print!("{e}");
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    };
    std::process::exit(frontend::run(config));
}

#[cfg(test)]
mod notices {
    /// THIRD-PARTY-NOTICES.txt is what scripts/x11/notices.sh makes of the
    /// X server's notices: run it after changing them.
    #[test]
    fn notices_are_current() {
        let x11 = include_str!("../../vintrdp-x11/data/COPYING");
        let fonts = include_str!("../../vintrdp-x11/data/fonts/COPYING");
        let mi = include_str!("../../vintrdp-x11/src/mi/COPYING");
        let pixman = include_str!("../../vintrdp-x11/src/pixman/COPYING");
        assert!([x11, fonts, mi, pixman].iter().all(|n| super::NOTICES.contains(n)), "run scripts/x11/notices.sh");
    }
}
