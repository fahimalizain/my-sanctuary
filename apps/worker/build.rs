use std::env;
use std::fs;
use std::path::PathBuf;

/// Reads the app version from the root `package.json` (the single source of
/// truth, previously injected via Go `-ldflags`) and exposes it to the crate
/// as the `APP_VERSION` env var, readable with `env!("APP_VERSION")`.
fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let pkg_path = manifest_dir.join("../../package.json");

    println!("cargo:rerun-if-changed={}", pkg_path.display());

    let pkg: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&pkg_path).expect("failed to read ../../package.json"),
    )
    .expect("failed to parse ../../package.json");

    let version = pkg
        .get("version")
        .and_then(|v| v.as_str())
        .expect("root package.json is missing a string \"version\" field");

    println!("cargo:rustc-env=APP_VERSION={}", version);
}
