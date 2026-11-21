use std::{
    env,
    io::{self, IsTerminal},
    process::{Command, ExitCode},
};

fn preserve_config(dir: &std::path::Path) -> io::Result<()> {
    let config = dir.join("config.json");
    let saved = dir.join("config.jumpbot-preserved.json");
    let mut source = match std::fs::File::open(&config) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let snapshot = (|| -> io::Result<()> {
        let mut pending = tempfile::NamedTempFile::new_in(dir)?;
        io::copy(&mut source, &mut pending)?;
        pending.as_file().sync_all()?;
        pending.persist(&saved)?;
        Ok(())
    })();
    drop(source);
    // Renaming on the same volume can still preserve the current bytes if a
    // full disk prevented a copy. The installer restores this before defaults.
    snapshot.or_else(|_| std::fs::rename(config, saved))
}

fn main() -> ExitCode {
    let result = (|| -> io::Result<_> {
        let path = env::current_exe()?;
        let dir = path.parent().unwrap();
        match env::args_os()
            .nth(1)
            .as_deref()
            .and_then(|arg| arg.to_str())
        {
            Some("--preserve-uninstall-config") => {
                preserve_config(dir)?;
                return Ok(0);
            }
            Some("--finish-config-restore") => {
                let saved = dir.join("config.jumpbot-preserved.json");
                if saved.try_exists()?
                    && std::fs::read(&saved)? == std::fs::read(dir.join("config.json"))?
                {
                    std::fs::remove_file(saved)?;
                }
                return Ok(0);
            }
            _ => {}
        }
        let args: &[&str] = match path.file_stem().and_then(|s| s.to_str()) {
            Some("run_long") => &["--max-jumps", "0", "--daemon"],
            Some("run_dryrun") => &["--dry-run", "--samples", "3"],
            Some("run_cpu") => &[
                "--backend",
                "simd",
                "--no-cuda",
                "--no-opencl",
                "--max-jumps",
                "0",
                "--daemon",
            ],
            Some("selftest") => &["--selftest"],
            _ => &[],
        };
        let status = Command::new(path.with_file_name("jumpbot.exe"))
            .current_dir(path.parent().unwrap())
            .args(args)
            .args(env::args_os().skip(1))
            .status()?;
        if !args.is_empty() && io::stdin().is_terminal() {
            println!("按回车键关闭窗口。");
            let _ = io::stdin().read_line(&mut String::new());
        }
        Ok(status.code().unwrap_or(1))
    })();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
