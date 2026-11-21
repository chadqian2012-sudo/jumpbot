# Third-party notices

The Windows packages contain the Rust CLI and desktop application. Both editions use the native OpenCV world library for image processing. The full edition enables the CUDA and OpenCL backends. The portable edition uses the CPU.

- OpenCV and the integer BGR-to-HSV conversion adapted from OpenCV's `RGB2HSV_b` use Apache-2.0. See [the upstream implementation](https://github.com/opencv/opencv/blob/4.14.0/modules/imgproc/src/color_hsv.simd.hpp), `LICENSE-OpenCV.txt`, and the SDK notices in `licenses/OpenCV/`.
- The desktop UI embeds Saira. Its SIL Open Font License and copyright notice are in `licenses/Saira-OFL.txt` in the package and `desktop/ui/Saira-OFL.txt` in the source tree.
- The Rust standard library notices are included in `licenses/rust-standard-library.html`. The package lists Cargo dependencies, versions and declared licenses in `licenses/cargo-dependencies.txt`. License texts supplied with these crates are in the corresponding package directories. The WebView2 Rust bindings omit their repository-level MIT license from the crates. The text is reproduced below.
- The desktop window uses Microsoft Edge WebView2 Runtime installed on the machine. The WebView2 SDK loader and the app-local Microsoft Visual C++ runtime are Microsoft components. [WebView2 SDK terms](https://www.nuget.org/packages/Microsoft.Web.WebView2/1.0.3800.47/License) · [Visual Studio licensing](https://visualstudio.microsoft.com/license-terms/).

## WebView2 Rust bindings

[webview2-rs license](https://github.com/wravery/webview2-rs/blob/edc2caf886175ccaebe86078c9cfe1ae2a187328/LICENSE)

MIT License

Copyright (c) 2021 Bill Avery

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
