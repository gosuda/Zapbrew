use std::env;
use std::io::{self, Write};
use std::process::Command;

fn main() {
    let rustc = match env::var_os("RUSTC") {
        Some(rustc) => rustc,
        None => "rustc".into(),
    };
    let output = match Command::new(&rustc).arg("--version").output() {
        Ok(output) => output,
        Err(error) => panic!("failed to run {rustc:?} --version: {error}"),
    };
    if !output.status.success() {
        panic!(
            "{rustc:?} --version exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let version = match String::from_utf8(output.stdout) {
        Ok(version) => version,
        Err(error) => panic!("{rustc:?} --version returned non-UTF-8 output: {error}"),
    };
    let version = version.trim();
    assert!(
        !version.is_empty(),
        "{rustc:?} --version returned no version"
    );

    let mut stdout = io::stdout().lock();
    if let Err(error) = writeln!(stdout, "cargo:rerun-if-env-changed=RUSTC") {
        panic!("failed to write Cargo build directive: {error}");
    }
    if let Err(error) = writeln!(stdout, "cargo:rustc-env=ZAPBREW_RUSTC_VERSION={version}") {
        panic!("failed to write rustc version build directive: {error}");
    }
}
