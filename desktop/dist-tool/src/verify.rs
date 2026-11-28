use super::*;
use std::collections::BTreeSet;
use zip::ZipArchive;

// Published v1.3 installers. Both AppIds are checked before either install starts.
const LEGACY: [(&str, &str, &str); 2] = [
    (
        "full",
        "{8F3A2C41-7B5E-4D19-9C6A-2E48D1F0B7A3}",
        "49597ff5952490372da1bbd313bf2af525a758a9f652216f31611ba12ef9f3b4",
    ),
    (
        "portable",
        "{B5B2F778-6F4B-4B39-A24D-707780C50259}",
        "9ff8eb769722674db46a433c98b60febf880add5810ec628f5124b1a278630c1",
    ),
];

fn registered(app_id: &str) -> Result<bool> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            root: isize,
            key: *const u16,
            options: u32,
            access: u32,
            handle: *mut isize,
        ) -> i32;
        fn RegCloseKey(handle: isize) -> i32;
    }
    let key: Vec<u16> =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{app_id}_is1")
            .encode_utf16()
            .chain(Some(0))
            .collect();
    for root in [0x8000_0001u32, 0x8000_0002] {
        // HKCU, HKLM
        for view in [0x0100, 0x0200] {
            // 64-bit and 32-bit registry views
            let mut handle = 0;
            // key is NUL-terminated and handle is writable for the duration of the call.
            let status = unsafe {
                RegOpenKeyExW(root as i32 as isize, key.as_ptr(), 0, 1 | view, &mut handle)
            };
            match status {
                0 => {
                    unsafe { RegCloseKey(handle) };
                    return Ok(true);
                }
                2 | 3 => continue, // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
                e => return Err(io::Error::from_raw_os_error(e).into()),
            }
        }
    }
    Ok(false)
}

fn require_unused_ids(hosted: bool, exists: impl Fn(&str) -> Result<bool>) -> Result<()> {
    for (_, id, _) in LEGACY {
        if exists(id)? {
            return Err(
                format!("installation already registered ({id}); refusing --install").into(),
            );
        }
    }
    if !hosted {
        return Err("--install requires a disposable GitHub-hosted Windows runner".into());
    }
    Ok(())
}

fn install(exe: &Path, target: &Path) -> Result<()> {
    run(Command::new(exe)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CURRENTUSER",
            "/NOICONS",
            "/MERGETASKS=!desktopicon",
        ])
        .arg(format!("/DIR={}", target.display())))
}

fn uninstall(target: &Path, id: &str) -> Result<()> {
    run(Command::new(target.join("unins000.exe")).args([
        "/VERYSILENT",
        "/SUPPRESSMSGBOXES",
        "/NORESTART",
    ]))?;
    // Inno's first phase exits before the second phase deletes the uninstaller.
    // Reinstalling during that interval could reuse a file still awaiting deletion.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while target.join("unins000.exe").try_exists()? {
        if std::time::Instant::now() >= deadline {
            return Err("Inno did not finish removing its uninstaller".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if registered(id)? || target.join("jumpbot.exe").try_exists()? {
        return Err("uninstall did not remove the registered application".into());
    }
    Ok(())
}

fn template_hashes(dir: &Path) -> Result<BTreeMap<PathBuf, String>> {
    files(dir)?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .map(|p| Ok((p.clone(), sha(&dir.join(p))?)))
        .collect()
}

fn check_upgrade(
    root: &Path,
    app: &Path,
    exe: &Path,
    temp: &Path,
    flavor: &str,
    id: &str,
    digest: &str,
) -> Result<()> {
    let target = temp.join("installed");
    let old_name = format!("jumpbot-setup-1.3-{flavor}.exe");
    let old = temp.join(&old_name);
    let url =
        format!("https://github.com/chadqian2012-sudo/jumpbot/releases/download/v1.3/{old_name}");
    run(Command::new(folder("SystemRoot").join("System32/curl.exe"))
        .args([
            "--fail",
            "--location",
            "--retry",
            "3",
            "--silent",
            "--show-error",
            "--output",
        ])
        .arg(&old)
        .arg(url))?;
    if sha(&old)? != digest {
        return Err(format!("v1.3 checksum mismatch: {old_name}").into());
    }
    install(&old, &target)?;
    let config = target.join("config.json");
    let legacy_digit = target.join("_internal/templates/digit_0.png");
    if !registered(id)? || !config.is_file() || !legacy_digit.is_file() {
        return Err("v1.3 install did not create the expected registration and user files".into());
    }
    let user = b"{\"k\": 2.83, \"gamma\": 0}\n";
    let custom_digit = app.join("templates/digit_1.png");
    let legacy_templates = target.join("_internal/templates");
    let templates = target.join("templates");
    fs::write(&config, user)?;
    copy(&custom_digit, &legacy_digit)?;
    copy(&custom_digit, &legacy_templates.join("custom/nested.png"))?;
    let mut expected_templates = template_hashes(&app.join("templates"))?;
    expected_templates.extend(template_hashes(&legacy_templates)?);
    install(exe, &target)?;
    if fs::read(&config)? != user || template_hashes(&templates)? != expected_templates {
        return Err("upgrade replaced v1.3 settings or templates".into());
    }
    for p in files(app)? {
        if p != Path::new("config.json")
            && !p.starts_with("templates")
            && sha(&target.join(&p))? != sha(&app.join(&p))?
        {
            return Err(format!("upgrade mismatch: {}", p.display()).into());
        }
    }
    run_cli(root, &target)?;
    // Preserve edits made after upgrading as well as the original v1.3 values.
    let edited = b"{\"k\": 2.92, \"gamma\": 0}\n";
    fs::write(&config, edited)?;
    copy(
        &app.join("templates/digit_2.png"),
        &templates.join("digit_0.png"),
    )?;
    expected_templates.insert(
        "digit_0.png".into(),
        sha(&app.join("templates/digit_2.png"))?,
    );
    let saved = target.join("config.jumpbot-preserved.json");
    uninstall(&target, id)?;
    if fs::read(&saved)? != edited || template_hashes(&templates)? != expected_templates {
        return Err("uninstall lost user settings or templates".into());
    }
    install(exe, &target)?;
    if fs::read(&config)? != edited
        || template_hashes(&templates)? != expected_templates
        || saved.exists()
    {
        return Err("reinstall did not restore user settings and templates".into());
    }
    uninstall(&target, id)
}

pub(super) fn run_checks(
    version: &str,
    packages: &Path,
    install: bool,
    offline: bool,
) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    if install && !offline {
        let hosted = env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted");
        require_unused_ids(hosted, registered)?;
    }
    let sums = fs::read_to_string(packages.join("SHA256SUMS.txt"))?;
    let mut verified = BTreeSet::new();
    for line in sums.lines() {
        let (expected, name) = line.split_once("  ").ok_or("invalid SHA256SUMS")?;
        if Path::new(name).file_name() != Some(OsStr::new(name))
            || !verified.insert(name.to_owned())
            || !sha(&packages.join(name))?.eq_ignore_ascii_case(expected)
        {
            return Err(format!("invalid checksum: {name}").into());
        }
    }
    if offline {
        let name = format!("jumpbot-setup-{version}-offline.exe");
        if !verified.contains(&name) {
            return Err("missing offline installer".into());
        }
        if install {
            run(Command::new("powershell.exe")
                .arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(root.join("tools/installer/verify-offline.ps1"))
                .arg("-Installer")
                .arg(packages.join(name)))?;
        }
        return Ok(());
    }
    for (flavor, id, digest) in LEGACY {
        let zip_name = format!("jumpbot-win64-v{version}-{flavor}.zip");
        let exe_name = format!("jumpbot-setup-{version}-{flavor}.exe");
        if !verified.contains(&zip_name) || !verified.contains(&exe_name) {
            return Err("missing release files".into());
        }
        let temp = tempfile::tempdir()?;
        let mut archive = ZipArchive::new(File::open(packages.join(&zip_name))?)?;
        let mut names = BTreeSet::new();
        for i in 0..archive.len() {
            let mut e = archive.by_index(i)?;
            let path = e.enclosed_name().ok_or("bad archive relative path")?;
            if !path.starts_with("jumpbot")
                || path.components().count() < 2
                || e.name().contains([':', '\\'])
                || !names.insert(path.to_string_lossy().to_lowercase())
                || e.is_symlink()
            {
                return Err("unexpected ZIP entry".into());
            }
            let dest = temp.path().join(path);
            if e.is_dir() {
                fs::create_dir_all(dest)?;
                continue;
            }
            fs::create_dir_all(dest.parent().unwrap())?;
            io::copy(&mut e, &mut File::create_new(dest)?)?;
        }
        let app = temp.path().join("jumpbot");
        verify_manifest(&app, flavor, version)?;
        run_cli(&root, &app)?;
        if install {
            check_upgrade(
                &root,
                &app,
                &packages.join(exe_name),
                temp.path(),
                flavor,
                id,
                digest,
            )?;
        }
        println!("{flavor} {} ZIP payload files verified", names.len());
        if std::env::var_os("JUMPBOT_TEST_GUI").is_some() {
            run(Command::new("cargo")
                .current_dir(&root)
                .args([
                    "test",
                    "--release",
                    "--manifest-path",
                    "desktop/Cargo.toml",
                    "--test",
                    "packaged_gui",
                    "--",
                    "--ignored",
                    "--nocapture",
                ])
                .env("JUMPBOT_TEST_PACKAGE", &app))?;
        }
    }
    Ok(())
}
fn verify_manifest(app: &Path, flavor: &str, version: &str) -> Result<()> {
    let build = app.join("build-source.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&build)?)?;
    let info: Value = serde_json::from_slice(&fs::read(app.join("source-version.json"))?)?;
    if manifest["flavor"] != flavor
        || info["flavor"] != flavor
        || info["commit"] != manifest["commit"]
        || info["version"] != version
        || info["build_manifest_sha256"] != sha(&build)?
    {
        return Err("package manifest identity mismatch".into());
    }
    let mut hashes = manifest["files_sha256"]
        .as_object()
        .ok_or("missing file list")?
        .clone();
    for n in [
        "jumpbot.exe",
        "jumpbot-desktop.exe",
        "config.json",
        "templates/piece.png",
        "THIRD_PARTY_NOTICES.md",
    ] {
        if !hashes.contains_key(n) {
            return Err(format!("missing required file {n}").into());
        }
    }
    for p in files(app)? {
        let relative = p.to_str().ok_or("non-Unicode payload")?.replace('\\', "/");
        if ["build-source.json", "source-version.json"].contains(&relative.as_str()) {
            continue;
        }
        let ext = p
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let lower = relative.to_ascii_lowercase();
        if [
            "py", "pyd", "pyc", "pyw", "pyo", "bat", "ps1", "mjs", "js", "cl", "ptx",
        ]
        .contains(&ext.as_str())
            || (ext == "html" && !lower.starts_with("licenses/"))
            || (["zip", "exe", "dll"].contains(&ext.as_str())
                && (lower.starts_with("python")
                    || lower.contains("webview2")
                    || lower.starts_with("node.")))
        {
            return Err(format!("unexpected runtime {relative}").into());
        }
        let digest = sha(&app.join(&p))?;
        if hashes
            .remove(&relative)
            .as_ref()
            .and_then(Value::as_str)
            .is_none_or(|v| !v.eq_ignore_ascii_case(&digest))
        {
            return Err(format!("modified/undeclared {relative}").into());
        }
    }
    if !hashes.is_empty() {
        return Err("payload is missing declared files".into());
    }
    Ok(())
}
fn run_cli(root: &Path, app: &Path) -> Result<()> {
    let mut c = Command::new(app.join("jumpbot.exe"));
    c.current_dir(app)
        .env("PATH", folder("SystemRoot").join("System32"))
        .env("JUMPBOT_NO_INPUT", "1")
        .env("JUMPBOT_DISABLE_CUDA", "1")
        .env("JUMPBOT_DISABLE_OPENCL", "1");
    c.args(["--backend", "simd", "--image"])
        .arg(root.join("tests/fixtures/skin-gold-play.png"))
        .arg("--image")
        .arg(root.join("tests/fixtures/purple-horizontal-fall.png"));
    let rows = output(&mut c)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() != 2
        || rows.iter().any(|r| r["backend"] != "simd")
        || !rows[1]["piece"].is_null()
        || !rows[1]["target"].is_null()
        || rows[0]["piece"]["x"]
            .as_f64()
            .is_none_or(|v| (v - 173.).abs() > 3.)
        || rows[0]["target"]["x"]
            .as_f64()
            .is_none_or(|v| (v - 404.).abs() > 3.)
    {
        return Err("packaged worker failed recognition fixtures".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installer_preflight_refuses_local_registered_or_unreadable_accounts() {
        assert!(require_unused_ids(false, |_| Ok(false)).is_err());
        for (_, present, _) in LEGACY {
            for hosted in [true, false] {
                assert!(require_unused_ids(hosted, |id| Ok(id == present)).is_err());
            }
        }
        assert!(require_unused_ids(true, |_| Err("access denied".into())).is_err());
        assert!(require_unused_ids(true, |_| Ok(false)).is_ok());
    }
}
