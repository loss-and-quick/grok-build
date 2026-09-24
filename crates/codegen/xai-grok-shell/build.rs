//! Build script for bundling ripgrep for the grok-shell crate.
//!
//! - If `GROK_SHELL_BUNDLE_RG_PATH` is set, always bundle it
//! - Otherwise, only bundle in release builds
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const RG_VER: &str = "15.0.0";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The browser client is baked in first, and unlike ripgrep it is baked into debug builds too:
    // `grok web` has to serve a page from a `cargo run` binary, not only from a release artifact.
    bundle_web_ui()?;

    // Only bundle in release builds to avoid slowing down cargo check.
    println!("cargo:rerun-if-env-changed=GROK_SHELL_BUNDLE_RG_PATH");
    println!("cargo:rerun-if-env-changed=GROK_SHELL_RG_DOWNLOAD_BASE");
    // Declare our custom cfg to the compiler so cfg(bundle_rg) is recognized by lints
    println!("cargo:rustc-check-cfg=cfg(bundle_rg)");

    // Bundle when a path override is set or this is a release build
    // Bail before touching the filesystem so debug `cargo check` needs no environment
    let path_override = env::var("GROK_SHELL_BUNDLE_RG_PATH").ok();
    let is_release = env::var("PROFILE").as_deref() == Ok("release");
    if path_override.is_none() && !is_release {
        return Ok(());
    }

    // In Bazel builds, write into OUT_DIR; XAI_ROOT/target/tmp is read-only inside the sandbox
    // Outside Bazel, prefer XAI_ROOT's shared cache dir and fall back to OUT_DIR for standalone checkouts where XAI_ROOT is unset
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let in_bazel = is_bazel_build(&manifest_dir);
    let gen_dir = if in_bazel {
        // OUT_DIR is always set by Cargo/Bazel for build scripts.
        PathBuf::from(env::var("OUT_DIR")?)
    } else if let Ok(xai_root) = env::var("XAI_ROOT") {
        PathBuf::from(xai_root).join("target/tmp/grok-shell-bundle-rg")
    } else {
        PathBuf::from(env::var("OUT_DIR")?)
    };
    fs::create_dir_all(&gen_dir)?;

    // Skip auto-bundling on Windows: ripgrep ships .zip archives there and this script only extracts .tar.gz
    // Returning before `cargo:rustc-cfg=bundle_rg` keeps the include_bytes! macros compiled out The runtime then falls back to `rg` on PATH (see src/util/ripgrep.rs::rg_path)
    // Users install via `winget install BurntSushi.ripgrep.MSVC` or `scoop install ripgrep` An explicit GROK_SHELL_BUNDLE_RG_PATH still bundles on Windows; the override branch below copies any binary regardless of target
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" && path_override.is_none() {
        return Ok(());
    }

    // Expose cfg so the crate can include the bundled bytes.
    println!("cargo:rustc-cfg=bundle_rg");
    println!("cargo:rustc-env=GROK_SHELL_RG_VER={}", RG_VER);
    println!(
        "cargo:rustc-env=GROK_SHELL_RG_GEN_DIR={}",
        gen_dir.display()
    );

    // If a local rg binary is provided, copy it directly and skip the target check
    if let Some(path) = path_override {
        let dest = gen_dir.join(format!("rg-{}-override.bin", RG_VER));
        println!("cargo:rustc-env=GROK_SHELL_RG_TARGET=override");
        let _ = fs::remove_file(&dest);
        fs::copy(PathBuf::from(path.clone()), &dest).map_err(|e| {
            format!(
                "Failed copying GROK_SHELL_BUNDLE_RG_PATH: {e} from path {path} to dest {}",
                dest.display()
            )
        })?;
        return Ok(());
    }

    // Determine supported ripgrep asset triple for auto-download.
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let asset_triple = match (target_os.as_str(), target_arch.as_str()) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => {
            return Err(format!(
                "Unsupported target for ripgrep bundling: {os}-{arch}. Set GROK_SHELL_BUNDLE_RG_PATH to a local rg binary for offline or unsupported builds.",
                os = target_os,
                arch = target_arch
            ).into());
        }
    };

    println!("cargo:rustc-env=GROK_SHELL_RG_TARGET={}", asset_triple);
    let dest = gen_dir.join(format!("rg-{}-{}.bin", RG_VER, asset_triple));
    let _ = fs::remove_file(&dest);

    // The download base is overridable so sandboxed or offline CI can point at an internal mirror; it defaults to the public GitHub releases URL
    // Example: GROK_SHELL_RG_DOWNLOAD_BASE=http://<mirror>/github/BurntSushi/ripgrep/releases/download
    let download_base = env::var("GROK_SHELL_RG_DOWNLOAD_BASE")
        .unwrap_or_else(|_| "https://github.com/BurntSushi/ripgrep/releases/download".to_string());
    let url = format!(
        "{base}/{v}/ripgrep-{v}-{t}.tar.gz",
        base = download_base.trim_end_matches('/'),
        v = RG_VER,
        t = asset_triple
    );

    let bytes: Vec<u8> = {
        let resp = reqwest::blocking::get(&url).map_err(|e| {
            format!(
                "Failed to download ripgrep: {}\nSet GROK_SHELL_BUNDLE_RG_PATH to a local rg for offline builds.",
                e
            )
        })?;
        if !resp.status().is_success() {
            return Err(format!(
                "HTTP {} downloading ripgrep. Set GROK_SHELL_BUNDLE_RG_PATH for offline builds.",
                resp.status()
            )
            .into());
        }
        resp.bytes()?.to_vec()
    };

    let gz = flate2::read::GzDecoder::new(bytes.as_slice());
    let mut ar = tar::Archive::new(gz);
    let mut found = false;
    for entry in ar.entries()? {
        let mut e = entry?;
        let p = e.path()?;
        if p.file_name().is_some_and(|n| n == "rg") {
            let data: Vec<u8> = {
                let mut v = Vec::new();
                io::copy(&mut e, &mut v)?;
                v
            };
            fs::write(&dest, &data)?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(format!(
            "Could not find 'rg' in ripgrep archive {}. Set GROK_SHELL_BUNDLE_RG_PATH for offline builds.",
            url
        )
        .into());
    }

    Ok(())
}

/// Bake `sdk/web/dist` into the binary, or record that there was nothing to bake.
///
/// This reads a directory; it never builds one. Shelling out to `bun` from here would put a
/// JavaScript toolchain on the critical path of every `cargo build` — including the sandboxed,
/// network-free nix build, where `bun install` cannot run at all — to produce an artifact that
/// changes far less often than the Rust does. So the JS build stays a separate, explicit step and
/// this script only picks up whatever it left behind.
///
/// `GROK_WEB_DIST` names that directory for packagers who build the client elsewhere, exactly as
/// `GROK_SHELL_BUNDLE_RG_PATH` above names a ripgrep the sandbox could not download.
///
/// A missing directory is not an error. The binary still builds, `/ws` still works, and the page
/// route explains itself instead of being blank — see `agent::web_assets`.
fn bundle_web_ui() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=GROK_WEB_DIST");
    println!("cargo:rustc-check-cfg=cfg(web_ui)");
    // Emitting any `rerun-if-changed` replaces cargo's default "rerun when a package file changes",
    // so this script's own source has to be named or an edit to it would not take effect.
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let dist = match env::var_os("GROK_WEB_DIST") {
        Some(path) => PathBuf::from(path),
        // crates/codegen/xai-grok-shell → repository root.
        None => manifest_dir.join("../../../sdk/web/dist"),
    };

    let index = dist.join("index.html");
    if !index.is_file() {
        // Watch the nearest directory that *does* exist, so the build picks the client up the first
        // time somebody runs `bun run build`. Naming a path that does not exist would make cargo
        // re-run this script on every single build, and the ripgrep download above is not free.
        let watch = if dist.is_dir() {
            Some(dist.clone())
        } else {
            dist.parent().filter(|p| p.is_dir()).map(Path::to_path_buf)
        };
        if let Some(watch) = watch {
            println!("cargo:rerun-if-changed={}", watch.display());
        }
        return Ok(());
    }

    let mut files = Vec::new();
    collect_web_files(&dist, &dist, &mut files)?;
    files.sort();

    let mut generated = String::from(
        "// @generated by build.rs from the `sdk/web` bundle. Do not edit.\n\
         pub(super) static ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for (relative, absolute) in &files {
        println!("cargo:rerun-if-changed={}", absolute.display());
        // Both are written as Rust string literals, so a path carrying a quote or a backslash would
        // otherwise produce a file that does not compile.
        generated.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            relative, absolute
        ));
    }
    generated.push_str("];\n");

    let out = PathBuf::from(env::var("OUT_DIR")?).join("web_assets_generated.rs");
    fs::write(&out, generated)?;
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rustc-cfg=web_ui");
    Ok(())
}

/// Every file under `dir`, as (slash-separated path relative to `root`, absolute path).
fn collect_web_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect_web_files(root, &path, out)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((relative, path));
    }
    Ok(())
}

fn is_bazel_build(manifest_dir: &Path) -> bool {
    let manifest_dir_str = manifest_dir.to_string_lossy();
    env::var_os("BAZEL_WORKSPACE").is_some()
        || env::var_os("BUILD_WORKSPACE_DIRECTORY").is_some()
        || env::var_os("BAZEL_EXECUTION_ROOT").is_some()
        || env::var_os("BAZEL_OUTPUT_BASE").is_some()
        || manifest_dir_str.contains("/execroot/")
        || manifest_dir_str.contains("/bazel-out/")
}
