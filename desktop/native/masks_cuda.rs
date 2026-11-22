use super::*;
use std::ffi::CStr;

driver_api! {"nvcuda.dll";
    fn cuInit(u32)->i32;
    fn cuDeviceGet(*mut i32,i32)->i32;
    fn cuDeviceGetName(*mut c_char,i32,i32)->i32;
    fn cuCtxCreate_v2(*mut Handle,u32,i32)->i32;
    fn cuCtxDestroy_v2(Handle)->i32;
    fn cuCtxPushCurrent_v2(Handle)->i32;
    fn cuCtxPopCurrent_v2(*mut Handle)->i32;
    fn cuModuleLoadData(*mut Handle,*const c_void)->i32;
    fn cuModuleGetFunction(*mut Handle,Handle,*const c_char)->i32;
    fn cuMemAlloc_v2(*mut u64,usize)->i32;
    fn cuMemFree_v2(u64)->i32;
    fn cuMemcpyHtoD_v2(u64,*const c_void,usize)->i32;
    fn cuMemcpyDtoH_v2(*mut c_void,u64,usize)->i32;
    fn cuLaunchKernel(Handle,u32,u32,u32,u32,u32,u32,u32,Handle,*mut *mut c_void,*mut *mut c_void)->i32;
}

struct Current(Rc<Api>);
impl Current {
    fn push(api: &Rc<Api>, ctx: Handle) -> Result<Self, String> {
        check(
            unsafe { (api.cuCtxPushCurrent_v2)(ctx) },
            "cuCtxPushCurrent",
        )?;
        Ok(Self(api.clone()))
    }
}
impl Drop for Current {
    fn drop(&mut self) {
        unsafe {
            (self.0.cuCtxPopCurrent_v2)(&mut ptr::null_mut());
        }
    }
}

pub(super) struct Cuda {
    api: Rc<Api>,
    ctx: Handle,
    kernel: Handle,
    constants: u64,
    buffer: u64,
    capacity: usize,
}

impl Cuda {
    pub(super) fn new(table: &Tables) -> Result<(Self, String), String> {
        let api = Api::load()?;
        let mut device = 0;
        let mut ctx = ptr::null_mut();
        let mut name = [0i8; 256];
        unsafe {
            check((api.cuInit)(0), "cuInit")?;
            check((api.cuDeviceGet)(&mut device, 0), "cuDeviceGet")?;
            check(
                (api.cuDeviceGetName)(name.as_mut_ptr(), 256, device),
                "cuDeviceGetName",
            )?;
            check((api.cuCtxCreate_v2)(&mut ctx, 0, device), "cuCtxCreate")?;
        }
        let mut cuda = Self {
            api,
            ctx,
            kernel: ptr::null_mut(),
            constants: 0,
            buffer: 0,
            capacity: 0,
        };
        // cuCtxCreate pushes the new private context. Restore the caller's
        // context on both success and partial initialization failure.
        let _current = Current(cuda.api.clone());
        let api = &cuda.api;
        let ptx = std::ffi::CString::new(kernel::generate(kernel::Target::Cuda)).unwrap();
        let mut module = ptr::null_mut();
        unsafe {
            check(
                (api.cuModuleLoadData)(&mut module, ptx.as_ptr().cast()),
                "cuModuleLoadData",
            )?;
            check(
                (api.cuModuleGetFunction)(&mut cuda.kernel, module, c"jumpbot_masks".as_ptr()),
                "cuModuleGetFunction",
            )?;
            let size = std::mem::size_of_val(table);
            check(
                (api.cuMemAlloc_v2)(&mut cuda.constants, size),
                "cuMemAlloc tables",
            )?;
            check(
                (api.cuMemcpyHtoD_v2)(cuda.constants, table.as_ptr().cast(), size),
                "cuMemcpyHtoD tables",
            )?;
        }
        let name = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok((cuda, name))
    }

    pub(super) fn compute(
        &mut self,
        f: &Frame,
        bg: &[u8],
        rows: [usize; 2],
    ) -> Result<Pair, String> {
        let _current = Current::push(&self.api, self.ctx)?;
        let api = &self.api;
        let n = f.width * f.height;
        let size = f.bgr.len() + bg.len() + 2 * n;
        unsafe {
            if size > self.capacity {
                if self.buffer != 0 {
                    check((api.cuMemFree_v2)(self.buffer), "cuMemFree")?;
                    self.buffer = 0;
                    self.capacity = 0;
                }
                check(
                    (api.cuMemAlloc_v2)(&mut self.buffer, size),
                    "cuMemAlloc frame",
                )?;
                self.capacity = size;
            }
            let base = self.buffer + f.bgr.len() as u64;
            let output = base + bg.len() as u64;
            check(
                (api.cuMemcpyHtoD_v2)(self.buffer, f.bgr.as_ptr().cast(), f.bgr.len()),
                "cuMemcpyHtoD frame",
            )?;
            check(
                (api.cuMemcpyHtoD_v2)(base, bg.as_ptr().cast(), bg.len()),
                "cuMemcpyHtoD background",
            )?;
            let dims = [n as i32, f.width as i32, rows[0] as i32, rows[1] as i32];
            let arg = |p: *const u64| p.cast_mut().cast::<c_void>();
            let mut args = [
                arg(&self.buffer),
                arg(&base),
                arg(&self.constants),
                arg(&output),
                ptr::from_ref(&dims[0]).cast_mut().cast(),
                ptr::from_ref(&dims[1]).cast_mut().cast(),
                ptr::from_ref(&dims[2]).cast_mut().cast(),
                ptr::from_ref(&dims[3]).cast_mut().cast(),
            ];
            check(
                (api.cuLaunchKernel)(
                    self.kernel,
                    n.div_ceil(256) as u32,
                    1,
                    1,
                    256,
                    1,
                    1,
                    0,
                    ptr::null_mut(),
                    args.as_mut_ptr(),
                    ptr::null_mut(),
                ),
                "cuLaunchKernel",
            )?;
            let mut pair = (vec![0; n], vec![0; n]);
            check(
                (api.cuMemcpyDtoH_v2)(pair.0.as_mut_ptr().cast(), output, n),
                "cuMemcpyDtoH piece",
            )?;
            check(
                (api.cuMemcpyDtoH_v2)(pair.1.as_mut_ptr().cast(), output + n as u64, n),
                "cuMemcpyDtoH block",
            )?;
            Ok(pair)
        }
    }
}

impl Drop for Cuda {
    fn drop(&mut self) {
        // The context owns its module and all allocations, including on device
        // errors. No stale module or allocation handles are used during teardown.
        unsafe {
            (self.api.cuCtxDestroy_v2)(self.ctx);
        }
    }
}

#[cfg(all(test, feature = "cuda"))]
mod tests {
    use super::*;
    use crate::{masks::Masks, Config};

    #[test]
    #[ignore = "requires an NVIDIA GPU and a working CUDA driver"]
    fn failed_copy_recomputes_the_changed_frame() {
        let cfg = Config::default();
        let mut masks = Masks::new(&cfg, "cuda");
        let mut frame = Frame {
            width: 37,
            height: 47,
            bgr: (0..37 * 47 * 3).map(|i| (i * 43 + i / 13) as u8).collect(),
        };
        let previous = masks.compute(&frame).unwrap();
        assert_eq!(masks.backend(), "cuda");
        // Request a non-empty upload to device address zero. The real driver
        // rejects it. The private context still owns the original allocation.
        let device = &mut masks.gpu.devices.as_mut().unwrap()[0];
        let Runtime::Cuda(cuda) = &mut device.runtime else {
            unreachable!()
        };
        cuda.buffer = 0;
        frame.bgr.reverse();
        let expected = Masks::new(&cfg, "cpu").compute(&frame).unwrap();
        assert_ne!(previous, expected);
        assert_eq!(masks.compute(&frame).unwrap(), expected);
        assert!(matches!(masks.backend(), "simd" | "cpu"));
        assert_eq!(masks.compute(&frame).unwrap(), expected);
    }
}
