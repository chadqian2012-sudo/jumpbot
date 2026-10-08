# Third-party notices

The integer BGR-to-HSV conversions in `Vision._masks_gpu` and the OpenCL
kernel in `Vision._init_opencl` are adapted from the
OpenCV project's `RGB2HSV_b` implementation in
`modules/imgproc/src/color_hsv.simd.hpp` (OpenCV 4.14.0).

Source: https://github.com/opencv/opencv/blob/4.14.0/modules/imgproc/src/color_hsv.simd.hpp

OpenCV is distributed under the Apache License, Version 2.0. A copy is included
in `LICENSE-OpenCV.txt`. The adapted implementation combines the conversion
with jumpbot's CUDA mask thresholds and uses persistent lookup tables and
device buffers. OpenCV's original integer lookup and rounding rules are retained.

Windows software deliveries also include a `licenses/` directory containing
the license files supplied by the installed runtime distributions (Python,
NumPy, OpenCV, MSS, PyOpenCL/PyTools and the PyInstaller bootloader terms).
Full builds additionally include the CuPy and bundled NVIDIA CUDA runtime,
NVRTC and nvJitLink license texts. Other dependency license files may remain
under `_internal/` in their original distribution directories.

These notices do not replace the vendor terms. The portable build omits the
CuPy/CUDA libraries; OpenCL uses the device driver's runtime.
