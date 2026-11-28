//! The bundled fonts (data/fonts, see its COPYING): a table of each font
//! directory's fonts.dir, fonts.alias and files, for font.rs. The files are
//! kept gzipped, as X.org ships them, and embedded inflated: a 2 MB block of
//! compressed data made Microsoft Defender's machine-learning check call the
//! Windows ARM64 build a trojan (Bearfoos.A!ml); the same fonts uncompressed
//! pass.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/fonts");
    println!("cargo:rerun-if-changed={}", root.display());
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut out = String::from("pub static BUNDLED: &[(&str, &str, &str, &[(&str, &[u8])])] = &[\n");
    for dir in ["misc", "75dpi", "100dpi"] {
        let d = root.join(dir);
        println!("cargo:rerun-if-changed={}", d.display());
        let mut files: Vec<String> = std::fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("{}: {e}", d.display()))
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|f| f.ends_with(".pcf.gz"))
            .collect();
        files.sort();
        let alias = d.join("fonts.alias");
        let path = |p: &Path| format!("{:?}", p.display().to_string());
        let alias = if alias.exists() { format!("include_str!({})", path(&alias)) } else { "\"\"".into() };
        writeln!(out, "    ({dir:?}, include_str!({}), {alias}, &[", path(&d.join("fonts.dir"))).unwrap();
        for f in files {
            // Inflated here, so the binary carries plain PCF (see the top).
            let plain = out_dir.join(format!("{dir}-{}", f.trim_end_matches(".gz")));
            std::fs::write(&plain, gunzip(&std::fs::read(d.join(&f)).unwrap())).unwrap();
            writeln!(out, "        ({f:?}, include_bytes!({})),", path(&plain)).unwrap();
        }
        out.push_str("    ]),\n");
    }
    out.push_str("];\n");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("fonts.rs"), out).unwrap();
}

/// A gzip file's deflate stream, inflated (one member; the header's optional fields skipped).
fn gunzip(data: &[u8]) -> Vec<u8> {
    assert!(data.len() > 18 && data[0] == 0x1f && data[1] == 0x8b && data[2] == 8, "not gzip");
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + u16::from_le_bytes([data[at], data[at + 1]]) as usize;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            at += data[at..].iter().position(|&b| b == 0).unwrap() + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec(&data[at..]).expect("inflate")
}
