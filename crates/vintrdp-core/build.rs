//! Compiles the vendored rdesktop protocol core (./rdesktop) into a static
//! library. Front-end functions (ui_*, xgui_*) and the state rdesktop.c used
//! to own are provided by the Rust crate.

use std::env;
use std::path::PathBuf;

const CORE_SOURCES: &[&str] = &[
    "tcp.c", "asn.c", "iso.c", "mcs.c", "secure.c", "licence.c", "rdp.c", "orders.c",
    "bitmap.c", "cache.c", "rdp5.c", "channels.c", "rdpdr.c", "addin.c", "serial.c",
    "printer.c", "disk.c", "parallel.c", "printercache.c", "mppc.c", "pstcache.c",
    "lspci.c", "seamless.c", "ssl.c", "utils.c", "stream.c", "dvc.c", "rdpear.c",
    "rdpedisp.c", "cliprdr.c", "ctrl.c",
    // CredSSP with the built-in NTLMv2 client (no GSSAPI/Kerberos)
    "cssp.c", "ntlmssp.c", "spnego.c",
];

const PKG_LIBS: &[&str] = &["gnutls", "nettle", "hogweed", "libtasn1"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("rdesktop");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let big_endian = env::var("CARGO_CFG_TARGET_ENDIAN").unwrap() == "big";
    println!("cargo:rerun-if-changed=rdesktop");

    let mut include_paths = Vec::new();
    for lib in PKG_LIBS {
        let found = pkg_config::Config::new()
            .probe(lib)
            .unwrap_or_else(|e| panic!("pkg-config could not find {lib}: {e}"));
        include_paths.extend(found.include_paths);
    }

    // GMP ships no .pc file; look in the usual prefixes.
    let prefixes: Vec<String> = match target_os.as_str() {
        "macos" => vec![env::var("HOMEBREW_PREFIX").unwrap_or_else(|_| "/opt/homebrew".into()), "/usr/local".into()],
        "linux" => vec![],
        _ => vec!["/usr/local".into(), "/usr/pkg".into()],
    };
    for p in &prefixes {
        include_paths.push(PathBuf::from(p).join("include"));
        println!("cargo:rustc-link-search=native={p}/lib");
    }
    println!("cargo:rustc-link-lib=gmp");
    // glibc has iconv built in; macOS and the BSDs need libiconv.
    if target_os != "linux" {
        println!("cargo:rustc-link-lib=iconv");
    }

    let mut b = cc::Build::new();
    b.includes(&include_paths)
        .include(&src)
        .flag("-include")
        .flag(src.join("nettle_compat.h").to_str().unwrap())
        .define(if big_endian { "B_ENDIAN" } else { "L_ENDIAN" }, "1")
        .define("HAVE_SYS_SELECT_H", None)
        .define("HAVE_LOCALE_H", None)
        .define("HAVE_LANGINFO_H", None)
        .define("HAVE_SYSEXITS_H", None)
        .define("HAVE_ICONV_H", None)
        .define("HAVE_ICONV", "1")
        .define("ICONV_CONST", "")
        .define("HAVE_DIRFD", None)
        .define("HAVE_SYS_PARAM_H", None)
        .define("IPv6", "1")
        .define("WITH_CREDSSP", None)
        .define("PACKAGE_VERSION", "\"1.9.1-vintrdp\"")
        .define("EGD_SOCKET", "\"/var/run/egd-pool\"")
        .define("KEYMAP_PATH", "\"/usr/share/rdesktop/keymaps/\"")
        .warnings(false)
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-implicit-function-declaration");

    // What ./configure discovers about filesystem statistics (disk.c).
    match target_os.as_str() {
        "linux" => {
            b.define("HAVE_SYS_VFS_H", None)
                .define("HAVE_SYS_STATVFS_H", None)
                .define("STAT_STATVFS", "1")
                .define("HAVE_STRUCT_STATVFS_F_NAMEMAX", None)
                .define("HAVE_MNTENT_H", None)
                .define("HAVE_SETMNTENT", None)
                .define("_GNU_SOURCE", None);
        }
        "netbsd" => {
            b.define("HAVE_SYS_STATVFS_H", None)
                .define("STAT_STATVFS", "1")
                .define("HAVE_STRUCT_STATVFS_F_NAMEMAX", None);
        }
        _ => {
            // macOS, FreeBSD, OpenBSD, DragonFly: 4.4BSD statfs(path, buf)
            b.define("HAVE_SYS_MOUNT_H", None).define("STAT_STATFS2_BSIZE", "1");
        }
    }

    for f in CORE_SOURCES {
        b.file(src.join(f));
    }
    b.compile("rdesktop_core");
}
