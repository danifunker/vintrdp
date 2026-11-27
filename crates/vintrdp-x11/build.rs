//! The bundled fonts (data/fonts, see its COPYING): a table of each font
//! directory's fonts.dir, fonts.alias and files, for font.rs.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/fonts");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut out =
        String::from("pub static BUNDLED: &[(&str, &str, &str, &[(&str, &[u8])])] = &[\n");
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
        let alias = if alias.exists() {
            format!("include_str!({})", path(&alias))
        } else {
            "\"\"".into()
        };
        writeln!(
            out,
            "    ({dir:?}, include_str!({}), {alias}, &[",
            path(&d.join("fonts.dir"))
        )
        .unwrap();
        for f in files {
            writeln!(
                out,
                "        ({f:?}, include_bytes!({})),",
                path(&d.join(&f))
            )
            .unwrap();
        }
        out.push_str("    ]),\n");
    }
    out.push_str("];\n");
    std::fs::write(
        Path::new(&std::env::var("OUT_DIR").unwrap()).join("fonts.rs"),
        out,
    )
    .unwrap();
}
