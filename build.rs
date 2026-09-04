//! Emit WAVE's standard Windows VST3 bundle path for cross-compilation.

use std::env;
use std::fs;

use toybox::bundle::windows::{WindowsBundleFormat, windows_bundle_paths, windows_rustc_link_arg};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "unknown".to_string());
    if target_os != "windows" {
        println!(
            "cargo:warning=skipping Windows VST3 bundle emission on non-Windows target ({target_os})"
        );
        return;
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let paths = windows_bundle_paths(WindowsBundleFormat::Vst3, "WAVE", &version);
    let output_path = paths.output_path(profile == "release");

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| {
            panic!(
                "failed to create VST3 output directory {}: {error}",
                parent.display()
            )
        });
    }

    let link_arg = windows_rustc_link_arg(output_path);
    println!("cargo:rustc-cdylib-link-arg={link_arg}");
    println!(
        "cargo:warning=writing VST3 binary to {}",
        output_path.display()
    );
}
