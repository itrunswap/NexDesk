# NexDesk

NexDesk 是一款支持局域网 IP 直连和设备 ID 连接的跨平台远程桌面。桌面端可以互控，浏览器、手机和平板可以通过自托管服务控制电脑，独立 CLI 可以在终端显示和操控远程桌面。

## 下载

从 [GitHub Releases](https://github.com/itrunswap/NexDesk/releases/latest) 选择对应平台的资产。`SHA256SUMS.txt` 列出各包的 SHA-256 校验值。

| 平台 | 发行资产 | 使用方式 |
| --- | --- | --- |
| Windows x64 | [v0.1.6 Windows ZIP](https://github.com/itrunswap/NexDesk/releases/download/v0.1.6/NexDesk-0.1.6-Desktop-Windows-x64.zip) | 完整解压后运行 `nexdesk_desktop.exe` |
| macOS Apple Silicon / Intel | [v0.1.6 桌面修复版](https://github.com/itrunswap/NexDesk/releases/tag/v0.1.6) | 选择 `Desktop-macOS-arm64.zip` 或 `Desktop-macOS-x64.zip`，解压后打开 `NexDesk.app`。预览签名见[运行与打包](docs/build.md#macos) |
| Linux x64 | `Desktop-Linux-x64.tar.gz` | 执行 `tar -xzf <包名>`，在解压目录运行 `./nexdesk_desktop` |
| Android ARM64 | `Android-ARM64-Preview.apk` | 安装预览 APK，填写服务地址和目标设备 ID |
| iOS / iPadOS | 浏览器或自签名应用 | 在 Safari 打开服务地址。原生包使用自己的开发团队签名 |
| 浏览器 | HTTPS 首页 | 打开默认服务或自托管地址，输入被控设备 ID 和密码 |
| 终端 | 桌面包或独立 CLI | 执行 `nexdesk-cli view`，也可独立构建或加载 Pi 扩展 |
| 自托管 | `SelfHost-Linux-x64.tar.gz` / `SelfHost-Windows-x64.zip` | 目录服务与 Web 界面随包提供，公网使用 HTTPS |

## 连接一台电脑

在被控电脑打开 NexDesk。默认服务地址是 [desk.zoukaixin.cn](https://desk.zoukaixin.cn)。在「设置 → 连接」中可以改为自托管服务。

- **同一局域网**。两台电脑选择「IP 直连」。主控输入被控端显示的 IP、端口和对应密码，端口省略时使用 `53980`。这条路径独立于设备目录。
- **设备 ID**。两台电脑使用同一目录服务。主控输入被控端展示的九位 ID 和对应密码。浏览器或移动端打开同一个 HTTPS 服务，也可按 ID 连接。
- **桌面连接**。输入密码即可发起连接。应用会记录通过验证的设备身份，近期连接可直接使用已保存的密码。
- **被控授权**。在被控电脑的「设置 → 共享 → 接入方式」选择「每次确认」「可信设备免确认」或「密码验证后连接」。「设置 → 权限」可配置来访能获得的权限。

## 运行、构建与部署

| 指南 | 内容 |
| --- | --- |
| [各端运行与打包](docs/build.md) | Windows、macOS、Linux、Android、iOS、Web、CLI 和 Pi 的工具链与命令 |
| [自托管部署](docs/deployment.md) | 本机运行、Linux Docker Compose、Nginx 与 systemd、HTTPS、TURN、备份 |
| [技术架构](docs/architecture.md) | Rust 核心、Flutter 和 WebRTC、连接协议、权限及存储 |
| [功能清单](docs/features.md) · [桌面交互](docs/desktop-design.md) | 跨端能力、首页、会话操作及文件传输 |
| [工程规范](docs/engineering-standards.md) · [开发路线](docs/roadmap.md) | 代码规范、模块边界及里程碑 |
| [ID / 终端使用手册](docs/m4-m5.md) · [维护手册](docs/m6-m7.md) | 终端操作、移动端、设备授权和发布维护 |
