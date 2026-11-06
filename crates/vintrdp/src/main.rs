//! vintrdp: an RDP 4/5 client. rdesktop's protocol core (vintrdp-core) with
//! a native front-end per OS: AppKit on macOS, Win32 on Windows, GTK 4 on
//! Linux and the BSDs.
//!
//! With arguments, connects straight away (rdesktop-compatible options);
//! without, shows a native connection dialog.

// A plain double-click must not open a console. `--version` and friends
// attach to the one they were started from instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod frontend;
mod launch;

use vintrdp_core::Config;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if !args.is_empty() {
        frontend::attach_console();
    }
    if args.iter().any(|a| a == "--version") {
        println!("vintrdp {}", launch::VERSION);
        return;
    }
    let config = if args.is_empty() {
        None
    } else {
        match Config::from_args(args) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
    };
    std::process::exit(frontend::run(config));
}
