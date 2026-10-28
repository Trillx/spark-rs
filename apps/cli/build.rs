use std::path::Path;
use std::{env, fs};

fn main() {
    // Link the VC++ runtime statically, as tauri-build does for spark-desktop,
    // so spark-cli.exe runs on Windows machines without the VC++ Redistributable.
    static_vcruntime::metabuild();
    embed_skills();
}

/// Lists every `skills/<name>/SKILL.md` for `commands.rs` to embed, so a new
/// skill needs no code.
fn embed_skills() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("cargo sets it")).join("skills");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("apps/cli/skills")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut list = String::from("&[\n");
    for name in names {
        let path = dir.join(&name).join("SKILL.md");
        list.push_str(&format!("    ({name:?}, include_str!({path:?})),\n"));
    }
    list.push_str("]\n");
    let out = Path::new(&env::var("OUT_DIR").expect("cargo sets it")).join("skills.rs");
    fs::write(out, list).expect("writing the skill list");
}
