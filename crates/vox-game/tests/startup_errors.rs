//! Runs the built game with mods that cannot load: it must stop with a non-zero exit code and
//! name the problem on stderr. Startup fails before the event loop is created, so no window
//! opens.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

#[test]
fn a_missing_dependency_stops_the_game_before_a_window_opens() {
    let mods = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_vox-game"))
        .arg("--mods")
        .arg(&mods)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .find(|line| line.starts_with("error:"))
        .unwrap_or_else(|| panic!("no error line: {stderr}"));
    assert!(line.contains("lonely") && line.contains("gem"), "{line}");
    // The game never got as far as printing the load order or opening a window.
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Mods:"));
}
