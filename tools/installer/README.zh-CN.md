# 跳一跳控制台安装指南

安装包适用于 64 位 Windows。首次安装可选空目录，升级沿用原安装目录。程序必须有 OpenCV，默认安装内置的 x64 DLL。CPU 和 Visual C++ 运行库必装。CUDA 和 OpenCL 可分别勾选。两项都选时，主程序在可用设备中自动选择。缺少合适的显卡驱动时使用 CPU。用户安装时只需要一个 EXE。

勾选 CUDA 或 OpenCL 会改变实际安装的 `jumpbot.exe`。控制台、命令行和开始菜单使用同一个工作程序。CPU 入口始终可用。显卡入口只随相应组件安装，桌面快捷方式可单独取消。重装可调整组件。配置和自定义模板在升级、卸载及原目录重装时保留。

静默卸载可在原安装目录运行下列命令。先关闭控制台和脚本，等待命令返回后再重装。若想沿用配置和模板，重装时选择原目录。

```powershell
$p = Start-Process -FilePath '.\unins000.exe' -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG="uninstall.log"' -Wait -PassThru
$p.ExitCode
```

## OpenCV 默认可离线安装，也可选择本机文件

在“OpenCV 运行库”页保留默认选项即可。高级用户可以浏览本地磁盘选择 DLL，或将文件框留空，复用原安装目录中已有的 DLL。安装器将所选文件复制到临时目录，核对与本包内置 DLL 完全相同的 SHA-256，再将副本安装到工作程序旁。外部原件留在原处，系统 PATH 保持原样。整个安装步骤使用 Inno 自带的复制和校验函数。选错文件时可以返回内置选项。

静默安装省略 `/OPENCV` 或指定 `/OPENCV=bundled` 使用内置文件。`/OPENCV=installed` 复用原安装目录中的 DLL，也可指定完整本地路径。无效来源会在写入程序目录前报错并返回非零退出码。以下命令在生成安装包后使用。

```powershell
$setup = '.\build\offline-delivery\jumpbot-setup-1.7.2-offline.exe'
$process = Start-Process $setup -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /OPENCV="D:\本机 OpenCV\opencv_world4140.dll" /LOG="opencv-install.log"' -Wait -PassThru
$process.ExitCode
```

安装日志的 `OpenCV file entry: bundled` 或 `OpenCV file entry: local` 标明实际使用的文件条目。本机来源另记所选绝对路径。下方实装验收会逐项检查这些记录，并尝试缺失文件和长度相同但哈希不符的文件。

## 先检查源码和构建工具

在候选工作树根目录打开 PowerShell。本机可复用如下离线环境。其他机器将路径替换为对应的本机工具和缓存即可。构建机需要 Rust MSVC、C++ 构建工具及其可再分发运行库、OpenCV x64 SDK、LLVM 和 Inno Setup 6。用户机器无需安装这些开发工具。

```powershell
$toolchain = (Resolve-Path '../jumpbot-rust-desktop/build/rust-toolchain').Path
$env:CARGO_HOME = Join-Path $toolchain 'cargo'
$env:RUSTUP_HOME = Join-Path $toolchain 'rustup'
$compiler = Join-Path $env:RUSTUP_HOME 'toolchains/stable-x86_64-pc-windows-msvc/bin'
$env:JUMPBOT_OPENCV_DIR = (Resolve-Path '../jumpbot-rust-desktop/build/cv-sdk').Path
$env:LIBCLANG_PATH = (Resolve-Path '../jumpbot-all-rust/build/bindgen/llvm20/bin').Path
$env:PATH = "$compiler;$env:LIBCLANG_PATH;$env:LOCALAPPDATA\Programs\Inno Setup 6;$env:PATH"
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'build/target'
cargo test --offline --locked --release --manifest-path desktop/Cargo.toml -p jumpbot-dist
cargo build --offline --locked --release --manifest-path desktop/Cargo.toml -p jumpbot-dist
```

## 冻结源码后生成一份安装 EXE

在确认的源码版本中运行下面的命令。输出目录须尚不存在。打包工具记录 Git 提交、源码和文件校验值，并在构建结束时再次检查源码。

```powershell
$dist = Join-Path $env:CARGO_TARGET_DIR 'release/jumpbot-dist.exe'
& $dist --offline-installer --version 1.7.2 --output build/offline-delivery
```

生成的安装文件为 `build/offline-delivery/jumpbot-setup-1.7.2-offline.exe`。同目录的 `SHA256SUMS.txt` 可用于校验。原有 full 和 portable 发布命令继续使用原来的参数。

## 在空闲测试账户验证安装、升级和卸载

下面的命令先检查交付文件的 SHA-256，再用生成的 EXE 静默安装到带空格的中文目录。它逐项核对 CPU、CUDA、OpenCL、双加速的实际工作程序、快捷方式和同一张图片的识别结果，并在屏蔽显卡后检查 CPU 回退。随后修改过的配置和模板会经历覆盖安装、卸载及重装。详细日志位于本工作树 `build/offline-install-proof-*`。

```powershell
& $dist --offline-installer --verify --install --version 1.7.2 --output build/offline-delivery
```

本安装程序采用固定的独立安装身份 `{AA105DEB-419A-4E5B-A840-4F7E2332A61C}`，默认位于 `%LOCALAPPDATA%\Programs\jumpbot-offline-preview`。已用此安装程序安装过控制台时，请换一个空闲账户运行自动实装验证。手动卸载可用 Windows “已安装的应用”里的“跳一跳控制台”。
