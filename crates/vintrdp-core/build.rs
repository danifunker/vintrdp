//! Links GnuTLS for the `tls` feature; compiles nothing. src/tls/gnutls.rs
//! declares the functions it calls by hand.
//!
//! VINTRDP_DEPS names a prefix from scripts/dist/build-deps.sh: its libraries
//! are linked statically, so packages carry no third-party libraries.
//! Otherwise pkg-config finds the system's GnuTLS.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VINTRDP_DEPS");
    if env::var_os("CARGO_FEATURE_TLS").is_none() {
        return;
    }
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let deps = env::var("VINTRDP_DEPS").ok().filter(|d| !d.is_empty()).map(PathBuf::from);
    let Some(prefix) = deps else {
        // Failing here would fail `cargo check` for other targets too, which
        // links nothing: warn, and leave -lgnutls to the linker.
        if target_os == "windows" {
            println!("cargo:warning=Windows builds with the tls feature need VINTRDP_DEPS (see scripts/dist/build-deps.sh)");
            println!("cargo:rustc-link-lib=gnutls");
        } else if let Err(e) = pkg_config::Config::new().probe("gnutls") {
            println!("cargo:warning=pkg-config could not find gnutls ({e}); trying -lgnutls");
            println!("cargo:rustc-link-lib=gnutls");
        }
        return;
    };

    let lib = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    // In dependency order: each after the libraries that use it.
    for name in ["gnutls", "hogweed", "nettle", "tasn1", "gmp"] {
        println!("cargo:rustc-link-lib=static={name}");
    }
    if target_os == "windows" {
        for name in ["ws2_32", "crypt32", "ncrypt", "bcrypt", "iphlpapi"] {
            println!("cargo:rustc-link-lib={name}");
        }
    } else if target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
}
