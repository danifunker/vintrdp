//! Stamps the version, and on Windows compiles the icon, the manifest and the
//! version resource into the executable.
//!
//! The version is VINTRDP_VERSION, which every release build sets
//! (scripts/dist/version.sh): X.Y.N, N the build number. A build without it is
//! `dev.<commit>`, as the scripts call one, so it never passes for a release.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VINTRDP_VERSION");
    println!("cargo:rerun-if-env-changed=WINDRES");
    let version = version();
    println!("cargo:rustc-env=VINTRDP_VERSION={version}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        resources(&version);
    }
}

fn version() -> String {
    if let Ok(v) = env::var("VINTRDP_VERSION").map(|v| v.trim().to_string()) {
        if !v.is_empty() {
            return v;
        }
    }
    // Rebuilt when the commit changes: HEAD, the branch it points at, packed refs.
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let mut watched = vec!["HEAD".to_string(), "packed-refs".to_string()];
    watched.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    match git(&["rev-parse", "--short=8", "HEAD"]) {
        Some(commit) => format!("dev.{commit}"),
        None => "dev".to_string(),
    }
}

/// Windows' numeric file version, X.Y.N.0, from a release's X.Y.N; 0.0.0.0 for a
/// dev build. The text fields carry the version itself.
fn file_version(version: &str) -> [u16; 4] {
    let parts: Option<Vec<u16>> = version.split('.').map(|p| p.parse().ok()).collect();
    match parts.as_deref() {
        Some(&[x, y, n]) => [x, y, n, 0],
        _ => [0; 4],
    }
}

fn resources(version: &str) {
    println!("cargo:rerun-if-changed=windows");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // windows/vintrdp.rc includes this for its VERSIONINFO.
    let [a, b, c, d] = file_version(version);
    let header = format!(
        "#define VINTRDP_FILE_VERSION {a},{b},{c},{d}\n#define VINTRDP_VERSION_TEXT \"{}\"\n",
        version.replace(['"', '\\'], "")
    );
    std::fs::write(out_dir.join("version.h"), header).expect("write version.h");

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86" => "i686".to_string(),
        // The NT 4.0 target for PINT.
        "arm" => "armv7".to_string(),
        a => a.to_string(),
    };
    let out = out_dir.join("vintrdp-res.o");
    let target = format!("{arch}-w64-mingw32");
    // $WINDRES, else MinGW's windres for the target, else LLVM's (which the
    // NT 4.0 toolchain, linking with lld and no MinGW, leaves us to find).
    let candidates: Vec<(String, Vec<String>)> = match env::var("WINDRES") {
        Ok(w) => vec![(w, vec![])],
        Err(_) => {
            let llvm = vec![format!("--target={target}")];
            let mut c = vec![(format!("{target}-windres"), vec![]), ("llvm-windres".into(), llvm.clone())];
            c.extend((14..=22).rev().map(|v| (format!("llvm-windres-{v}"), llvm.clone())));
            c
        }
    };
    for (windres, extra) in &candidates {
        let Ok(status) = Command::new(windres)
            .args(extra)
            .arg("--include-dir")
            .arg(&out_dir)
            .args(["windows/vintrdp.rc", "-O", "coff", "-o"])
            .arg(&out)
            .status()
        else {
            continue;
        };
        assert!(status.success(), "{windres} failed");
        println!("cargo:rustc-link-arg-bins={}", out.display());
        return;
    }
    panic!("no resource compiler: install MinGW's {target}-windres or LLVM's llvm-windres, or set WINDRES");
}
