fn main() {
    if let Err(e) = jumpbot_desktop::profile::run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
