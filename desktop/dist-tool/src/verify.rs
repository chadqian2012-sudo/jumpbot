use super::*;
use std::collections::BTreeSet;
use zip::ZipArchive;

pub(super) fn run_checks(version: &str, packages: &Path, install: bool) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
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
    for flavor in ["full", "portable"] {
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
        let target = temp.path().join("installed");
        if install {
            fs::create_dir_all(target.join("_internal/templates"))?;
            let user = b"{\"k\": 2.61, \"gamma\": 0}\n";
            fs::write(target.join("config.json"), user)?;
            copy(
                &app.join("templates/digit_1.png"),
                &target.join("_internal/templates/digit_0.png"),
            )?;
            run(Command::new(packages.join(exe_name))
                .args([
                    "/VERYSILENT",
                    "/SUPPRESSMSGBOXES",
                    "/NORESTART",
                    "/CURRENTUSER",
                    "/NOICONS",
                    "/MERGETASKS=!desktopicon",
                ])
                .arg(format!("/DIR={}", target.display())))?;
            for p in files(&app)? {
                let expected = if p == Path::new("templates/digit_0.png") {
                    app.join("templates/digit_1.png")
                } else {
                    app.join(&p)
                };
                if p == Path::new("config.json") {
                    if fs::read(target.join(&p))? != user {
                        return Err("installer replaced user settings".into());
                    }
                } else if sha(&target.join(&p))? != sha(&expected)? {
                    return Err(format!("installed file mismatch {}", p.display()).into());
                }
            }
            run_cli(&root, &target)?;
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
