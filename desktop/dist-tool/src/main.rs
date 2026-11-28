mod verify;
use clap::Parser;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    ffi::OsStr,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, ZipWriter};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    version: String,
    #[arg(long)]
    output: PathBuf,
    /// Permit local validation of uncommitted source.
    #[arg(long)]
    candidate: bool,
    /// Build one standalone, component-selectable installer with its own preview AppId.
    #[arg(long)]
    offline_installer: bool,
    /// Check finished ZIPs, manifests and no-input recognition.
    #[arg(long)]
    verify: bool,
    /// Run the installers on a disposable Windows account during verification.
    #[arg(long, requires = "verify")]
    install: bool,
}

fn run(cmd: &mut Command) -> Result<()> {
    let code = cmd.status()?;
    if !code.success() {
        return Err(format!("{cmd:?} returned {code}").into());
    }
    Ok(())
}
fn output(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn sha(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut list = Vec::new();
    for entry in WalkDir::new(dir) {
        let e = entry?;
        if e.file_type().is_symlink() {
            return Err(format!("unexpected symlink {}", e.path().display()).into());
        }
        if e.file_type().is_file() {
            list.push(e.path().strip_prefix(dir)?.to_owned());
        }
    }
    list.sort();
    Ok(list)
}
fn copy(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst.parent().ok_or("no destination parent")?)?;
    fs::copy(src, dst)?;
    Ok(())
}
fn tree(src: &Path, dst: &Path) -> Result<()> {
    for p in files(src)? {
        copy(&src.join(&p), &dst.join(&p))?;
    }
    Ok(())
}
fn installed(program: &str, paths: &[PathBuf]) -> Result<PathBuf> {
    paths
        .iter()
        .cloned()
        .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default()).map(|p| p.join(program)))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("Missing {program}").into())
}
fn folder(var: &str) -> PathBuf {
    env::var_os(var).map(PathBuf::from).unwrap_or_default()
}
fn sdk_import(sdk: &Path) -> Result<PathBuf> {
    let mut libs = Vec::new();
    for item in WalkDir::new(sdk.join("x64")).max_depth(3) {
        let item = item?;
        let name = item.file_name().to_string_lossy();
        if item.file_type().is_file()
            && name
                .strip_prefix("opencv_world")
                .and_then(|s| s.strip_suffix(".lib"))
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|x| x.is_ascii_digit()))
        {
            libs.push(item.into_path());
        }
    }
    libs.sort();
    libs.pop()
        .ok_or_else(|| "OpenCV SDK release import library missing".into())
}
fn cargo(root: &Path, sdk: &Path, flavor: &str) -> Result<Command> {
    let mut c = Command::new("cargo");
    c.current_dir(root);
    if !matches!(flavor, "full" | "portable") {
        c.arg("--offline");
    }
    let lib = sdk_import(sdk)?;
    c.env("JUMPBOT_OPENCV_DIR", sdk)
        .env("OPENCV_LINK_PATHS", lib.parent().unwrap())
        .env("OPENCV_LINK_LIBS", lib.file_stem().unwrap())
        .env("OPENCV_INCLUDE_PATHS", sdk.join("include"));
    Ok(c)
}
fn licenses(root: &Path, sdk: &Path, stage: &Path, flavor: &str) -> Result<()> {
    let dst = stage.join("licenses");
    let mut c = cargo(root, sdk, flavor)?;
    c.args([
        "metadata",
        "--manifest-path",
        "desktop/Cargo.toml",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        "x86_64-pc-windows-msvc",
    ]);
    if flavor == "portable" {
        c.arg("--no-default-features");
    }
    let graph: Value = serde_json::from_str(&output(&mut c)?)?;
    let mut entries = Vec::new();
    for p in graph["packages"]
        .as_array()
        .ok_or("Cargo metadata missing packages")?
    {
        if p["source"].is_null() {
            continue;
        }
        let name = p["name"].as_str().ok_or("unknown crate")?;
        let version = p["version"].as_str().ok_or("missing version")?;
        let dir = Path::new(p["manifest_path"].as_str().ok_or("manifest")?)
            .parent()
            .unwrap();
        let d = dst.join(format!("{name}-{version}"));
        for e in fs::read_dir(dir)? {
            let e = e?;
            let n = e.file_name();
            let n = n.to_string_lossy();
            let prefix = n.split(['-', '_', '.']).next().unwrap_or("");
            if e.file_type()?.is_file()
                && [
                    "LICENSE",
                    "LICENCE",
                    "COPYING",
                    "COPYRIGHT",
                    "NOTICE",
                    "UNLICENSE",
                ]
                .contains(&prefix)
            {
                copy(&e.path(), &d.join(n.as_ref()))?;
            }
        }
        if let Some(path) = p["license_file"].as_str() {
            let f = dir.join(path);
            copy(&f, &d.join(f.file_name().ok_or("invalid license_file")?))?;
        }
        if dir.join("licenses").is_dir() {
            tree(&dir.join("licenses"), &d.join("licenses"))?;
        }
        entries.push(format!(
            "{name} {version} | {} | {}",
            p["license"].as_str().unwrap_or("see notices"),
            p["repository"].as_str().unwrap_or("")
        ));
    }
    entries.sort();
    fs::write(dst.join("cargo-dependencies.txt"), entries.join("\n"))?;
    let toolchain = output(Command::new("rustc").args(["--print", "sysroot"]))?;
    let docs = Path::new(toolchain.trim()).join("share/doc/rust");
    let notice = ["COPYRIGHT-library.html", "COPYRIGHT.html"]
        .iter()
        .map(|n| docs.join(n))
        .find(|p| p.is_file())
        .ok_or("missing Rust notices")?;
    copy(&notice, &dst.join("rust-standard-library.html"))?;
    copy(
        &root.join("desktop/ui/Saira-OFL.txt"),
        &dst.join("Saira-OFL.txt"),
    )?;
    tree(&sdk.join("etc/licenses"), &dst.join("OpenCV"))
}
fn source_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let list = output(Command::new("git").current_dir(root).args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "desktop",
        "templates",
        "tools/installer",
        "config.json",
        "README.md",
        "*NOTICE*",
        "LICENSE*",
    ]))?;
    list.split('\0')
        .filter(|s| !s.is_empty() && root.join(s).is_file())
        .map(|s| Ok((s.to_string(), sha(&root.join(s))?)))
        .collect()
}
fn build_binaries(
    root: &Path,
    sdk: &Path,
    flavor: &str,
    worker_only: bool,
) -> Result<BTreeMap<String, PathBuf>> {
    let target =
        root.join("desktop/target")
            .join(if flavor == "portable" { "portable" } else { "" });
    let mut c = cargo(root, sdk, flavor)?;
    c.args([
        "build",
        "--locked",
        "--release",
        "--manifest-path",
        "desktop/Cargo.toml",
        "--target",
        "x86_64-pc-windows-msvc",
        "--bin",
        "jumpbot",
        "--message-format=json-render-diagnostics",
        "--target-dir",
    ])
    .arg(&target);
    if !worker_only {
        c.args(["--bin", "jumpbot-desktop", "--bin", "jumpbot-launch"]);
    }
    match flavor {
        "portable" | "cpu" => {
            c.arg("--no-default-features");
        }
        "cuda" | "opencl" => {
            c.args(["--no-default-features", "--features", flavor]);
        }
        _ => {}
    }
    let artifacts = output(&mut c)?;
    let mut binaries = BTreeMap::new();
    for line in artifacts.lines() {
        let event: Value = serde_json::from_str(line)?;
        if event["reason"] == "compiler-artifact" {
            if let (Some(name), Some(path)) = (
                event["target"]["name"].as_str(),
                event["executable"].as_str(),
            ) {
                binaries.insert(name.to_owned(), PathBuf::from(path));
            }
        }
    }
    Ok(binaries)
}
fn build(root: &Path, sdk: &Path, stage: &Path, flavor: &str) -> Result<()> {
    let binaries = build_binaries(root, sdk, flavor, false)?;
    let binary = |name: &str| {
        binaries
            .get(name)
            .ok_or_else(|| format!("Cargo did not build {name}"))
    };
    for name in ["jumpbot", "jumpbot-desktop"] {
        copy(binary(name)?, &stage.join(format!("{name}.exe")))?;
    }
    for name in [
        "run.exe",
        "run_long.exe",
        "run_cpu.exe",
        "run_dryrun.exe",
        "selftest.exe",
    ] {
        copy(binary("jumpbot-launch")?, &stage.join(name))?;
    }
    let lib = sdk_import(sdk)?;
    let dll = lib
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid SDK layout")?
        .join("bin")
        .join(lib.file_name().unwrap())
        .with_extension("dll");
    copy(&dll, &stage.join(dll.file_name().unwrap()))?;
    let vswhere = folder("ProgramFiles(x86)").join("Microsoft Visual Studio/Installer/vswhere.exe");
    let crt = output(Command::new(vswhere).args([
        "-latest",
        "-products",
        "*",
        "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
        "-find",
        "VC\\Redist\\MSVC\\**\\x64\\Microsoft.VC*.CRT\\vcruntime140.dll",
    ]))?;
    let crt = crt
        .lines()
        .map(str::trim)
        .filter(|s| !s.contains("onecore") && !s.contains("debug_nonredist"))
        .max()
        .ok_or("missing VC++ redistributable runtime")?;
    for file in fs::read_dir(Path::new(crt).parent().unwrap())? {
        let file = file?;
        if file.path().extension() == Some(OsStr::new("dll")) {
            copy(&file.path(), &stage.join(file.file_name()))?;
        }
    }
    for n in [
        "config.json",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
        "LICENSE-OpenCV.txt",
    ] {
        copy(&root.join(n), &stage.join(n))?;
    }
    tree(&root.join("templates"), &stage.join("templates"))?;
    licenses(root, sdk, stage, flavor)?;
    if flavor == "offline" {
        for feature in ["cpu", "cuda", "opencl"] {
            let variant = build_binaries(root, sdk, feature, true)?;
            copy(
                variant.get("jumpbot").ok_or("missing worker")?,
                &stage.join("workers").join(feature).join("jumpbot.exe"),
            )?;
        }
    }
    let system = folder("SystemRoot");
    run(Command::new(stage.join("jumpbot.exe"))
        .current_dir(stage)
        .env("PATH", env::join_paths([system.join("System32"), system])?)
        .env("JUMPBOT_NO_INPUT", "1")
        .args(["--selftest", "--no-cuda", "--no-opencl"]))?;
    Ok(())
}
fn write_payload_manifest(
    stage: &Path,
    out: &Path,
    version: &str,
    flavor: &str,
) -> Result<PathBuf> {
    let mut entries = String::from("\u{feff}");
    let mut zip = if flavor == "offline" {
        None
    } else {
        Some(ZipWriter::new(File::create(
            out.join(format!("jumpbot-win64-v{version}-{flavor}.zip")),
        )?))
    };
    let mut cv_dll = None;
    for path in files(stage)? {
        let relative = path.to_str().ok_or("non-Unicode file")?.replace('\\', "/");
        if flavor == "offline"
            && relative
                .strip_prefix("opencv_world")
                .and_then(|s| s.strip_suffix(".dll"))
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            if cv_dll.replace(relative).is_some() {
                return Err("offline installer expects one OpenCV runtime".into());
            }
            continue; // The Inno template installs it from one of two sources.
        }
        if let Some(z) = &mut zip {
            z.start_file(format!("jumpbot/{relative}"), SimpleFileOptions::default())?;
            io::copy(&mut File::open(stage.join(&path))?, z)?;
        }
        let filename = relative.replace('/', "\\").replace('{', "{{");
        let parent = filename
            .rsplit_once('\\')
            .map(|(d, _)| format!("\\{d}"))
            .unwrap_or_default();
        let flags = if relative == "config.json"
            || relative.starts_with("templates/") && relative.ends_with(".png")
        {
            "onlyifdoesntexist uninsneveruninstall"
        } else {
            "ignoreversion"
        };
        let (parent, components) = if flavor == "offline" {
            match relative.as_str() {
                "workers/cpu/jumpbot.exe" => {
                    (String::new(), "; Components: not cuda and not opencl")
                }
                "workers/cuda/jumpbot.exe" => (String::new(), "; Components: cuda and not opencl"),
                "workers/opencl/jumpbot.exe" => {
                    (String::new(), "; Components: opencl and not cuda")
                }
                "jumpbot.exe" => (parent, "; Components: cuda and opencl"),
                _ => (parent, ""),
            }
        } else {
            (parent, "")
        };
        entries.push_str(&format!("Source: \"{{#PayloadDir}}\\{filename}\"; DestDir: \"{{app}}{parent}\"; Flags: {flags}{components}\r\n"));
    }
    if let Some(zip) = zip {
        zip.finish()?.sync_all()?;
    }
    if flavor == "offline" {
        let dll = cv_dll.ok_or("offline installer is missing its reference OpenCV runtime")?;
        let path = stage.join(&dll);
        let digest = sha(&path)?;
        let size = fs::metadata(path)?.len();
        entries.push_str(&format!(
            "#define public CvDll \"{dll}\"\r\n#define public CvHash \"{digest}\"\r\n#define public CvSize {size}\r\n"
        ));
    }
    let manifest = stage.with_extension("iss");
    fs::write(&manifest, entries)?;
    Ok(manifest)
}
fn write_package(
    root: &Path,
    stage: &Path,
    out: &Path,
    version: &str,
    flavor: &str,
    iscc: &Path,
) -> Result<()> {
    let manifest = write_payload_manifest(stage, out, version, flavor)?;
    let mut numeric: Vec<u16> = version
        .split('-')
        .next()
        .unwrap()
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    numeric.resize(4, 0);
    run(Command::new(iscc)
        .arg("/Q")
        .arg(format!("/DPayloadDir={}", stage.display()))
        .arg(format!("/DPayloadFiles={}", manifest.display()))
        .arg(format!("/DOutputDir={}", out.display()))
        .arg(format!("/DAppVersion={version}"))
        .arg(format!(
            "/DAppVersionInfo={}",
            numeric
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(".")
        ))
        .arg(format!("/DAppFlavor={flavor}"))
        .arg(root.join("tools/installer/jumpbot.iss")))?;
    Ok(())
}
fn package(o: Args) -> Result<()> {
    if !regex::Regex::new(r"^\d+\.\d+(?:\.\d+){0,2}(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$")?
        .is_match(&o.version)
    {
        return Err("invalid package version".into());
    }
    if o.verify {
        return verify::run_checks(
            &o.version,
            &std::path::absolute(&o.output)?,
            o.install,
            o.offline_installer,
        );
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    let sdk = std::path::absolute(
        std::env::var_os("JUMPBOT_OPENCV_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("build/cv-sdk")),
    )?;
    let dest = std::path::absolute(&o.output)?;
    if dest.exists() {
        return Err(format!("Output exists: {}", dest.display()).into());
    }
    let summary = root.join(format!("TEST_SUMMARY_v{}.md", o.version));
    if !o.candidate
        && (!summary.is_file()
            || !output(
                Command::new("git")
                    .current_dir(&root)
                    .args(["status", "--porcelain"]),
            )?
            .is_empty())
    {
        return Err("release requires committed source and its own test summary".into());
    }
    let commit = output(
        Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD"]),
    )?;
    let source = source_hashes(&root)?;
    let parent = dest.parent().ok_or("output parent missing")?;
    fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    let delivery = temp.path().join("delivery");
    fs::create_dir(&delivery)?;
    let iscc = installed(
        "ISCC.exe",
        &[
            folder("LOCALAPPDATA").join("Programs/Inno Setup 6/ISCC.exe"),
            folder("ProgramFiles(x86)").join("Inno Setup 6/ISCC.exe"),
        ],
    )?;
    let flavors: &[&str] = if o.offline_installer {
        &["offline"]
    } else {
        &["full", "portable"]
    };
    for &flavor in flavors {
        let stage = temp.path().join(flavor);
        fs::create_dir(&stage)?;
        build(&root, &sdk, &stage, flavor)?;
        let hashes = files(&stage)?
            .into_iter()
            .map(|p| {
                Ok((
                    p.to_string_lossy().replace('\\', "/"),
                    sha(&stage.join(&p))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let build_file = stage.join("build-source.json");
        fs::write(
            &build_file,
            serde_json::to_vec_pretty(
                &json!({"commit":commit.trim(),"flavor":flavor,"source_sha256":source,"files_sha256":hashes}),
            )?,
        )?;
        fs::write(
            stage.join("source-version.json"),
            serde_json::to_vec_pretty(
                &json!({"version":o.version,"commit":commit.trim(),"flavor":flavor,"candidate":o.candidate,"build_manifest_sha256":sha(&build_file)?}),
            )?,
        )?;
        write_package(&root, &stage, &delivery, &o.version, flavor, &iscc)?;
    }
    if source_hashes(&root)? != source
        || output(
            Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"]),
        )? != commit
    {
        return Err("source changed during build".into());
    }
    if summary.is_file() {
        copy(&summary, &delivery.join("TEST_SUMMARY.md"))?;
    }
    let mut sums = File::create_new(delivery.join("SHA256SUMS.txt"))?;
    for p in files(&delivery)? {
        if p != Path::new("SHA256SUMS.txt") {
            writeln!(sums, "{}  {}", sha(&delivery.join(&p))?, p.display())?;
        }
    }
    sums.sync_all()?;
    drop(sums);
    fs::rename(delivery, &dest)?;
    println!("{}", dest.display());
    Ok(())
}
fn main() -> ExitCode {
    match package(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_manifest_and_legacy_archives_use_their_expected_workers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for flavor in ["offline", "full", "portable"] {
            let stage = temp.path().join(flavor);
            fs::create_dir(&stage)?;
            fs::write(stage.join("config.json"), "{}")?;
            fs::write(stage.join("jumpbot.exe"), "both")?;
            let cv = stage.join("opencv_world4140.dll");
            fs::write(&cv, b"reference DLL bytes")?;
            let variants = [
                ("workers/cpu", "not cuda and not opencl"),
                ("workers/cuda", "cuda and not opencl"),
                ("workers/opencl", "opencl and not cuda"),
                ("", "cuda and opencl"),
            ];
            if flavor == "offline" {
                for (dir, _) in &variants[..3] {
                    fs::create_dir_all(stage.join(dir))?;
                    fs::write(stage.join(dir).join("jumpbot.exe"), dir)?;
                }
            }
            let manifest = fs::read_to_string(write_payload_manifest(
                &stage,
                temp.path(),
                "1.6.1",
                flavor,
            )?)?;
            let workers: Vec<_> = manifest
                .lines()
                .filter(|l| l.contains("jumpbot.exe"))
                .collect();
            assert!(manifest.lines().any(|l| l.contains("config.json")
                && l.contains("onlyifdoesntexist uninsneveruninstall")));
            let zip = temp
                .path()
                .join(format!("jumpbot-win64-v1.6.1-{flavor}.zip"));
            if flavor == "offline" {
                assert!(!zip.exists());
                assert!(manifest.contains("#define public CvSize 19"));
                assert!(manifest.contains("#define public CvDll \"opencv_world4140.dll\""));
                assert!(manifest.contains(&format!("#define public CvHash \"{}\"", sha(&cv)?)));
                assert!(!manifest
                    .lines()
                    .any(|l| l.starts_with("Source:") && l.contains("opencv_world")));
                fs::write(stage.join("opencv_world999.dll"), "ambiguous")?;
                assert!(write_payload_manifest(&stage, temp.path(), "1.6.1", flavor).is_err());
                assert_eq!(workers.len(), variants.len());
                for (dir, condition) in variants {
                    let source = Path::new(&dir.replace('/', r"\")).join("jumpbot.exe");
                    let entry = format!(
                        r#"Source: "{{#PayloadDir}}\{}"; DestDir: "{{app}}"; Flags: ignoreversion; Components: {condition}"#,
                        source.display()
                    );
                    assert!(
                        workers
                            .iter()
                            .any(|line| line.trim_start_matches('\u{feff}') == entry),
                        "{entry}"
                    );
                }
            } else {
                assert_eq!(workers.len(), 1);
                assert!(!workers[0].contains("Components:"));
                let mut archive = zip::ZipArchive::new(File::open(zip)?)?;
                assert!(archive.by_name("jumpbot/jumpbot.exe").is_ok());
                assert!(archive.by_name("jumpbot/opencv_world4140.dll").is_ok());
                assert!(manifest
                    .lines()
                    .any(|l| l.contains("opencv_world4140.dll")
                        && l.ends_with("Flags: ignoreversion")));
                assert!(!manifest.contains("CvHash"));
            }
        }
        Ok(())
    }

    #[test]
    fn desktop_shortcut_launches_gui_console() {
        let iss = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/installer/jumpbot.iss"),
        )
        .unwrap();
        let line = iss
            .lines()
            .find(|l| {
                l.contains("{autodesktop}")
                    && l.contains("AppTitle")
                    && l.contains("Tasks: desktopicon")
            })
            .expect("desktop shortcut line");
        assert!(
            line.contains(r#"Filename: "{app}\jumpbot-desktop.exe""#),
            "{line}"
        );
        assert!(!line.contains("--daemon"), "{line}");
    }
}
