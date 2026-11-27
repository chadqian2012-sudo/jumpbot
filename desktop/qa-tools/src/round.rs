use super::*;
use clap::{Args, ValueEnum};
use std::{
    io::{BufRead, BufReader},
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation},
    Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
        UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F8},
    },
};
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Backend {
    Auto,
    Simd,
    #[value(alias = "cuda")]
    Gpu,
    Opencl,
}
impl Backend {
    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Simd => "simd",
            Self::Gpu => "gpu",
            Self::Opencl => "opencl",
        }
    }
}
#[derive(Args)]
pub struct Suite {
    #[arg(long)]
    execute_live: bool,
    #[arg(long)]
    exe: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
#[derive(Args)]
pub struct Round {
    #[arg(long)]
    suite_stop_file: Option<PathBuf>,
    #[command(flatten)]
    common: Suite,
    #[arg(long,default_value_t=500,value_parser=clap::value_parser!(u32).range(1..=2147483647))]
    jumps: u32,
    #[arg(long,default_value_t=50,value_parser=clap::value_parser!(u32).range(1..=2147483647))]
    pause_every: u32,
    #[arg(long, value_enum, default_value = "auto")]
    backend: Backend,
}
fn cancelled(flag: &AtomicBool, stop: &Path) -> bool {
    if unsafe { GetAsyncKeyState(VK_F8 as i32) } < 0 {
        flag.store(true, Ordering::SeqCst);
    }
    flag.load(Ordering::SeqCst) || stop.is_file()
}
fn start(s: &Suite) -> Result<Arc<AtomicBool>> {
    if !s.execute_live || env::var("JUMPBOT_NO_INPUT").as_deref() == Ok("1") {
        return Err("Requires --execute-live and JUMPBOT_NO_INPUT must not be 1".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let f = stop.clone();
    ctrlc::set_handler(move || f.store(true, Ordering::SeqCst))?;
    Ok(stop)
}
fn fresh_dir(path: &Path) -> Result<PathBuf> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path)?;
    Ok(path.canonicalize()?)
}
pub fn passed(s: &Value) -> bool {
    s["state"] == "complete" && s["return_code"] == 0 && s["round_pass"] == true
}
fn could_own_input(name: &str, line: &str) -> bool {
    let native = name.eq_ignore_ascii_case("jumpbot.exe");
    if line.trim().is_empty() {
        return true;
    }
    if !native&&!regex::Regex::new(r"(?i)(?:jumpbot|live_acceptance)\.py\b|(?:^|\s)-m\s+(?:tools\.live_acceptance|jumpbot)\b").unwrap().is_match(line){return false;}
    let word = regex::Regex::new(r#""[^"\r\n]*"|[^\s"]+"#).unwrap();
    let mut pos = 0;
    let mut args = Vec::new();
    for m in word.find_iter(line) {
        if !line[pos..m.start()].chars().all(char::is_whitespace)
            || (m.start() == pos && pos != 0)
            || m.as_str().ends_with("\\\"")
        {
            return true;
        }
        args.push(m.as_str().trim_matches('"'));
        pos = m.end();
    }
    if !line[pos..].trim().is_empty() {
        return true;
    }
    let first = if native {
        1
    } else {
        if args
            .get(1)
            .and_then(|s| Path::new(s).file_name())
            .is_none_or(|s| !s.eq_ignore_ascii_case("jumpbot.py"))
        {
            return true;
        }
        2
    };
    let mut it = args.get(first..).unwrap_or_default().iter().copied();
    if let Some(head) = it.clone().next() {
        if head == "--_opencl-check" {
            return !matches!(args.get(first..),Some([_,s]) if !s.is_empty()&&!s.starts_with('-'));
        }
        if head == "--panel" {
            return it.count() != 1;
        }
    }
    let mut offline = false;
    while let Some(a) = it.next() {
        if matches!(a, "--selftest" | "--harvest-score") {
            offline = true;
            continue;
        }
        if matches!(a, "--no-cuda" | "--no-opencl" | "--worker") {
            continue;
        }
        let (k, v) = a.split_once('=').map_or((a, None), |(k, v)| (k, Some(v)));
        if !matches!(
            k,
            "--image"
                | "--config"
                | "--backend"
                | "--bench-repeat"
                | "--out-dir"
                | "--opencl-device"
                | "--score-dir"
        ) || v
            .or_else(|| it.next())
            .is_none_or(|s| s.is_empty() || s.starts_with('-'))
        {
            return true;
        }
        offline |= k == "--image";
    }
    !offline
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn command_line(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let handle = Handle(h);
        // Request only the command line of a potential JumpBot owner. The returned string
        // and its UNICODE_STRING header both live in this caller-owned query buffer.
        let mut bytes = 0;
        NtQueryInformationProcess(
            handle.0,
            ProcessCommandLineInformation,
            std::ptr::null_mut(),
            0,
            &mut bytes,
        );
        if !(std::mem::size_of::<UNICODE_STRING>() as u32..=131072).contains(&bytes) {
            return None;
        }
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        if NtQueryInformationProcess(
            handle.0,
            ProcessCommandLineInformation,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) < 0
        {
            return None;
        }
        let line = &*buffer.as_ptr().cast::<UNICODE_STRING>();
        let start = line.Buffer as usize;
        let end = start.checked_add(line.Length as usize)?;
        let base = buffer.as_ptr() as usize;
        if start < base
            || end > base + std::mem::size_of_val(buffer.as_slice())
            || !start.is_multiple_of(2)
            || !line.Length.is_multiple_of(2)
        {
            return None;
        }
        String::from_utf16(std::slice::from_raw_parts(
            line.Buffer,
            line.Length as usize / 2,
        ))
        .ok()
    }
}
fn check_owners() -> Result<()> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let snap = Handle(snap);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snap.0, &mut entry);
        let mut owners = vec![];
        while more != 0 {
            let n = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..n]).to_ascii_lowercase();
            if matches!(name.as_str(), "jumpbot.exe" | "python.exe" | "pythonw.exe")
                && command_line(entry.th32ProcessID).is_none_or(|s| could_own_input(&name, &s))
            {
                owners.push(entry.th32ProcessID);
            }
            more = Process32NextW(snap.0, &mut entry);
        }
        if GetLastError() != ERROR_NO_MORE_FILES {
            return Err(std::io::Error::last_os_error().into());
        }
        if owners.is_empty() {
            Ok(())
        } else {
            Err(format!("Another JumpBot process may own input: {owners:?}").into())
        }
    }
}
pub fn finish(s: &Value, counts: &Value, total: u64) -> Result<()> {
    if s["state"] != "stopped" || s["stop_reason"] != "达到最大跳跃次数" || s["paused"] != false
    {
        return Err("Worker did not finish the bounded round".into());
    }
    for (k, v) in counts.as_object().ok_or("Missing CSV counts")? {
        if v.as_u64().is_none() || s[k].as_u64() != v.as_u64() {
            return Err(format!("Worker and CSV disagree on {k}").into());
        }
    }
    let num = |k: &str| counts[k].as_u64().unwrap_or(0) as u128;
    if s["jumps"].as_u64() != Some(total)
        || counts["attempts"] != total
        || num("judged") == 0
        || num("landed") * 100 < 98 * total as u128
        || 100 * num("hits") < 95 * num("judged")
    {
        return Err("Sample count or required landing/hit rate was not met".into());
    }
    Ok(())
}
struct Package {
    exe: PathBuf,
    hash: String,
    manifest: String,
    commit: Value,
}
impl Package {
    fn open(exe: &Path) -> Result<Self> {
        let mut path = exe.canonicalize()?;
        if path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("jumpbot-desktop.exe"))
        {
            path.set_file_name("jumpbot.exe");
        }
        if !path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("jumpbot.exe"))
        {
            return Err("Expected packaged jumpbot.exe".into());
        }
        let manifest = path.with_file_name("build-source.json");
        let m = read_json(&manifest)?;
        let digest = sha(&manifest)?;
        let hash = sha(&path)?;
        if !m["files_sha256"]["jumpbot.exe"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case(&hash))
            || !m["commit"].is_string()
        {
            return Err("Packaged worker hash or commit missing/mismatched".into());
        }
        let version = path.with_file_name("source-version.json");
        if version.exists() {
            let v = read_json(&version)?;
            if v["commit"] != m["commit"]
                || !v["build_manifest_sha256"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&digest))
            {
                return Err("Release metadata disagrees with manifest".into());
            }
        }
        Ok(Self {
            exe: path,
            hash,
            manifest: digest,
            commit: m["commit"].clone(),
        })
    }
}
fn run_round(
    exe: &Path,
    out: &Path,
    total: u32,
    every: u32,
    backend: Backend,
    stop: &AtomicBool,
    suite_stop: &Path,
) -> Result<Value> {
    let p = Package::open(exe)?;
    check_owners()?;
    let dir = fresh_dir(out)?;
    let status = dir.join("test-status.json");
    let stop_file = dir.join("stop.request");
    let mut s = json!({"state":"starting","requested_jumps":total,"backend":backend.label(),"frozen_exe":p.exe,"frozen_exe_sha256":p.hash,"build_manifest_sha256":p.manifest,"source_commit":p.commit,"child_pid":null,"progress":null,"return_code":null,"pause_requests":0,"round_pass":false,"stats":null,"stop_reason":""});
    let stopped = || cancelled(stop, suite_stop) || stop_file.exists();
    let result = (|| -> Result<()> {
        save(&status, &s)?;
        if stopped() {
            return Err("User stop before worker launch".into());
        }
        let mut cmd = Command::new(&p.exe);
        cmd.current_dir(p.exe.parent().unwrap())
            .args([
                "--worker",
                "--max-jumps",
                &total.to_string(),
                "--backend",
                backend.label(),
                "--save-debug",
                "--out-dir",
            ])
            .arg(dir.join("debug"))
            .arg("--log-dir")
            .arg(dir.join("logs"))
            .creation_flags(0x08000000);
        if matches!(backend, Backend::Simd) {
            cmd.args(["--no-cuda", "--no-opencl"]);
        }
        let mut console = fs::File::create(dir.join("console.log"))?;
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(dir.join("stderr.log"))?)
            .spawn()?;
        s["child_pid"] = child.id().into();
        s["state"] = "running".into();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let monitored = (|| -> Result<()> {
            save(&status, &s)?;
            let mut next = every as u64;
            let mut stopping = false;
            let mut eof = false;
            loop {
                if stopped() {
                    s["stop_reason"] = "User stop requested".into();
                }
                if !stopping && s["stop_reason"] != "" {
                    stopping = true;
                    s["state"] = "stopping".into();
                    if let Some(mut pipe) = child.stdin.take() {
                        let _ = writeln!(pipe, "stop");
                    }
                    save(&status, &s)?;
                }
                if eof {
                    if child.try_wait()?.is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(40));
                    continue;
                }
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        eof = true;
                        continue;
                    }
                    Ok(line) => {
                        let line = line?;
                        writeln!(console, "{line}")?;
                        let Some(payload) = line.strip_prefix("@jumpbot ") else {
                            continue;
                        };
                        let snap: Value = serde_json::from_str(payload)?;
                        if snap["state"].as_str().is_none_or(str::is_empty) {
                            return Err("Invalid worker snapshot".into());
                        }
                        let n = snap["attempts"].as_u64().unwrap_or(0);
                        let jumps = snap["jumps"].as_u64().unwrap_or(0);
                        if !stopping {
                            if n > 0
                                && (100u128
                                    * n.saturating_sub(snap["landed"].as_u64().unwrap_or(0))
                                        as u128
                                    > 2 * total as u128
                                    || 100u128 * snap["misses"].as_u64().unwrap_or(0) as u128
                                        > 5 * total as u128)
                            {
                                s["stop_reason"] = "Rate threshold is unreachable".into();
                            } else if matches!(snap["state"].as_str(), Some("playing" | "dead"))
                                && jumps >= next
                                && jumps < u64::from(total)
                                && child.try_wait()?.is_none()
                            {
                                writeln!(
                                    child.stdin.as_mut().ok_or("Missing worker stdin")?,
                                    "pause"
                                )?;
                                s["pause_requests"] =
                                    (s["pause_requests"].as_u64().unwrap() + 1).into();
                                next = jumps + u64::from(every);
                            }
                        }
                        s["progress"] = snap;
                        save(&status, &s)?;
                    }
                }
            }
            Ok(())
        })();
        child.stdin.take();
        let code = child.wait()?;
        let _ = reader.join();
        s["return_code"] = code.code().into();
        monitored?;
        let counts = report::Rows::one(&dir)?.recorded_counts()?;
        s["stats"] = counts.clone();
        if !code.success() || s["stop_reason"] != "" || stopped() {
            return Err("User, threshold or worker stop".into());
        }
        finish(&s["progress"], &counts, total as u64)?;
        if sha(&p.exe)? != p.hash || sha(&p.exe.with_file_name("build-source.json"))? != p.manifest
        {
            return Err("Package changed during the round".into());
        }
        Ok(())
    })();
    let ok = result.is_ok() && !stopped();
    if stopped() {
        s["stop_reason"] = "User stop requested".into();
    } else if let Err(e) = result {
        if s["stop_reason"] == "" {
            s["stop_reason"] = e.to_string().into();
        }
    } else {
        s["stop_reason"] = s["progress"]["stop_reason"].clone();
    }
    s["round_pass"] = ok.into();
    s["state"] = if ok { "complete" } else { "stopped" }.into();
    save(&status, &s)?;
    save(
        &dir.join("run-evidence.json"),
        &json!({"requested_jumps":total,"backend":backend.label(),"return_code":s["return_code"],"final_status":s["progress"],"pause_requests":s["pause_requests"],"frozen_exe_sha256":p.hash,"build_manifest_sha256":p.manifest}),
    )?;
    save(&dir.join("summary.json"), &s)?;
    Ok(s)
}
pub fn single(r: Round) -> Result<bool> {
    let stop = start(&r.common)?;
    let s = run_round(
        &r.common.exe,
        &r.common.out,
        r.jumps,
        r.pause_every,
        r.backend,
        &stop,
        r.suite_stop_file
            .as_deref()
            .unwrap_or(&r.common.out.join("stop.request")),
    )?;
    println!("{s}");
    Ok(passed(&s))
}
pub fn suite(s: Suite) -> Result<bool> {
    let stop = start(&s)?;
    let exe = Package::open(&s.exe)?.exe;
    let dir = fresh_dir(&s.out)?;
    let sentinel = dir.join("stop.request");
    let status = dir.join("suite-status.json");
    let mut v = json!({"state":"running","rounds":[],"cumulative_attempts":0,"requested_skin":"gold","all_rounds_pass":false,"stop_reason":""});
    for (total, b) in [
        (500, Backend::Simd),
        (1000, Backend::Auto),
        (2000, Backend::Gpu),
    ] {
        if cancelled(&stop, &sentinel) {
            v["stop_reason"] = "User stop before next round".into();
            break;
        }
        let path = dir.join(format!("round-{total}-{}", b.label()));
        v["current_round"] = total.into();
        v["current_backend"] = b.label().into();
        save(&status, &v)?;
        let result = match run_round(&exe, &path, total, 50, b, &stop, &sentinel) {
            Ok(row) => row,
            Err(e) => {
                v["stop_reason"] = e.to_string().into();
                break;
            }
        };
        v["rounds"].as_array_mut().unwrap().push(json!({"jumps":total,"backend":b.label(),"directory":path,"process_return_code":i32::from(!passed(&result)),"evidence":result}));
        v["cumulative_attempts"] = (v["cumulative_attempts"].as_u64().unwrap()
            + result["stats"]["attempts"].as_u64().unwrap_or(0))
        .into();
        save(&status, &v)?;
        if !passed(&result) || cancelled(&stop, &sentinel) {
            v["stop_reason"] = result["stop_reason"].clone();
            break;
        }
    }
    let ok = !cancelled(&stop, &sentinel)
        && v["rounds"].as_array().unwrap().len() == 3
        && v["rounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| passed(&r["evidence"]));
    v["all_rounds_pass"] = ok.into();
    v["state"] = if ok { "complete" } else { "stopped" }.into();
    save(&status, &v)?;
    println!("{v}");
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_offline_processes_and_ambiguous_owners() {
        for line in [
            r#"jumpbot.exe --config "config with spaces.json" --image "frame 1.png" --bench-repeat 3"#,
            "jumpbot.exe --_opencl-check list",
        ] {
            assert!(!could_own_input("jumpbot.exe", line));
        }
        for line in [
            r#"jumpbot.exe --config "directory --image frame/config.json""#,
            "jumpbot.exe --probe",
            "jumpbot.exe --_opencl-check list --worker",
            "jumpbot.exe --worker --max-jumps 0",
            "jumpbot.exe --image --worker",
        ] {
            assert!(could_own_input("jumpbot.exe", line));
        }
        assert!(could_own_input(
            "pythonw.exe",
            "pythonw.exe jumpbot.py --probe"
        ));
        assert!(!could_own_input("python.exe", "python.exe unrelated.py"));
    }
    #[test]
    fn exact_csv_thresholds_and_terminal_state() {
        let counts = json!({"attempts":100,"landed":98,"deaths":2,"hits":76,"misses":4,"judged":80,"unknown":18});
        let mut status = counts.clone();
        status["jumps"] = 100.into();
        status["state"] = "stopped".into();
        status["stop_reason"] = "达到最大跳跃次数".into();
        status["paused"] = false.into();
        assert!(finish(&status, &counts, 100).is_ok());
        for (k, v) in [
            ("misses", json!(3)),
            ("jumps", json!(99)),
            ("state", json!("playing")),
            ("paused", json!(true)),
            ("stop_reason", json!("用户停止")),
        ] {
            let mut s = status.clone();
            s[k] = v;
            assert!(finish(&s, &counts, 100).is_err());
        }
        for (hits, misses, landed) in [(75, 5, 98), (0, 0, 98), (98, 0, 97)] {
            let mut s = status.clone();
            s["hits"] = hits.into();
            s["misses"] = misses.into();
            s["judged"] = (hits + misses).into();
            s["landed"] = landed.into();
            let mut c = counts.clone();
            for k in ["hits", "misses", "judged", "landed"] {
                c[k] = s[k].clone();
            }
            assert!(finish(&s, &c, 100).is_err());
        }
        assert!(!passed(
            &json!({"state":"complete","round_pass":1,"return_code":0})
        ));
    }
}
