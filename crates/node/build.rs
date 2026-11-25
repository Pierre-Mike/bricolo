//! Embeds every file under `templates/` in the `bricolo` binary, so `bricolo new`,
//! `bricolo new-script` and `bricolo new-sidecar` work from an installed `bricolo`
//! without this repository. Writes `$OUT_DIR/templates.rs` with one
//! `include_str!` per file, and `$OUT_DIR/skill.rs` the same way for
//! `skills/create-bricolo-app`, which `bricolo skill install` writes out.

use std::path::{Path, PathBuf};

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    for entry in std::fs::read_dir(dir).expect("cannot read templates/") {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).unwrap();
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates")
        .canonicalize()
        .expect("templates/ is missing");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.sort();
    let mut code = String::from(
        "/// Every file under templates/, by its `/`-separated path relative to it.\n\
         pub const FILES: &[(&str, &str)] = &[\n",
    );
    for (rel, path) in files {
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("templates.rs");
    std::fs::write(out, code).unwrap();

    // The create-bricolo-app skill, for `bricolo skill install`. Its evals stay in
    // the repository: they are for developing the skill, not using it.
    let skill = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/create-bricolo-app")
        .canonicalize()
        .expect("skills/create-bricolo-app is missing");
    println!("cargo:rerun-if-changed={}", skill.display());
    let mut files = Vec::new();
    walk(&skill, &skill, &mut files);
    files.retain(|(rel, _)| !rel.starts_with("evals/"));
    files.sort();
    let mut code = String::from(
        "/// Every file of skills/create-bricolo-app but evals/, by its `/`-separated path.\n\
         pub const FILES: &[(&str, &str)] = &[\n",
    );
    for (rel, path) in files {
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("skill.rs");
    std::fs::write(out, code).unwrap();

    // On macOS, `bricolo` carries Info.plist in its __TEXT,__info_plist section:
    // its identifier and the permission usage strings (docs/install.md).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        println!("cargo:rerun-if-changed={}", plist.display());
        println!(
            "cargo:rustc-link-arg-bin=bricolo=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
