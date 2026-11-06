//! Stamps the release version, and on Windows compiles the icon and manifest
//! into the executable.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VINTRDP_VERSION");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=windows");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let arch = if arch == "x86" { "i686".to_string() } else { arch };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("vintrdp-res.o");
    let windres = env::var("WINDRES").unwrap_or_else(|_| format!("{arch}-w64-mingw32-windres"));
    let status = Command::new(&windres)
        .args(["windows/vintrdp.rc", "-O", "coff", "-o"])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("could not run {windres}: {e}"));
    assert!(status.success(), "{windres} failed");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
