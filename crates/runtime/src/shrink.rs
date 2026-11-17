//! `safeconduct shrink -- <test command>`: find the fewest fuzz faults that still make a test fail.
//!
//! The test runs an agent on `safeconduct replay --fuzz` (it starts the runtime itself, or a
//! client does) and fails with a nonzero exit code. Like `git bisect run`, shrink only reads that
//! code. It runs the test once with all faults, then again without each fault in turn, and keeps
//! a fault out when the test still fails the same way. The runtime reads the fault list from
//! SAFECONDUCT_FAULTS and writes the faults that happened to SAFECONDUCT_FAULT_LOG.
use serde_json::{Value, json};
use std::process::{Command, Stdio};

/// Returns the exit code for safeconduct: 0 = shrunk and the result fails again, 1 = not.
pub fn run(cmd: &[String]) -> i32 {
    let log = std::env::temp_dir().join(format!("safeconduct-faults-{}.jsonl", std::process::id()));
    let mut trials = 0;
    // Run the test once with only these faults (None = every fault). Its exit code, and its faults.
    let mut trial = |only: Option<&[String]>| -> (i32, Vec<Value>) {
        std::fs::write(&log, "").expect("write the fault log");
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..]).env("SAFECONDUCT_FAULT_LOG", &log).stdout(Stdio::null());
        if let Some(only) = only {
            c.env("SAFECONDUCT_FAULTS", only.join(","));
        }
        let code = match c.status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("shrink: cannot run {}: {e}", cmd[0]);
                std::process::exit(2);
            }
        };
        let faults: Vec<Value> = std::fs::read_to_string(&log).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        trials += 1;
        println!("{}", json!({ "event": "trial", "n": trials, "faults": only.map_or(faults.len(), <[String]>::len), "code": code }));
        (code, faults)
    };

    let (code, all) = trial(None);
    if code == 0 {
        println!("{}", json!({ "event": "shrunk", "error": "the test passed: there is no failure to shrink" }));
        return 1;
    }
    // Faults off the recording always happen and have no id: they cannot be turned off.
    let mut keep: Vec<String> = Vec::new();
    for id in all.iter().filter_map(|f| f["id"].as_str()) {
        if !keep.iter().any(|k| k == id) {
            keep.push(id.to_string());
        }
    }
    // ponytail: one fault at a time, n + 2 runs of the test (+1 per needed fault). Use ddmin if
    // tests get slow and faults many.
    let mut i = 0;
    while i < keep.len() {
        let without: Vec<String> = keep.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, k)| k.clone()).collect();
        // A pass decides that the fault is needed, so it must happen twice: one slow or flaky
        // pass (under load the test timed out once) would keep a fault that does not matter.
        if trial(Some(&without)).0 == code || trial(Some(&without)).0 == code {
            keep = without;
        } else {
            i += 1;
        }
    }
    // A live model or a flaky test can fail by chance: run the result once more.
    let reproduces = trial(Some(&keep)).0 == code;
    std::fs::remove_file(&log).ok();
    let faults: Vec<&Value> = keep.iter().filter_map(|id| all.iter().find(|f| f["id"] == id.as_str())).collect();
    println!(
        "{}",
        json!({
            "event": "shrunk", "faults": faults, "of": all.len(), "code": code, "trials": trials, "reproduces": reproduces,
            "reproduce": format!("SAFECONDUCT_FAULTS={} {}", keep.join(","), cmd.join(" ")),
        })
    );
    if reproduces { 0 } else { 1 }
}
