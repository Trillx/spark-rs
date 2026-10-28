//! spark-cli run as agents run it, without the app.

use std::process::Command;

fn spark_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spark-cli"))
        .args(args)
        .output()
        .expect("spark-cli runs")
}

#[test]
fn a_mistyped_command_answers_in_json_when_asked() {
    let out = spark_cli(&["--json", "thread"]);
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("MESSAGE_ID"), "{message}");
    // Without --json, clap's own message on stderr.
    let plain = spark_cli(&["thread"]);
    assert!(plain.stdout.is_empty());
    assert!(!plain.status.success());
}

#[test]
fn help_and_version_stay_text() {
    let out = spark_cli(&["--json", "--version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("spark-cli "));
}
