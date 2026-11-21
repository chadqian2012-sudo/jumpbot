use std::{
    env,
    io::{self, IsTerminal},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let result = (|| -> io::Result<_> {
        let path = env::current_exe()?;
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
