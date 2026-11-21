use super::{cpu, Frame, Pair, Tables};
use std::{
    env,
    ffi::{c_char, c_void},
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    ptr,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

type Handle = *mut c_void;
use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_SYSTEM32};

macro_rules! driver_api {
    ($dll:literal; $(fn $name:ident($($arg:ty),*) -> $ret:ty;)+) => {
        #[allow(non_snake_case)]
        struct Api { _library:Library, $($name:unsafe extern "system" fn($($arg),*) -> $ret,)+ }
        impl Api {
            fn load()->Result<Rc<Self>,String> {
                let library=unsafe { Library::load_with_flags($dll, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|e|e.to_string())?;
                Ok(Rc::new(Self {
                    $($name:{
                        unsafe { *library.get::<unsafe extern "system" fn($($arg),*) -> $ret>(
                            concat!(stringify!($name),"\0").as_bytes()).map_err(|e| e.to_string())? }
                    },)+
                    _library:library
                }))
            }
        }
    };
}

fn check(status: i32, operation: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{operation} returned {status}"))
    }
}

#[path = "masks_opencl.rs"]
mod cl;
#[path = "masks_cuda.rs"]
mod cuda;

enum Runtime {
    Cuda(cuda::Cuda),
    Opencl(cl::Opencl),
}

pub(super) struct Device {
    runtime: Runtime,
    pub(super) name: String,
    pub(super) ms: f64,
}

impl Device {
    pub(super) fn cuda(table: &Tables) -> Result<Self, String> {
        let (runtime, name) = cuda::Cuda::new(table)?;
        Ok(Self {
            runtime: Runtime::Cuda(runtime),
            name: format!("CUDA {name}"),
            ms: 0.,
        })
    }
    pub(super) fn kind(&self) -> &'static str {
        match self.runtime {
            Runtime::Cuda(_) => "cuda",
            Runtime::Opencl(_) => "opencl",
        }
    }
    pub(super) fn compute(
        &mut self,
        f: &Frame,
        bg: &[u8],
        rows: [usize; 2],
    ) -> Result<Pair, String> {
        match &mut self.runtime {
            Runtime::Cuda(d) => d.compute(f, bg, rows),
            Runtime::Opencl(d) => d.compute(f, bg, rows),
        }
    }
}

fn preflight(selector: &str, candidate: Option<(usize, &str)>) -> Result<(), String> {
    let mut command = Command::new(env::current_exe().map_err(|e| e.to_string())?);
    let (index, name) = candidate
        .map(|(i, n)| (i.to_string(), n))
        .unwrap_or_else(|| ("list".into(), ""));
    let mut child = command
        .args(["--_opencl-check", selector])
        .env("JUMPBOT_OPENCL_PROBE_INDEX", index)
        .env("JUMPBOT_OPENCL_PROBE_NAME", name)
        .env("JUMPBOT_NO_INPUT", "1")
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("preflight exited {status}"))
            };
        }
        if start.elapsed() >= Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("preflight exceeded 30 seconds".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn opencl_devices(selector: &str, table: &Tables) -> Vec<Device> {
    let candidates = preflight(selector, None).and_then(|_| cl::candidates(selector));
    match candidates {
        Ok(list) => list
            .into_iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let name = candidate.name.clone();
                match preflight(selector, Some((index, &name))).and_then(|_| candidate.open(table))
                {
                    Ok(d) => Some(Device {
                        runtime: Runtime::Opencl(d),
                        name: format!("OpenCL {name}"),
                        ms: 0.,
                    }),
                    Err(e) => {
                        eprintln!("OpenCL {name}: {e}");
                        None
                    }
                }
            })
            .collect(),
        Err(e) => {
            eprintln!("OpenCL: {e}");
            Vec::new()
        }
    }
}

pub(super) fn probe(selector: &str, table: &Tables) -> bool {
    let Ok(list) = cl::candidates(selector) else {
        return false;
    };
    // Ordinals distinguish two adapters with the same vendor and model. Confirm
    // the name as well in case enumeration changed between the two processes.
    let list = match env::var("JUMPBOT_OPENCL_PROBE_INDEX").as_deref() {
        Ok("list") => return !list.is_empty(),
        Ok(index) => {
            let Ok(i) = index.parse::<usize>() else {
                return false;
            };
            let Some(candidate) = list.into_iter().nth(i) else {
                return false;
            };
            if env::var("JUMPBOT_OPENCL_PROBE_NAME").as_deref() != Ok(candidate.name.as_str()) {
                return false;
            }
            vec![candidate]
        }
        Err(_) => list,
    };
    let (w, h) = (37, 23);
    let frame = Frame {
        width: w,
        height: h,
        bgr: (0..w * h * 3).map(|i| (i * 47 + i / 7) as u8).collect(),
    };
    let bg = super::background(&frame);
    let rows = [2, h - 1];
    let expected = cpu(&frame, &bg, table, rows, false);
    list.into_iter().any(|c| {
        c.open(table)
            .and_then(|mut cl| cl.compute(&frame, &bg, rows))
            .is_ok_and(|v| v == expected)
    })
}
