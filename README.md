# typ-edit

基于 Rust、eframe/egui 与 [Typst](https://typst.app/) 0.15 的桌面编辑器，面向长文档写作与 Typst 源码编辑。Windows 优先；macOS（Apple Silicon / Intel）支持原生构建与经 osxcross 的交叉构建，并在打包时生成带 Finder 拖拽安装背景的 DMG；Linux 保持代码级兼容并提供交叉编译工具链。

## 功能

- 虚拟化代码编辑器：语法高亮、软换行、多光标、折叠、书签、查找替换、IME、拖拽与触摸滚动。
- 实时 Typst 预览：默认栅格预览；`gpu-canvas` 特性开启时可选用实验性 GPU 画布，渲染不支持的内容自动回退。GPU 画布的大图默认使用源纹理 + mipmap 线性采样，小图/像素图/缩略图保留精确 CPU 路径；不可矢量化的 SVG 使用稳定源分辨率，允许轻微视觉差异。实验性设置中可用 log 滑条调整图片下采样长边阈值（1024–12800 px，默认 4096）。
- 项目能力：项目树、全项目搜索、文档大纲、符号跳转/引用/重命名、自动补全、诊断、拼写检查。
- Git（仅本地仓库）：状态、提交历史、逐提交 diff、行级 blame。
- 局域网协作：mDNS 发现、密码认证加密、服务端全局撤销/重做、远端光标、图片同步。
- 导出 PDF / PNG / SVG，崩溃恢复快照，中文/英文界面。

## 项目实例与对话框

同一项目同一时间只允许一个本地 Typst Editor 实例编辑。锁文件位于项目的 `.typ-edit/local/project.lock`，记录当前进程 PID 与进程启动时间；切换项目/关闭应用时释放，崩溃遗留的锁会在 PID 不存在或启动时间不匹配（PID 复用）时回收。`.typ-edit` 不出现在项目树中，协作路径校验也不会读取或写入 `.typ-edit/local/*`。

已有项目时，打开项目、打开文件、导出和另存为的 shell 对话框默认从当前项目根目录开始。鼠标悬停在编辑区文本上会显示 I-beam 光标。

## 工作区

`crates/` 下有 12 个成员 crate，根包 `typ-edit` 是集成层与二进制入口。

| Crate               | 职责                                        |
| ------------------- | ----------------------------------------- |
| `typ-edit-model`    | 跨线程消息与位置类型的纯契约，std-only                   |
| `typ-edit-buffer`   | rope 文本缓冲、行索引、撤销/重做                       |
| `typ-edit-editor`   | 不依赖 typst 的虚拟化 egui 编辑器控件                 |
| `typ-edit-preview`  | 不依赖 typst 的栅格预览控件                         |
| `typ-edit-widgets`  | 共享 egui 控件与设计 token                       |
| `typ-edit-i18n`     | 内嵌中英文语言包与运行时切换                            |
| `typ-edit-engine`   | Typst 解析/编译/栅格/拼写/搜索/IDE worker 与字体目录     |
| `typ-edit-analysis` | 不依赖 UI 的源码分析、引用分类与 lint                   |
| `typ-edit-git`      | 不依赖 UI 的本地 Git 集成                         |
| `typ-edit-collab`   | 不依赖 UI 的局域网协作协议、加密、状态机与服务端/客户端            |
| `typ-edit-geom`     | std-only 几何与 BVH，供 engine 与 GPU canvas 共享 |
| `typst-gpu-canvas`  | 可选的 Typst GPU 画布渲染器                       |

## 构建与运行

要求：Rust stable（当前验证版本 1.99）；Windows 需要 MSVC 工具链，macOS 需要 Xcode Command Line Tools，Linux/WSL 交叉构建需要 osxcross 的系统依赖。Typst 字体与 `wgpu` 运行环境由依赖提供。

```bash
cargo run --bin typst-editor        # 调试构建
scripts/release.sh                  # Release：strip + 路径重映射（macOS/Linux/WSL）
# Windows PowerShell: .\scripts\release.ps1

# Windows 一键 Release：Windows 安装包 + macOS DMG（PowerShell 或 BAT）
powershell -ExecutionPolicy Bypass -File scripts\release-all.ps1
scripts\release-all.bat
```

`release-all` 默认构建 Windows release，生成 `dist/windows/Typst-Editor-<version>-x64.msi` 与 `dist/windows/Typst-Editor-<version>-x64.exe`（NSIS 安装器，内嵌并调用 MSI；欢迎页/完成页使用与应用一致的浅色卡片 UI）；随后通过 WSL 运行 macOS release 构建，生成 `dist/macos/Typst Editor.app` 与 styled `Typst Editor.dmg`。`-DryRun` 只打印计划；`-SkipWindows` / `-SkipMac` / `-MacArch` 可控制范围。首次运行会把 WiX 4.0.6 装到用户本地缓存（Windows 下默认为 `%LOCALAPPDATA%\typ-edit\wix`），需要 dotnet；Windows 侧还需要 NSIS 3.x（`makensis.exe`，本机为 `C:\Program Files (x86)\NSIS`）；macOS 侧需要 WSL、osxcross 与 Go 1.24+。开发和 release 输出分离：dev 产物在 `dev-dist/`，release 安装包在 `dist/`。

Release 构建使用 `[profile.release] strip = "symbols"`；`scripts/release.sh` / `scripts/release.ps1` 注入 `--remap-path-prefix=$HOME=/build` 与 `--remap-path-prefix=$PWD=/src`，并透传 `--target` 等参数。Cargo 的 `.cargo/config.toml` 不展开这些变量（实测会原样传给 rustc），因此 remap 由 release wrapper 负责；直接 `cargo build --release` 会 strip，但不会自动 remap。

常用校验：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace --all-targets
```

`cargo test` 会编译并运行各 crate 的单测、集成测试、无头帧级 harness 与 GPU 保真测试；GPU 保真测试需要一个可用的 `wgpu` 适配器。

### macOS

原生构建（macOS + Xcode Command Line Tools）：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
bash scripts/macos/build-macos.sh --arch aarch64 --profile release --package
# Intel: --arch x86_64；通用二进制: --arch universal
```

`--package` 的 release 产物为 `dist/macos/Typst Editor.app`、`Typst Editor.app.tar.gz` 与 `Typst Editor.dmg`。DMG 包含 Finder 背景、拖拽安装布局、`Applications -> /Applications` 与卷图标；需要 Go 1.24+，`--no-dmg` 可跳过。`--profile dev` 的产物写到 `dev-dist/macos/`（结构相同）。macOS 主窗口使用 8 pt 圆角；新配置 UI 字体为 `PingFang SC`、编辑器字体为 `Menlo`（Windows 分别为 `Microsoft YaHei` / `Consolas`，平台硬编码属于 deviation）。

`build-macos.sh --profile release` 会自动应用 release strip 与 remap；`--profile dev` 不 strip/remap，并写入 `dev-dist/macos/`。

交叉构建（Linux / WSL2）：脚本会从 [joseluisq/macosx-sdks](https://github.com/joseluisq/macosx-sdks) 下载并校验 `MacOSX15.5.sdk.tar.xz`，按固定 commit 构建 osxcross，然后产出 release 的 `dist/macos/Typst Editor.app`、`Typst Editor.app.tar.gz` 与 `Typst Editor.dmg`。

```bash
bash scripts/macos/build-macos.sh --arch aarch64 --profile release --package
bash scripts/macos/build-macos.sh --arch aarch64 --profile release --package --no-dmg  # 跳过 DMG
bash scripts/macos/build-macos.sh --arch aarch64 --check   # 只做类型检查
```

DMG 步骤使用纯 Go 的 HFS+/UDIF 后端，WSL/Linux 不需要 macOS 的 `hdiutil`；需要 Go 1.24+。DMG 失败时 `.app`/`.tar.gz` 会保留，可加 `--no-dmg` 重试。

Windows 原生 shell 不能直接构建 macOS 产物；请在 WSL2（Ubuntu）中运行。

## 字体

启动时系统字体扫描在后台线程完成。首帧使用 egui 内置字体加内嵌数学字体，扫描完成后自动切换到用户配置的 UI/编辑器中文字体；typst 编译使用的 `FontStore` 直接复用同一份 `fontdb` 扫描结果，不会二次扫描系统字体。

## 协作安全边界

- 仅面向局域网；未设置密码时，协作流量为明文。
- 设置密码时使用 PBKDF2-HMAC-SHA256 + ChaCha20-Poly1305，双向 HMAC 认证，每连接派生独立会话密钥。
- 服务端在创建连接线程前检查连接上限；入站、待加入 outbox 与事件队列均有界；加入后必须在 `ready_timeout` 内发送 `Ready`。
- 仍未提供：PAKE、前向保密、按 IP 的加入速率限制。请只在可信局域网使用，并使用强密码。
