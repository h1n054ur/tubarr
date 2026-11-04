use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    // `sqlx::migrate!` embeds the migrations at compile time. Rebuild when one is added, not only edited.
    println!("cargo:rerun-if-changed=migrations");
    embed_ui();
    embed_git_sha();
}

/// Passes the commit the image was built from (`TUBARR_GIT_SHA`, set by the Dockerfile from a build
/// arg) on as `TUBARR_BUILD_SHA`, cut to 12 characters. Anything that is not a hex sha is dropped, so
/// a plain `cargo build` simply reports no commit.
fn embed_git_sha() {
    println!("cargo:rerun-if-env-changed=TUBARR_GIT_SHA");
    let sha = env::var("TUBARR_GIT_SHA").unwrap_or_default();
    let sha = sha.trim();
    if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        let short = &sha[..sha.len().min(12)];
        println!(
            "cargo:rustc-env=TUBARR_BUILD_SHA={}",
            short.to_ascii_lowercase()
        );
    }
}

/// Lists the built web UI (`web/dist`, made by `bun run build`) for `admin::ui` to embed. A
/// missing dist is fine: the binary then serves a "UI not built" page, so `cargo build` never
/// needs Bun.
fn embed_ui() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let web = manifest.join("../../web");
    let dist = web.join("dist");
    let mut files = Vec::new();
    if dist.is_dir() {
        println!("cargo:rerun-if-changed={}", dist.display());
        collect(&dist, &dist, &mut files);
        files.sort();
    } else if web.is_dir() {
        // Watch the folder so a later `bun run build` is picked up.
        println!("cargo:rerun-if-changed={}", web.display());
    }
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        code.push_str(&format!("    ({rel:?}, include_bytes!({path:?})),\n"));
    }
    code.push_str("];\n");
    let out = PathBuf::from(env::var("OUT_DIR").expect("set by cargo")).join("ui_assets.rs");
    fs::write(out, code).expect("write ui_assets.rs");
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((rel, path.display().to_string()));
        }
    }
}
