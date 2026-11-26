# 各端运行与打包

在仓库根目录运行各段命令，或按示例切换到对应子目录。`0.1.10` 的各平台发行包使用 `v0.1.10` 标签对应的源码。桌面和移动版本取自 `apps/desktop/pubspec.yaml`，目录协议版本通过 `/healthz` 查看。[v0.1.10 发行包](https://github.com/itrunswap/NexDesk/releases/tag/v0.1.10)

| 工具 | 版本与用途 |
| --- | --- |
| Rust / Cargo | `1.96.0`，编译原生代理、FFI、目录及 CLI |
| Flutter / Dart | Flutter `3.47.0` 携带 Dart `3.13.0`，编译桌面和移动客户端 |
| Node.js / npm | Node.js `24`，构建浏览器端和 Pi 扩展 |
| WASM 工具 | Clang、`wasm32-unknown-unknown` Rust 目标、`wasm-bindgen-cli 0.2.130` |
| Python | `3.12` 或以上，生成第三方声明、便携发行包和数据库快照 |

桌面端在对应操作系统上构建。先执行 `flutter doctor -v` 检查平台工具链。客户端源码默认使用 `https://desk.zoukaixin.cn`。自行运营时，在 Flutter 构建或运行命令追加 `--dart-define=NEXDESK_DEFAULT_DIRECTORY=https://desk.example.com`。桌面用户也可以通过设置改用自己的服务。[Flutter 桌面环境](https://docs.flutter.dev/platform-integration/desktop)

## Windows

安装 Git、Rust MSVC 工具链、Flutter、Visual Studio 的“使用 C++ 的桌面开发”工作负载及 Windows SDK。使用 x64 开发者 PowerShell，原生媒体 SDK 由脚本下载并校验。

```powershell
rustup override set 1.96.0
$env:LK_CUSTOM_WEBRTC = & .\tools\m1\prepare-windows.ps1
cargo build --locked --release -p nexdesk-agent -p nexdesk-ffi -p nexdesk-cli
$native = (Resolve-Path target\release).Path

Push-Location apps\desktop
flutter pub get --enforce-lockfile
flutter build windows --release --no-pub
$bundle = 'build\windows\x64\runner\Release'
Copy-Item -LiteralPath @(
    "$native\nexdesk-agent.exe",
    "$native\nexdesk_ffi.dll",
    "$native\nexdesk-cli.exe"
) -Destination $bundle
Pop-Location
& .\apps\desktop\build\windows\x64\runner\Release\nexdesk_desktop.exe
```

分发 `apps/desktop/build/windows/x64/runner/Release` 内的全部文件。以下命令生成 ZIP，保留 Flutter 资源目录结构。

```powershell
Compress-Archive -Path 'apps\desktop\build\windows\x64\runner\Release\*' -DestinationPath 'target\NexDesk-Desktop-Windows-x64.zip'
Get-FileHash 'target\NexDesk-Desktop-Windows-x64.zip' -Algorithm SHA256
```

开发时可从 `apps/desktop` 运行 `flutter run -d windows`。若把 Rust 文件留在 Cargo 输出目录，同时传入 `--dart-define=NEXDESK_CORE_LIBRARY=<DLL 完整路径>` 和 `--dart-define=NEXDESK_AGENT_PATH=<代理完整路径>`。

## macOS

安装完整的 Xcode、命令行工具、Rust 和 Flutter。在目标架构的 Mac 上运行下列命令。Apple Silicon 选 `arm64`，Intel 选 `x64`。同一脚本也在 GitHub Actions 的两种原生 runner 上运行。

```sh
rustup override set 1.96.0
sudo xcodebuild -runFirstLaunch
bash tools/release/package-macos.sh
```

输出在 `target/release-<版本号>/`，包括 `NexDesk.app` 的 ZIP 和独立 SHA-256 文件。首次作为被控端使用时，在「系统设置 → 隐私与安全性」授予屏幕录制和辅助功能权限。GitHub 预览包使用 ad hoc 签名，Gatekeeper 可能要求到「隐私与安全性」确认打开。用于正式分发时设置 `NEXDESK_MACOS_SIGN_IDENTITY` 和 `NEXDESK_NOTARY_PROFILE` 再执行同一脚本，完成 Developer ID 签名、公证和 stapling。[Apple 的应用打开说明](https://support.apple.com/102445)

GitHub Actions 使用发行标签中的应用源码和本次工作流所在提交的打包脚本。发布前在签名后的包内启动接收程序，验证原生媒体初始化、临时钥匙串中的凭据写入和重启后设备身份恢复。

## Linux

在 x86_64 Linux 上安装 GTK 3、Ninja、CMake、pkg-config、C/C++ 工具链、X11 和 PulseAudio 开发库。以下包名适用于 Ubuntu。在构建前从发行版或 LLVM 软件源安装可用的 Clang 21 或更新版本。预构建 WebRTC SDK 需要支持 Chromium libc++ 头文件的编译器。

```sh
sudo apt-get install build-essential cmake ninja-build pkg-config libgtk-3-dev \
    libpulse-dev libx11-dev libxext-dev libxrandr-dev libxfixes-dev libxtst-dev
rustup override set 1.96.0
export CC=clang-21 CXX=clang++-21
cargo build --locked --release -p nexdesk-agent -p nexdesk-ffi -p nexdesk-cli
(
    cd apps/desktop
    flutter pub get --enforce-lockfile
    flutter build linux --release --no-pub
)
bundle=apps/desktop/build/linux/x64/release/bundle
install -m 755 target/release/nexdesk-agent target/release/libnexdesk_ffi.so \
    target/release/nexdesk-cli "$bundle/"
tar -czf target/NexDesk-Desktop-Linux-x64.tar.gz -C "$bundle" \
    nexdesk_desktop nexdesk-agent nexdesk-cli libnexdesk_ffi.so lib data
```

在图形用户会话中运行 `"$bundle/nexdesk_desktop"`。发行包需要 GTK 3、PulseAudio 客户端库和 X11 运行库。在 Ubuntu 可按发行版安装 `libgtk-3-0t64`（旧版为 `libgtk-3-0`）、`libpulse0`、`libx11-6`、`libxext6`、`libxrandr2` 和 `libxtst6`。用 `ldd ./nexdesk_desktop` 和 `ldd ./nexdesk-cli` 核对缺少的动态库。PulseAudio 或 PipeWire 的 PulseAudio 兼容服务提供系统声音。准备打包到更旧的发行版时，在相同或更早的系统上构建并校验 glibc 与 GTK 依赖。

## Android

安装 Flutter、JDK 21、Android SDK 平台和构建工具。调试时可以在 `apps/desktop` 执行 `flutter run -d <设备编号>`。发布 APK 使用自己的 keystore，在仓库外创建签名属性文件，包含 `storeFile`、`storePassword`、`keyAlias`、`keyPassword`。`storeFile` 使用绝对路径。

```powershell
$env:NEXDESK_ANDROID_SIGNING_PROPERTIES = 'C:\secure\nexdesk-signing.properties'
Set-Location apps\desktop
flutter pub get --enforce-lockfile
flutter build apk --release --no-pub --target-platform android-arm64
flutter build appbundle --release --no-pub
```

本地分发使用 `build/app/outputs/flutter-apk/app-release.apk`。应用商店可使用 `build/app/outputs/bundle/release/app-release.aab`。覆盖安装必须使用同一签名密钥。移动端用于连接并控制电脑。

## iOS / iPadOS

在 Mac 上为 Runner 配置 Apple 开发团队、应用标识和分发描述文件。用 Xcode 查看所选证书、团队和部署目标。发布渠道要求的导出选项可通过 `--export-options-plist` 提供。

```sh
cd apps/desktop
flutter pub get --enforce-lockfile
flutter run -d <设备编号>
flutter build ipa --release
```

签名归档和分发文件分别出现在 `build/ios/archive/` 和 `build/ios/ipa/`。需要直接使用时可在 Safari 打开自托管服务的 HTTPS 首页。

## 浏览器与目录服务

在 Linux、macOS 或 WSL 中使用仓库内的构建脚本，生成 WebRTC 前端资源和原生目录程序。

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.130
bash tools/m4/build-web.sh
cargo build --locked --release -p nexdesk-signaling
./target/release/nexdesk-signaling
```

打开 `http://127.0.0.1:54000` 调试。Windows PowerShell 可运行 `.\tools\m4\build-web.ps1`，若 WASM 已在 WSL 构建，传入 `-WasmBinary <文件路径>`。Web 输出在 `apps/web/dist`。目录二进制与同版本 Web 输出可通过下列命令打包为自托管资产，Windows 上将文件名换成 `nexdesk-signaling.exe`。

```sh
python3 tools/release/package-server.py target/release/nexdesk-signaling
```

`package-server.py` 把 Vite 输出、第三方声明和部署样例一起打包。通过[自托管部署](deployment.md)配置 HTTPS 和 TURN，将页面和目录接口挂在同一来源。

## CLI 与 Pi 扩展

在依赖项目原生 WebRTC SDK 的桌面构建环境下执行以下命令。编译后把 `nexdesk-cli` 放在 PATH 或使用完整路径。Windows 文件名为 `nexdesk-cli.exe`。

```sh
cargo build --locked --release -p nexdesk-cli
./target/release/nexdesk-cli check-id 012345678
./target/release/nexdesk-cli view https://desk.example.com 012345678 --control --render auto
```

Pi 扩展用 `pi -e "$PWD/extensions/pi/index.ts"` 加载。扩展通过 PATH 或 `NEXDESK_CLI` 调用 CLI。在 Pi 内输入 `/nexdesk` 后选择连接。单独检查扩展的 TypeScript 可在 `extensions/pi` 执行 `npm ci` 和 `npm run check`。会话快捷键和本地命令见[终端使用手册](m4-m5.md#独立终端主控)。
