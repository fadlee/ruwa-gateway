//! Exposes the compiler version to `/app/metrics` (GOWA reports `go_version`).

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RUWA_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-changed=build.rs");
}
