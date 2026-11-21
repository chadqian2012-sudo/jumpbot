// A text-only child for the runner protocol. It has no window or input API.
use std::{
    env, fs,
    io::{self, BufRead},
    path::Path,
};
fn main() {
    let args: Vec<_> = env::args().collect();
    let log = Path::new(&args[args.iter().position(|a| a == "--log-dir").unwrap() + 1]);
    fs::create_dir_all(log).unwrap();
    if Path::new("rows.csv").exists() {
        fs::copy("rows.csv", log.join("run_fixture.csv")).unwrap();
    }
    fs::write(log.join("args"), args.join("\n")).unwrap();
    let mut input = io::stdin().lock().lines();
    for line in fs::read_to_string("transcript").unwrap().lines() {
        if line == "!stdin" {
            let cmd = input
                .next()
                .transpose()
                .unwrap()
                .unwrap_or_else(|| "EOF".into());
            let mut data = fs::read_to_string("received").unwrap_or_default();
            data += &format!("{cmd}\n");
            fs::write("received", data).unwrap();
        } else if let Some(err) = line.strip_prefix("!stderr ") {
            eprintln!("{err}");
        } else {
            println!("{line}");
        }
    }
    if Path::new("exit-code").is_file() {
        std::process::exit(7);
    }
}
