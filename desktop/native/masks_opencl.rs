use super::*;
use std::{ffi::CStr, mem::size_of};

driver_api! {"OpenCL.dll";
    fn clGetPlatformIDs(u32,*mut Handle,*mut u32)->i32;
    fn clGetDeviceIDs(Handle,u64,u32,*mut Handle,*mut u32)->i32;
    fn clGetDeviceInfo(Handle,u32,usize,*mut c_void,*mut usize)->i32;
    fn clCreateContext(*const isize,u32,*const Handle,Handle,Handle,*mut i32)->Handle;
    fn clCreateCommandQueue(Handle,Handle,u64,*mut i32)->Handle;
    fn clCreateProgramWithSource(Handle,u32,*const *const c_char,*const usize,*mut i32)->Handle;
    fn clBuildProgram(Handle,u32,*const Handle,*const c_char,Handle,Handle)->i32;
    fn clGetProgramBuildInfo(Handle,Handle,u32,usize,*mut c_void,*mut usize)->i32;
    fn clCreateKernel(Handle,*const c_char,*mut i32)->Handle;
    fn clCreateBuffer(Handle,u64,usize,*mut c_void,*mut i32)->Handle;
    fn clSetKernelArg(Handle,u32,usize,*const c_void)->i32;
    fn clEnqueueWriteBuffer(Handle,Handle,u32,usize,usize,*const c_void,u32,*const Handle,*mut Handle)->i32;
    fn clEnqueueNDRangeKernel(Handle,Handle,u32,*const usize,*const usize,*const usize,u32,*const Handle,*mut Handle)->i32;
    fn clEnqueueReadBuffer(Handle,Handle,u32,usize,usize,*mut c_void,u32,*const Handle,*mut Handle)->i32;
    fn clReleaseMemObject(Handle)->i32;
    fn clReleaseKernel(Handle)->i32;
    fn clReleaseProgram(Handle)->i32;
    fn clReleaseCommandQueue(Handle)->i32;
    fn clReleaseContext(Handle)->i32;
}

pub(super) struct Candidate {
    api: Rc<Api>,
    id: Handle,
    pub(super) name: String,
}

fn handles(
    mut enumerate: impl FnMut(u32, *mut Handle, *mut u32) -> i32,
) -> Result<Vec<Handle>, String> {
    let mut count = 0;
    check(
        enumerate(0, ptr::null_mut(), &mut count),
        "OpenCL enumeration",
    )?;
    let mut ids = vec![ptr::null_mut(); count as usize];
    if count != 0 {
        check(
            enumerate(count, ids.as_mut_ptr(), ptr::null_mut()),
            "OpenCL enumeration",
        )?;
    }
    Ok(ids)
}

fn driver_string(
    mut query: impl FnMut(usize, *mut c_void, *mut usize) -> i32,
) -> Result<String, String> {
    let mut size = 0;
    check(query(0, ptr::null_mut(), &mut size), "OpenCL info size")?;
    let mut bytes = vec![0; size];
    check(
        query(size, bytes.as_mut_ptr().cast(), ptr::null_mut()),
        "OpenCL info",
    )?;
    CStr::from_bytes_until_nul(&bytes)
        .map(|s| s.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

pub(super) fn candidates(selector: &str) -> Result<Vec<Candidate>, String> {
    let selector = selector.trim().to_lowercase();
    let api = Api::load()?;
    let platforms = handles(|n, p, count| unsafe { (api.clGetPlatformIDs)(n, p, count) })?;
    let mut list = Vec::new();
    for platform in platforms {
        let Ok(devices) =
            handles(|n, p, count| unsafe { (api.clGetDeviceIDs)(platform, 4, n, p, count) })
        else {
            continue;
        };
        for id in devices {
            let string = |key| {
                driver_string(|n, p, count| unsafe { (api.clGetDeviceInfo)(id, key, n, p, count) })
            };
            let enabled = |key| {
                let mut yes = 0u32;
                unsafe {
                    (api.clGetDeviceInfo)(
                        id,
                        key,
                        4,
                        ptr::from_mut(&mut yes).cast(),
                        ptr::null_mut(),
                    ) == 0
                        && yes != 0
                }
            };
            if !enabled(0x1027) || !enabled(0x1028) {
                continue;
            } // device available and compiler available
            let (Ok(vendor), Ok(name), Ok(version)) =
                (string(0x102c), string(0x102b), string(0x102f))
            else {
                continue;
            };
            let name = format!("{vendor} {name}");
            if version.starts_with("OpenCL 1.0")
                || version.starts_with("OpenCL 1.1")
                || !name.to_lowercase().contains(&selector)
            {
                continue;
            }
            list.push(Candidate {
                api: api.clone(),
                id,
                name,
            });
        }
    }
    Ok(list)
}

pub(super) struct Opencl {
    api: Rc<Api>,
    context: Handle,
    queue: Handle,
    program: Handle,
    kernel: Handle,
    constants: Handle,
    // BGR, row backgrounds and packed outputs have independent capacity requirements.
    buffers: [Handle; 3],
    capacities: [usize; 3],
}

impl Candidate {
    pub(super) fn open(self, table: &Tables) -> Result<Opencl, String> {
        let null = ptr::null_mut();
        let mut cl = Opencl {
            api: self.api,
            context: null,
            queue: null,
            program: null,
            kernel: null,
            constants: null,
            buffers: [null; 3],
            capacities: [0; 3],
        };
        let api = &cl.api;
        let mut rc = 0;
        unsafe {
            cl.context = (api.clCreateContext)(ptr::null(), 1, &self.id, null, null, &mut rc);
            check(rc, "clCreateContext")?;
            cl.queue = (api.clCreateCommandQueue)(cl.context, self.id, 0, &mut rc);
            check(rc, "clCreateCommandQueue")?;
            let src = kernel::generate(kernel::Target::OpenCl);
            cl.program = (api.clCreateProgramWithSource)(
                cl.context,
                1,
                &src.as_ptr().cast(),
                &src.len(),
                &mut rc,
            );
            check(rc, "clCreateProgramWithSource")?;
            rc = (api.clBuildProgram)(
                cl.program,
                1,
                &self.id,
                c"-cl-std=CL1.2".as_ptr(),
                null,
                null,
            );
            if rc != 0 {
                let log = driver_string(|n, p, size| {
                    (api.clGetProgramBuildInfo)(cl.program, self.id, 0x1183, n, p, size)
                });
                return Err(format!(
                    "clBuildProgram returned {rc}: {}",
                    log.unwrap_or_default()
                ));
            }
            cl.kernel = (api.clCreateKernel)(cl.program, c"jumpbot_masks".as_ptr(), &mut rc);
            check(rc, "clCreateKernel")?;
            cl.constants = (api.clCreateBuffer)(
                cl.context,
                4 | 32,
                std::mem::size_of_val(table),
                table.as_ptr().cast_mut().cast(),
                &mut rc,
            );
            check(rc, "clCreateBuffer tables")?;
        }
        Ok(cl)
    }
}

impl Opencl {
    pub(super) fn compute(
        &mut self,
        f: &Frame,
        bg: &[u8],
        rows: [usize; 2],
    ) -> Result<Pair, String> {
        let api = &self.api;
        let n = f.width * f.height;
        let sizes = [f.bgr.len(), bg.len(), 2 * n];
        unsafe {
            for (i, size) in sizes.into_iter().enumerate() {
                if size > self.capacities[i] {
                    let mut rc = 0;
                    let flags = if i == 2 { 2 } else { 4 };
                    let buffer =
                        (api.clCreateBuffer)(self.context, flags, size, ptr::null_mut(), &mut rc);
                    check(rc, "clCreateBuffer frame")?;
                    if !self.buffers[i].is_null() {
                        (api.clReleaseMemObject)(self.buffers[i]);
                    }
                    self.buffers[i] = buffer;
                    self.capacities[i] = size;
                }
            }
            let [bgr, base, out] = self.buffers;
            // Blocking transfers also keep Rust input lifetimes valid on every error path.
            for (buffer, input) in [(bgr, f.bgr.as_slice()), (base, bg)] {
                check(
                    (api.clEnqueueWriteBuffer)(
                        self.queue,
                        buffer,
                        1,
                        0,
                        input.len(),
                        input.as_ptr().cast(),
                        0,
                        ptr::null(),
                        ptr::null_mut(),
                    ),
                    "clEnqueueWriteBuffer",
                )?;
            }
            for (i, buffer) in [bgr, base, self.constants, out].iter().enumerate() {
                check(
                    (api.clSetKernelArg)(
                        self.kernel,
                        i as u32,
                        size_of::<Handle>(),
                        ptr::from_ref(buffer).cast(),
                    ),
                    "clSetKernelArg buffer",
                )?;
            }
            for (i, arg) in [n as i32, f.width as i32, rows[0] as i32, rows[1] as i32]
                .iter()
                .enumerate()
            {
                check(
                    (api.clSetKernelArg)(self.kernel, 4 + i as u32, 4, ptr::from_ref(arg).cast()),
                    "clSetKernelArg dimension",
                )?;
            }
            check(
                (api.clEnqueueNDRangeKernel)(
                    self.queue,
                    self.kernel,
                    1,
                    ptr::null(),
                    &n,
                    ptr::null(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                ),
                "clEnqueueNDRangeKernel",
            )?;
            let mut pair = (vec![0; n], vec![0; n]);
            for (offset, mask) in [(0, &mut pair.0), (n, &mut pair.1)] {
                check(
                    (api.clEnqueueReadBuffer)(
                        self.queue,
                        out,
                        1,
                        offset,
                        n,
                        mask.as_mut_ptr().cast(),
                        0,
                        ptr::null(),
                        ptr::null_mut(),
                    ),
                    "clEnqueueReadBuffer",
                )?;
            }
            Ok(pair)
        }
    }
}
impl Drop for Opencl {
    fn drop(&mut self) {
        unsafe {
            for buf in self.buffers.into_iter().chain([self.constants]) {
                if !buf.is_null() {
                    (self.api.clReleaseMemObject)(buf);
                }
            }
            if !self.kernel.is_null() {
                (self.api.clReleaseKernel)(self.kernel);
            }
            if !self.program.is_null() {
                (self.api.clReleaseProgram)(self.program);
            }
            if !self.queue.is_null() {
                (self.api.clReleaseCommandQueue)(self.queue);
            }
            if !self.context.is_null() {
                (self.api.clReleaseContext)(self.context);
            }
        }
    }
}

#[cfg(all(test, feature = "opencl"))]
mod tests {
    use super::*;
    use crate::{masks::Masks, Config};

    #[test]
    #[ignore = "requires an OpenCL GPU and a working vendor driver"]
    fn rejected_upload_recomputes_the_current_frame() {
        let cfg = Config::default();
        let mut scalar = Masks::new(&cfg, "cpu");
        let frame = |w| Frame {
            width: w,
            height: 23,
            bgr: (0..w * 23 * 3).map(|i| (i * 43 + i / 13) as u8).collect(),
        };
        let mut adapters = candidates("").unwrap();
        // Two independent contexts exercise failover even on a single-GPU host.
        if adapters.len() == 1 {
            adapters.extend(candidates("").unwrap());
        }
        let count = adapters.len();
        assert!(count >= 2);
        let mut masks = Masks::new(&cfg, "opencl");
        let small = frame(37);
        let large = frame(71);
        // The unit-test binary cannot serve as the CLI preflight child.
        masks.gpu.devices = Some(
            adapters
                .into_iter()
                .map(|adapter| Device {
                    name: adapter.name.clone(),
                    runtime: Runtime::Opencl(adapter.open(&masks.table).unwrap()),
                    ms: 0.,
                })
                .collect(),
        );
        masks.select(&small, [0, small.height]);
        assert_eq!(masks.gpu.devices.as_ref().unwrap().len(), count);
        for remaining in (0..count).rev() {
            let i = masks.gpu.chosen.expect("a validated OpenCL device remains");
            let name = masks.gpu.devices.as_ref().unwrap()[i].name.clone();
            assert_eq!(
                masks.compute(&small).unwrap(),
                scalar.compute(&small).unwrap()
            );
            assert_eq!(masks.backend(), "opencl", "{name}");

            let Runtime::Opencl(cl) = &mut masks.gpu.devices.as_mut().unwrap()[i].runtime else {
                unreachable!()
            };
            // A CL_MEM_HOST_NO_ACCESS buffer is valid device memory, but the
            // driver must reject an upload to it with CL_INVALID_OPERATION.
            let mut rc = 0;
            let device_only = unsafe {
                (cl.api.clCreateBuffer)(
                    cl.context,
                    4 | (1 << 9),
                    large.bgr.len(),
                    ptr::null_mut(),
                    &mut rc,
                )
            };
            check(rc, "create device-only test buffer").unwrap();
            let old = std::mem::replace(&mut cl.buffers[0], device_only);
            check(
                unsafe { (cl.api.clReleaseMemObject)(old) },
                "release old test buffer",
            )
            .unwrap();
            cl.capacities[0] = large.bgr.len();
            assert_eq!(
                masks.compute(&large).unwrap(),
                scalar.compute(&large).unwrap(),
                "{name}"
            );
            assert!(matches!(masks.backend(), "simd" | "cpu"), "{name}");
            assert_eq!(masks.gpu.devices.as_ref().unwrap().len(), remaining);
        }
        assert_eq!(
            masks.compute(&small).unwrap(),
            scalar.compute(&small).unwrap()
        );
        assert!(matches!(masks.backend(), "simd" | "cpu"));
    }
}
