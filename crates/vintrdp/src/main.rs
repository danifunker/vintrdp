//! vintrdp: an RDP 4/5 client. rdesktop's protocol core (vintrdp-core) with
//! a native front-end per OS: AppKit on macOS, Win32 on Windows, GTK 4 on
//! Linux and the BSDs.
//!
//! With arguments, connects straight away (rdesktop-compatible options);
//! without, shows a native connection dialog. `--control` runs a session with
//! no window, driven by text commands (see control.rs).

// A plain double-click must not open a console. `--version` and friends
// attach to the one they were started from instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod control;
mod frontend;
mod launch;

use std::path::Path;

use vintrdp_core::Config;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if !args.is_empty() {
        frontend::attach_console();
    }
    // No window: a session driven by text commands (control.rs).
    match args.first().map(String::as_str) {
        Some("--control") => std::process::exit(control::run(args[1..].to_vec())),
        Some("--send") => std::process::exit(control::send(args[1..].to_vec())),
        _ => {}
    }
    if args.iter().any(|a| a == "--version") {
        println!("vintrdp {}", launch::VERSION);
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
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    };
    std::process::exit(frontend::run(config));
}
