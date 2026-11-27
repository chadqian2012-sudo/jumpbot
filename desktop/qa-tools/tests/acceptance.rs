use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
fn read(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}
fn command(dir: &Path, sub: &str, dest: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_jumpbot-qa"));
    c.env_remove("JUMPBOT_NO_INPUT")
        .arg(sub)
        .arg("--execute-live")
        .arg("--exe")
        .arg(dir.join("jumpbot.exe"))
        .arg("--out")
        .arg(dir.join(dest));
    c
}
fn snap(state: &str, attempts: u64) -> Value {
    json!({"state":state,"paused":false,"stop_reason":"达到最大跳跃次数","attempts":attempts,"jumps":attempts,"landed":attempts,"deaths":0,"hits":attempts,"misses":0,"judged":attempts,"unknown":0})
}
#[test]
fn bounded_round_and_suite_with_text_only_worker() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let exe = p.join("jumpbot.exe");
    assert!(Command::new("rustc")
        .arg("--edition=2021")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/worker.rs"))
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap()
        .success());
    let hash = format!("{:x}", Sha256::digest(fs::read(&exe).unwrap()));
    fs::write(
        p.join("build-source.json"),
        json!({"commit":"fixture","files_sha256":{"jumpbot.exe":hash}}).to_string(),
    )
    .unwrap();
    let rows="index,result,center_hit,note,mask_backend\n1,landed,1,[center x1],simd\n2,landed,1,[center x2],simd\n";
    fs::write(p.join("rows.csv"), rows).unwrap();
    fs::write(
        p.join("transcript"),
        format!(
            "@jumpbot {}\n!stdin\n@jumpbot {}",
            snap("playing", 1),
            snap("stopped", 2)
        ),
    )
    .unwrap();
    let no_input = command(p, "run-frozen-acceptance", "disabled")
        .env("JUMPBOT_NO_INPUT", "1")
        .output()
        .unwrap();
    assert!(!no_input.status.success() && !p.join("disabled").exists());
    let mut offline = Command::new(&exe)
        .args(["--config", "with spaces.json", "--image", "q a.png"])
        .env("JUMPBOT_QA_HOLD_TEXT", "1")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let out = command(p, "run-frozen-acceptance", "pass")
        .args(["--jumps", "2", "--pause-every", "1", "--backend", "simd"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "exit {:?}, stdout {} stderr {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(p.join("received")).unwrap(), "pause\n");
    drop(offline.stdin.take());
    assert!(offline.wait().unwrap().success());
    let mut ambiguous = Command::new(&exe)
        .args(["--config", "directory --image frame/config.json"])
        .env("JUMPBOT_QA_HOLD_TEXT", "1")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let blocked = command(p, "run-frozen-acceptance", "blocked")
        .output()
        .unwrap();
    drop(ambiguous.stdin.take());
    assert!(ambiguous.wait().unwrap().success());
    assert!(!blocked.status.success() && !p.join("blocked").exists());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains(&ambiguous.id().to_string()));
    fs::write(p.join("suite-stop"), "").unwrap();
    assert!(!command(p, "run-frozen-acceptance", "pre-stopped")
        .arg("--suite-stop-file")
        .arg(p.join("suite-stop"))
        .output()
        .unwrap()
        .status
        .success());
    assert!(read(&p.join("pre-stopped/test-status.json"))["child_pid"].is_null());
    let state = read(&p.join("pass/test-status.json"));
    assert_eq!(state["pause_requests"], 1);
    assert_eq!(state["stats"]["hits"], 2);
    let args = fs::read_to_string(p.join("pass/logs/args")).unwrap();
    assert!(
        args.contains("--max-jumps\n2") && args.contains("--no-cuda") && !args.contains("--daemon")
    );
    let summary = Command::new(env!("CARGO_BIN_EXE_jumpbot-qa"))
        .arg("summarize-acceptance")
        .arg(p.join("pass"))
        .output()
        .unwrap();
    assert!(summary.status.success(), "{summary:?}");
    assert!(!command(p, "run-frozen-acceptance", "pass")
        .output()
        .unwrap()
        .status
        .success());
    fs::write(
        p.join("transcript"),
        format!(
            "@jumpbot {}\n!stdin\n!stdin\n@jumpbot {}",
            snap("playing", 1),
            snap("stopped", 2)
        ),
    )
    .unwrap();
    fs::remove_file(p.join("received")).unwrap();
    let mut child = command(p, "run-frozen-acceptance", "stop")
        .args(["--jumps", "2", "--pause-every", "1"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !p.join("received").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(30));
    }
    fs::write(p.join("stop/stop.request"), "").unwrap();
    assert!(!child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(p.join("received")).unwrap(),
        "pause\nstop\n"
    );
    assert_eq!(read(&p.join("stop/test-status.json"))["round_pass"], false);
    fs::write(p.join("transcript"), "@jumpbot {").unwrap();
    fs::write(p.join("exit-code"), "").unwrap();
    assert!(!command(p, "run-skin-acceptance", "suite")
        .output()
        .unwrap()
        .status
        .success());
    let suite = read(&p.join("suite/suite-status.json"));
    assert_eq!(suite["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(suite["all_rounds_pass"], false);
    assert_eq!(suite["rounds"][0]["evidence"]["return_code"], 7);
}
