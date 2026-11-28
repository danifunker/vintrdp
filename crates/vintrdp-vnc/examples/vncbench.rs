//! How fast a VNC server's updates reach us: frames per second, and the time
//! from a key press to the screen changing.
//!
//! `cargo run --release -p vintrdp-vnc --example vncbench -- HOST:PORT fps SECS`
//! counts updates for SECS seconds (have something animate on the server).
//! `... HOST:PORT latency N` types x and backspace in turn, N keys a second
//! or so apart, and times each until an update draws something (at a shell
//! prompt, say); the screen should otherwise be still.
//! `... HOST:PORT record SECS FILE` saves what the server sends for SECS
//! seconds, and `... - replay 0 FILE` decodes that as fast as it can, timing
//! the decoders alone. `VNC_PASSWORD` gives the password.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vintrdp_vnc::{Host, Settings};

struct Bench {
    start: Instant,
    fb: Mutex<(usize, usize, Vec<u32>)>,
    updates: AtomicU64,
    pixels: AtomicU64,
    /// Pixels drawn since the last `mark`, and when they passed the threshold
    /// (microseconds since `start`, 0 for not yet).
    since_mark: AtomicU64,
    hit: AtomicU64,
}

const THRESHOLD: u64 = 100;

impl Host for Bench {
    fn resize(&self, width: u16, height: u16) {
        *self.fb.lock().unwrap() = (width as usize, height as usize, vec![0; width as usize * height as usize]);
    }
    fn draw(&self, x: usize, y: usize, w: usize, h: usize, px: &[u32]) {
        let mut fb = self.fb.lock().unwrap();
        let (fw, fh) = (fb.0, fb.1);
        if x >= fw || y >= fh {
            return;
        }
        let cw = w.min(fw - x);
        for row in 0..h.min(fh - y) {
            fb.2[(y + row) * fw + x..][..cw].copy_from_slice(&px[row * w..][..cw]);
        }
        drop(fb);
        self.pixels.fetch_add((w * h) as u64, Ordering::Relaxed);
        let before = self.since_mark.fetch_add((w * h) as u64, Ordering::AcqRel);
        if before < THRESHOLD && before + (w * h) as u64 >= THRESHOLD {
            self.hit.store(self.start.elapsed().as_micros() as u64, Ordering::Release);
        }
    }
    fn copy(&self, _sx: usize, _sy: usize, _x: usize, _y: usize, w: usize, h: usize) {
        self.pixels.fetch_add((w * h) as u64, Ordering::Relaxed);
        let before = self.since_mark.fetch_add((w * h) as u64, Ordering::AcqRel);
        if before < THRESHOLD && before + (w * h) as u64 >= THRESHOLD {
            self.hit.store(self.start.elapsed().as_micros() as u64, Ordering::Release);
        }
    }
    fn updated(&self) {
        self.updates.fetch_add(1, Ordering::Relaxed);
    }
}

/// A local port that hands its connection to `serve`.
fn listen(serve: impl FnOnce(std::net::TcpStream) + Send + 'static) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener.accept().unwrap().0));
    port
}

fn main() {
    use std::io::{Read, Write};
    let args: Vec<String> = std::env::args().collect();
    let (addr, mode, n) = (&args[1], args[2].as_str(), args[3].parse::<u64>().unwrap());
    let port = match mode {
        "record" => {
            // A proxy in between, keeping what the server says.
            let (host, port) = addr.rsplit_once(':').unwrap();
            let server = vintrdp_vnc::connect(host, port.parse().unwrap()).unwrap();
            let mut file = std::fs::File::create(&args[4]).unwrap();
            listen(move |client| {
                let (mut up, mut down) = (server.try_clone().unwrap(), client.try_clone().unwrap());
                std::thread::spawn(move || std::io::copy(&mut down, &mut up));
                let (mut server, mut client) = (server, client);
                let mut buf = vec![0; 1 << 16];
                while let Ok(k @ 1..) = server.read(&mut buf) {
                    file.write_all(&buf[..k]).unwrap();
                    if client.write_all(&buf[..k]).is_err() {
                        break;
                    }
                }
            })
        }
        "replay" => {
            let data = std::fs::read(&args[4]).unwrap();
            let replay = listen(move |mut client| {
                let mut sink = client.try_clone().unwrap();
                std::thread::spawn(move || std::io::copy(&mut sink, &mut std::io::sink()));
                let _ = client.write_all(&data);
                let _ = client.shutdown(std::net::Shutdown::Write);
            });
            let (mut client, _sender) = vintrdp_vnc::client(vintrdp_vnc::connect("127.0.0.1", replay).unwrap());
            let bench = Bench {
                start: Instant::now(),
                fb: Mutex::new((0, 0, Vec::new())),
                updates: AtomicU64::new(0),
                pixels: AtomicU64::new(0),
                since_mark: AtomicU64::new(u64::MAX / 2),
                hit: AtomicU64::new(0),
            };
            client.handshake(&Settings { depth: 24, ..Default::default() }, &bench).unwrap();
            let t0 = Instant::now();
            let _ = client.run(&bench);
            let t = t0.elapsed().as_secs_f64();
            let (u, p) = (bench.updates.load(Ordering::Relaxed), bench.pixels.load(Ordering::Relaxed));
            println!("{u} updates, {:.1} Mpixels in {:.0} ms: {:.1} Mpixels/s, {:.2} ms per update", p as f64 / 1e6, t * 1000.0, p as f64 / t / 1e6, t * 1000.0 / u as f64);
            return;
        }
        _ => addr.rsplit_once(':').unwrap().1.parse().unwrap(),
    };
    let host = if mode == "record" { "127.0.0.1" } else { addr.rsplit_once(':').unwrap().0 };
    let tcp = vintrdp_vnc::connect(host, port).unwrap();
    let (mut client, sender) = vintrdp_vnc::client(tcp);
    let bench = Arc::new(Bench {
        start: Instant::now(),
        fb: Mutex::new((0, 0, Vec::new())),
        updates: AtomicU64::new(0),
        pixels: AtomicU64::new(0),
        since_mark: AtomicU64::new(u64::MAX / 2),
        hit: AtomicU64::new(0),
    });
    let settings = Settings { password: std::env::var("VNC_PASSWORD").unwrap_or_default(), depth: 24, shared: true, verbose: true, ..Default::default() };
    {
        let bench = bench.clone();
        std::thread::spawn(move || {
            client.handshake(&settings, &*bench).unwrap();
            let e = client.run(&*bench);
            eprintln!("ended: {e:?}");
            std::process::exit(1);
        });
    }
    std::thread::sleep(Duration::from_secs(2));
    match mode {
        "record" => std::thread::sleep(Duration::from_secs(n)),
        "fps" => {
            let (u0, p0, t0) = (bench.updates.load(Ordering::Relaxed), bench.pixels.load(Ordering::Relaxed), Instant::now());
            std::thread::sleep(Duration::from_secs(n));
            let (u, p, t) = (bench.updates.load(Ordering::Relaxed) - u0, bench.pixels.load(Ordering::Relaxed) - p0, t0.elapsed().as_secs_f64());
            println!("{:.1} updates/s, {:.2} Mpixels/s over {t:.1} s", u as f64 / t, p as f64 / t / 1e6);
        }
        "latency" => {
            // The pointer somewhere, as when it's over the window.
            let mut b = sender.batch();
            b.pointer(0, 0, 0);
            sender.send(b).unwrap();
            let mut times = Vec::new();
            let mut seed = 12345u64;
            for i in 0..n {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                std::thread::sleep(Duration::from_millis(800 + (seed >> 33) % 400));
                // An x, then a backspace: the screen changes each time.
                let (sym, sc) = if i % 2 == 0 { (0x78, 0x2d) } else { (0xff08, 0x0e) };
                let mut b = sender.batch();
                b.key(sym, Some((sc, false)), true);
                b.key(sym, Some((sc, false)), false);
                bench.hit.store(0, Ordering::Release);
                bench.since_mark.store(0, Ordering::Release);
                let sent = bench.start.elapsed().as_micros() as u64;
                sender.send(b).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while bench.hit.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_micros(200));
                }
                let hit = bench.hit.load(Ordering::Acquire);
                if hit == 0 {
                    println!("no response");
                } else {
                    times.push((hit - sent) as f64 / 1000.0);
                }
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mean = times.iter().sum::<f64>() / times.len().max(1) as f64;
            let at = |q: f64| times.get(((times.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
            println!("key to screen: mean {mean:.1} ms, min {:.1}, median {:.1}, p90 {:.1}, max {:.1} ({} samples)", at(0.0), at(0.5), at(0.9), at(1.0), times.len());
        }
        _ => panic!("fps or latency"),
    }
}
