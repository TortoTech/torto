<p align="center">
  <a href="README.md">English</a> · <strong>简体中文</strong>
</p>

<p align="center">
  <img src="assets/windows/torto-128.png" width="112" height="112" alt="Torto 小龟阅读图标">
</p>

<h1 id="torto-小龟阅读" align="center">Torto · 小龟阅读</h1>

<p align="center">
  一款以专注模式为核心、本地优先的 Windows 与 macOS 电子书阅读器。<br>
  不依赖 WebView，以原生 Rust 渲染，让书库始终由你掌控。
</p>

<p align="center">
  <a href="https://github.com/TortoTech/torto/releases/latest"><img src="https://img.shields.io/github/v/release/TortoTech/torto?display_name=tag&sort=semver" alt="最新版本"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/TortoTech/torto" alt="MIT 许可证"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-5b6ee1" alt="支持 Windows 与 macOS">
  <img src="https://img.shields.io/badge/UI-egui-7c3aed" alt="使用 egui 构建">
  <a href="https://linux.do"><img src="https://img.shields.io/badge/LINUX-DO-FFB003.svg" alt="LINUX DO"></a>
</p>

<p align="center">
  <a href="#专注模式">专注模式</a> •
  <a href="#主要功能">主要功能</a> •
  <a href="#产品截图">产品截图</a> •
  <a href="#下载与使用">下载与使用</a> •
  <a href="#数据与隐私">数据与隐私</a> •
  <a href="#当前说明">当前说明</a> •
  <a href="#开发者信息">开发者信息</a> •
  <a href="#开源许可">开源许可</a>
</p>

## 认识 Torto

Torto（中文名“小龟阅读”）是一款围绕**专注模式**设计的开源桌面电子书阅读器。它不把书简单看成一叠页面，而是根据内容结构形成有意义的阅读单元——普通段落、多级列表、引用、代码块、表格和图片——并让当前单元保持在稳定的阅读位置。导航、高亮、批注、脚注、翻译和 AI 对话都会跟随当前上下文。

需要传统阅读方式时，仍可使用经典模式的单栏、双栏和纵向滑动布局。本地书架、全文搜索、翻译、可选 AI 助手、PDF OCR 与 WebDAV 同步，则建立在同一套原生阅读模型之上。

与基于浏览器的阅读器不同，Torto 使用 egui、Parley、Vello 和 wgpu 构建原生 Rust 阅读管线，自行完成书籍解析、排版、分页与渲染。

## 专注模式

专注模式是 Torto 默认且主打的阅读体验，也是它区别于传统电子书阅读器的核心功能。

- **按内容语义阅读，而不是被页面限制。** 每次激活一个阅读单元；图片也能成为当前单元，多级列表的子项会跟随所属父项，较大的表格和图片仍可在单元内部滚动，不会被直接跳过。
- **让视线保持稳定。** 较短内容会从居中阅读区域内的固定位置开始，只有内容超过可用高度时才向外扩展，减少不同长度段落切换时反复上下跳动。
- **统一的单元导航。** 鼠标滚轮、方向键和正文滚动条都按阅读单元移动；目录、对话、脚注面板或输入框正在操作时，滚动与按键会留在对应区域内。
- **直接处理当前内容。** 按 `Space` 唤起工具栏，按 `Tab` 打开当前单元的对话，按 `1` 高亮，按 `2` 添加批注，按 `3` **按句分段**，按一次左 `Alt` 查看或关闭脚注。所有快捷键均可配置。
- **上下文不会串段。** AI 对话在本次打开书籍期间绑定到对应阅读单元；高亮与批注则持久保存到稳定的原文位置。当前单元的脚注会收纳为简洁图标，需要时再展开查看。
- **让扫描 PDF 也能进入专注阅读。** 原始版式 PDF 使用经典模式；完成正文 OCR 并生成流式版式后，即可切换到专注模式，继续使用相同的单元导航与阅读工具。

## 主要功能

<div align="left">✅ 已实现</div>

| **功能** | **说明** | **状态** |
| --- | --- | --- |
| **专注模式** | 在稳定位置阅读一个语义单元，并让导航、操作、对话、批注和脚注始终跟随当前内容。 | ✅ 核心 |
| **多格式支持** | 阅读无 DRM 的 EPUB、MOBI、AZW、AZW3/KF8、FB2、FBZ、CBZ、CHM 与 PDF 文件。 | ✅ |
| **原生渲染** | 使用 Rust 完成解析、排版、分页与渲染，不嵌入浏览器或 WebView。 | ✅ |
| **经典模式** | 在单栏、双栏和按小节纵向滑动布局之间切换，并可选择统一覆盖或跟随书籍的正文样式。 | ✅ |
| **封面式本地书架** | 批量导入并浏览封面卡片，按书名或作者搜索，优先展示最近阅读的书籍，并识别重复导入。 | ✅ |
| **导航与搜索** | 支持层级目录、章节跟随、全书搜索、键盘导航、鼠标滚轮翻页和 `F11` 全屏。 | ✅ |
| **排版与主题** | 配置默认字体、中文字体、代码字体和界面字体；选择统一或书籍原版样式；主题可跟随系统或使用浅色、深色模式。 | ✅ |
| **文字选择与批注** | 支持自由、按单词、按句子和按段落选择，复制文字、创建高亮与笔记，并准确返回原文位置。 | ✅ |
| **图片预览** | 点击正文图片打开蒙层预览，通过滚轮缩放、拖拽平移，并可复制图片到剪贴板。 | ✅ |
| **翻译阅读** | 以替换或双语模式翻译正文，也可以翻译书籍目录。 | ✅ |
| **AI 阅读助手** | 流式返回带原文引用的回答，并渲染 Markdown、表格、公式、SVG 与 Mermaid，图形可放大查看。 | ✅ 可选 |
| **PDF OCR 与元数据识别** | 识别书名、作者、目录、特殊页面与正文，并在 PDF 原始版式和 OCR 流式版式之间切换。 | ✅ 可选 |
| **WebDAV 同步** | 通过自己的 WebDAV 服务直接同步书籍、阅读进度、高亮与笔记。 | ✅ 可选 |
| **Windows 自动更新** | 自动检查 GitHub Releases，以 SHA-256 校验 MSI，并在用户确认后完成升级。 | ✅ Windows |

## 产品截图

### 整洁的本地书架

导入电子书后自动读取书名、作者和封面，可以搜索书籍，也可以直接继续上次阅读。

![Torto 小龟阅读书架](assets/screenshots/library.png)

### 经典双栏布局

专注模式是上文介绍的默认体验；如果更喜欢传统的书页视图，也可以切换到经典双栏布局。

![Torto 小龟阅读经典双栏界面](assets/screenshots/reader.png)

## 下载与使用

前往 [GitHub Releases](https://github.com/TortoTech/torto/releases/latest) 下载最新安装包。

| **平台** | **安装包** | **系统要求** |
| --- | --- | --- |
| Windows | `Torto-*-x86_64.msi` | 64 位 Windows 10 或 Windows 11 |
| macOS，Apple 芯片 | `Torto-*-macos-arm64.dmg` | macOS 12 或更高版本 |
| macOS，Intel | `Torto-*-macos-x86_64.dmg` | macOS 12 或更高版本 |

首次打开后：

1. 点击书架右上角“导入”，选择一本或多本电子书。
2. 点击封面或书名进入阅读。
3. 默认在专注模式下，使用 `↑` / `↓`、鼠标滚轮或正文滚动条切换阅读单元。
4. 按 `Space` 唤起当前内容的操作工具栏，按 `Tab` 打开绑定到当前单元的对话。
5. 使用 `Ctrl + F` 搜索当前书籍。
6. 通过阅读器菜单调整布局、排版、主题、翻译、AI、PDF OCR、快捷键与云同步设置。

## 数据与隐私

- 导入的电子书和阅读数据保存在本机应用数据目录。
- WebDAV 密码和 AI API Key 保存在系统安全凭据存储中，而不是普通配置文件里。
- AI 与翻译功能需要主动配置和使用，Torto 不会在后台自动发送书籍内容。
- WebDAV 同步由桌面端直接连接用户选择的服务，不经过 Torto 自建中转服务器。

## 当前说明

Torto 仍在持续开发中。当前不支持带 DRM 的电子书；PDF 在原始版式下不能使用专注模式，需要先生成可重排的 OCR 正文。原生渲染器也不追求完整浏览器级 HTML/CSS 兼容，复杂固定版式、竖排、Ruby 注音以及部分书内交互内容仍可能无法完整显示。

如果遇到解析、排版或安装问题，欢迎在 [Issues](https://github.com/TortoTech/torto/issues) 中反馈，并附上电子书格式、问题截图和可复现步骤。请勿上传受版权保护的完整书籍。

## 开发者信息

<details>
<summary>展开本地开发说明</summary>

### 本地开发

项目使用 Rust `1.97.1`，桌面端包名仍为 `rebook-desktop`，运行产物为 `torto.exe`。

```powershell
# Windows 可选：安装 sccache 以复用编译缓存。
winget install --id Mozilla.sccache --exact

# 使用 sccache；未安装时会回退到普通 Cargo。
.\scripts\cargo-sccache.cmd run --locked -p rebook-desktop
cargo run --locked -p rebook-desktop

# 与 Release 接近并保留调试符号的性能构建。
cargo run --locked --profile perf -p rebook-desktop
```

修改 Rust 或 TOML 后自动重启：

```powershell
watchexec -r -e rs,toml -- cargo run --locked -p rebook-desktop
```

质量检查：

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

重新生成 Windows / macOS 多尺寸图标：

```powershell
cargo run -p rebook-desktop --example generate_windows_icons
cargo run -p rebook-desktop --example generate_macos_icons
```

核心架构采用 `parser → Reading IR → layout → renderer`，正文由 Parley、Vello 和 wgpu 完成原生排版与渲染。

- [原生渲染架构决策](docs/adr-0001-native-epub-renderer.md)
- [WebDAV 同步协议 v1](docs/webdav-sync-v1.md)
- [核心依赖已知问题](docs/known-upstream-issues.md)

</details>

## 开源许可

本项目基于 [MIT License](LICENSE) 开源。
