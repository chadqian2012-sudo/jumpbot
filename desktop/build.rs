use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=JUMPBOT_OPENCV_DIR");
    let sdk = env::var_os("JUMPBOT_OPENCV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../build/cv-sdk"));
    let vc = fs::read_dir(sdk.join("x64"))
        .expect("Set JUMPBOT_OPENCV_DIR to an OpenCV Windows x64 SDK with include and x64/vc*/lib")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("lib").is_dir())
        .max()
        .expect("OpenCV lib directory");
    let lib = fs::read_dir(vc.join("lib"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|s| s.starts_with("opencv_world") && s.ends_with(".lib") && !s.ends_with("d.lib"))
        .expect("Release opencv_world import library");
    let stem = lib.trim_end_matches(".lib");
    let dll = vc.join("bin").join(format!("{stem}.dll"));
    println!("cargo:rerun-if-changed={}", dll.display());
    println!(
        "cargo:rerun-if-changed={}",
        vc.join("lib").join(&lib).display()
    );
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = out
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .and_then(Path::parent)
        .expect("Cargo profile directory");
    for dir in [profile.to_owned(), profile.join("deps")] {
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(dll.file_name().unwrap());
        if fs::read(&dest).ok() != Some(fs::read(&dll).expect("OpenCV DLL")) {
            fs::copy(&dll, &dest).expect("stage OpenCV runtime");
        }
    }
}
