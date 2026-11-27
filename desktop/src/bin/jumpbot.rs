fn main() {
    if let Err(error) = jumpbot_desktop::runtime::run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
