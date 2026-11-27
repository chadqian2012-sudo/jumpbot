# Third-party notices

The Windows packages contain the Rust CLI and desktop application. Both editions use the native OpenCV world library for image processing. The full edition enables the CUDA and OpenCL backends. The portable edition uses the CPU.

- OpenCV and the integer BGR-to-HSV conversion adapted from OpenCV's `RGB2HSV_b` use Apache-2.0. See [the upstream implementation](https://github.com/opencv/opencv/blob/4.14.0/modules/imgproc/src/color_hsv.simd.hpp), `LICENSE-OpenCV.txt`, and the SDK notices in `licenses/OpenCV/`.
- The desktop UI embeds Saira. Its SIL Open Font License and copyright notice are in `licenses/Saira-OFL.txt` in the package and `desktop/ui/Saira-OFL.txt` in the source tree.
- The Rust standard library notices are included in `licenses/rust-standard-library.html`. The package lists Cargo dependencies, versions and declared licenses in `licenses/cargo-dependencies.txt`. License texts supplied with these crates are in the corresponding package directories.
- The native desktop window uses Windows system APIs. Microsoft Visual C++ redistributable terms apply to the app-local runtime DLLs. [Visual Studio licensing](https://visualstudio.microsoft.com/license-terms/).
