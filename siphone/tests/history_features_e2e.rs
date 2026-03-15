use std::process::Command;

fn unique_test_dir(prefix: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    p.push(format!("sipr-{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn siphone_bin() -> String {
    std::env::var("CARGO_BIN_EXE_siphone")
        .expect("CARGO_BIN_EXE_siphone must be set by cargo test for integration tests")
}

#[test]
fn e2e_config_init_contains_default_max_history() {
    let out = Command::new(siphone_bin())
        .args(["config", "--init"])
        .output()
        .expect("failed to run siphone config --init");

    assert!(out.status.success(), "command failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"max_history\": 1000"),
        "expected max_history default in template, got:\n{}",
        stdout
    );
}

#[test]
fn e2e_config_show_reads_max_history_from_active_config() {
    let home = unique_test_dir("cfg-max-history");
    let config_path = home.join(".sipr.json");
    std::fs::write(&config_path, "{\n  \"max_history\": 321\n}\n").unwrap();

    let out = Command::new(siphone_bin())
        .args(["config", "--show"])
        .env("HOME", &home)
        .output()
        .expect("failed to run siphone config --show");

    assert!(out.status.success(), "command failed: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"max_history\": 321"),
        "expected active config value in output, got:\n{}",
        stdout
    );

    let _ = std::fs::remove_dir_all(home);
}
