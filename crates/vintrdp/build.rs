//! Stamps the release version, and on Windows compiles the icon and manifest
//! into the executable.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VINTRDP_VERSION");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=windows");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86" => "i686".to_string(),
        // The NT 4.0 target for PINT.
        "arm" => "armv7".to_string(),
        a => a.to_string(),
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("vintrdp-res.o");
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
        let Ok(status) = Command::new(windres).args(extra).args(["windows/vintrdp.rc", "-O", "coff", "-o"]).arg(&out).status() else {
            continue;
        };
        assert!(status.success(), "{windres} failed");
        println!("cargo:rustc-link-arg-bins={}", out.display());
        return;
    }
    panic!("no resource compiler: install MinGW's {target}-windres or LLVM's llvm-windres, or set WINDRES");
}
