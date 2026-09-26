//! Regression: `openflux ... | head` must not print a Rust panic when the reader goes away.
#![cfg(unix)]

use std::process::Command;

/// A pipe holds 64 KiB, so an engine log well past that size guarantees the writer still has
/// output to push after `head -1` has exited and closed its end.
fn big_engine_log(dir: &std::path::Path) {
    let line = "2026/01/01 00:00:00 [ENGINE] 0123456789abcdef tunnel frame payload\n";
    let log = line.repeat(4096);
    std::fs::write(dir.join("engine.log"), log).expect("write engine log");
}

#[test]
fn a_closed_stdout_pipe_does_not_panic() {
    let tmp = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        tmp.path().join("openflux.toml"),
        "active_profile = \"demo\"\n\n[[profiles]]\nname = \"demo\"\n",
    )
    .expect("write config");
    big_engine_log(tmp.path());

    let script = format!(
        "{} --config-dir {} logs --lines 100000 | head -1",
        env!("CARGO_BIN_EXE_openflux"),
        tmp.path().display()
    );
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run pipeline");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "head should still exit 0");
    assert!(
        !out.stdout.is_empty(),
        "head should still print the first log line"
    );
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "CLI panicked on a closed stdout pipe: {stderr}"
    );
}
