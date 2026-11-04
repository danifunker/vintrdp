//! Connect without a window: log events, and write the framebuffer to a PPM
//! file when the session ends or after a timeout.
//!
//!   cargo run -p vintrdp-core --example headless -- [--secs N] [--out fb.ppm] <rdesktop args>

use std::sync::Arc;
use std::time::Duration;

use vintrdp_core::{Config, Event, Frontend, Session};

struct Log;

impl Frontend for Log {
    fn notify(&self, event: Event) {
        match event {
            Event::Updated | Event::Cursor => {}
            e => eprintln!("event: {e:?}"),
        }
    }
    fn screen_size(&self) -> (u32, u32) {
        (1920, 1080)
    }
    fn ask(&self, title: &str, message: &str, accept: &str, _reject: &str) -> bool {
        eprintln!("ask: {title}: {message}\n  -> answering {accept}");
        true
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str, default: &str| match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            args.remove(i)
        }
        None => default.to_string(),
    };
    let secs: u64 = take("--secs", "15").parse().expect("--secs N");
    let out = take("--out", "fb.ppm");

    let config = Config::from_args(args).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let session = Session::start(config, Arc::new(Log)).expect("start");
    let outcome = session.wait(Duration::from_secs(secs));
    session
        .with_framebuffer(|w, h, px| {
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            ppm.extend(px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
            std::fs::write(&out, ppm).expect("write framebuffer");
            eprintln!("framebuffer {w}x{h} written to {out}");
        })
        .unwrap_or_else(|| eprintln!("no framebuffer"));
    match outcome {
        Some(o) => eprintln!("ended: {o:?}"),
        None => {
            session.quit();
            eprintln!("still running after {secs}s; quit -> {:?}", session.wait(Duration::from_secs(5)));
        }
    }
}
