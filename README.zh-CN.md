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
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-blue" alt="AGPL-3.0-only 许可证"></a>
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
  <a href="#开发者信息">开发者信息</a> •
  <a href="#当前说明">当前说明</a> •
  <a href="#开源许可">开源许可</a>
</p>

## 认识 Torto

Torto（中文名“小龟阅读”）是一款围绕**专注模式**设计的开源桌面电子书阅读器。它不把书简单看作一叠静态页面，而是根据内容结构解析为有意义的阅读单元——段落、多级列表、引用、代码块、表格与图块——并让当前单元稳定锚定在舒适的居中起读位置。导航、高亮、批注、脚注、公式释义、表格图表、翻译和 AI 对话始终紧随当前上下文。

需要传统阅读方式时，可随时切换单栏、双栏和纵向滑动布局。与基于浏览器的阅读器不同，Torto 使用 `egui`、`Parley`、`Vello` 和 `wgpu` 构建原生 Rust 阅读管线，并在后台按需下载默认阅读字体，兼顾出版级排版与轻量安装包。

## 专注模式

专注模式是 Torto 默认且主打的精读体验：

- **按语义阅读，而非被页码割裂。** 每次聚焦一个阅读单元；多级列表与其引导段保持同组，超长表格或大图可在单元内平滑滚动，不会被直接跳过。
- **视线居中锚定。** 较短内容从固定的居中起读线开始，超长内容才向下展开，消除不同长度段落切换时的视线跳动；聚焦样式支持**背景高亮**或**周边淡化**。
- **全键盘段落操作。** 按 `Space` 唤起工具栏，按 `Tab` 唤起对话框，按 `1` 高亮段落，按 `2` 唤起批注框，按 `3` **按句分段**，按 `4` **可视化表格**（切换表格与 AI 图表），按 `0` 预览图表，按左 `Alt` 切换**脚注／公式备注**。所有快捷键均可自定义。
- **独立上下文与引用预览。** 随段对话绑定当前阅读单元，高亮与批注持久保存在精确原文位置；正文中引用的图、表与公式支持直接预览并按阅读顺序切换。
- **PDF 同样支持专注阅读。** 文字型 PDF 可直接通过本地重排进入专注模式；扫描版 PDF 在完成云端 OCR 后同样支持完整的单元导航与精读工具。

## 主要功能

| **功能** | **说明** | **状态** |
| --- | --- | --- |
| **专注模式** | 在稳定的居中起读线逐段精读，支持按句分段（`3`）、周边淡化及随段快捷操作。 | ✅ 核心 |
| **多格式与本地书架** | 支持无 DRM 的 EPUB、MOBI、AZW、AZW3/KF8、FB2、FBZ、CBZ、CHM 与 PDF；封面书架支持搜索、去重与阅读统计。 | ✅ |
| **原生 Rust 渲染** | 纯 Rust 完成解析、排版、分页与渲染，无 WebView；默认阅读字体在后台按需下载并跨版本保留。 | ✅ |
| **经典布局与主题** | 单栏、双栏与按节纵向滑动布局；可选统一版式或原书样式，支持浅色、深色与跟随系统主题。 | ✅ |
| **图表公式与引用预览** | 点击或按 `0` 缩放预览图片、表格与 LaTeX 公式；从正文引用直接预览图表与公式；一键将表格复制为 Markdown。 | ✅ |
| **AI 排版、公式与图表** | 补充未识别标题、引用、代码块与图注，将图片型表格转为文字表格；按左 `Alt` 查看独立公式符号说明，按 `4` 将表格切换为 AI 图表。 | ✅ 可选 |
| **AI 阅读助手** | 支持随段对话（`Tab`）与可调宽度的全书对话侧栏，带原文引用溯源、图片提问（含剪贴板粘贴图片）、`/` 阅读技能与 `@` 引用。 | ✅ 可选 |
| **双语翻译与术语库** | 支持替换或双语对照翻译正文与目录；可选开启**专家翻译**，自动按书积累并复用专有名词术语表。 | ✅ 可选 |
| **PDF 本地重排与 OCR** | 文字型 PDF 支持本地直接重排；支持 AI 自动识别目录与元数据，或通过 PaddleOCR / MinerU 识别扫描版 PDF。 | ✅ |
| **WebDAV 云同步** | 通过自有 WebDAV 服务直连同步书籍、阅读进度、高亮、笔记、阅读统计及已生成的 PDF 重排内容。 | ✅ 可选 |
| **应用内反馈与自动更新** | 在应用内附带截图与阅读上下文提交问题反馈或功能建议；Windows 支持 SHA-256 校验自动升级。 | ✅ |

### AI 与 PDF 能力亮点

- **丰富的 AI 服务商支持：** 内置 OpenAI、Anthropic、Gemini、DeepSeek、OpenRouter、SiliconFlow、xAI、Groq、Mistral、Moonshot、MiniMax、Z.ai、Ollama、llama.cpp 及自定义 OpenAI 兼容接口。
- **无感调度与多语言适配：** AI 排版与翻译优先处理当前阅读区域并按小节稳定分批，相关结果合并应用以减少版面跳动；即使未开启正文翻译，AI 图表文字与公式符号说明也会自动使用翻译目标语言。
- **智能 PDF 工作流：** 文字型 PDF 本地重排自动过滤页眉页脚并修复断词连字符；配置视觉与工具模型后，可自动探查封面书名、目录结构及印刷页码映射。

## 产品截图

### 整洁的本地书架

导入电子书后自动读取书名、作者和封面，支持搜索、查看阅读统计，并直接继续上次阅读。

![Torto 小龟阅读书架](assets/screenshots/library.png)

### 经典双栏布局

除默认的专注模式外，也可随时切换到传统的双栏书页视图。

![Torto 小龟阅读经典双栏界面](assets/screenshots/reader.png)

## 下载与使用

前往 [GitHub Releases](https://github.com/TortoTech/torto/releases/latest) 下载最新安装包。

| **平台** | **安装包** | **系统要求** |
| --- | --- | --- |
| Windows | `Torto-*-x86_64.msi` | 64 位 Windows 10 或 Windows 11 |
| macOS，Apple 芯片 | `Torto-*-macos-arm64.dmg` | macOS 15 或更高版本 |
| macOS，Intel | `Torto-*-macos-x86_64.dmg` | macOS 15 或更高版本 |

安装后导入书籍即可在专注模式下阅读：
- 使用 `↑` / `↓`、鼠标滚轮或滚动条切换阅读单元。
- 按 `Space` 唤起工具栏，按 `Tab` 唤起对话框，按 `1` / `2` 高亮段落或唤起批注框，按 `3` 按句分段，按 `4` 可视化表格，按 `0` 预览图表，按左 `Alt` 切换脚注／公式备注。
- 按 `Ctrl + F` 搜索全书，按 `Ctrl + ,` 配置排版、主题、翻译、AI、OCR、快捷键与 WebDAV 同步。

## 数据与隐私

- 导入的电子书、批注、统计及缓存的 OCR/重排数据均保存在本机应用数据目录。
- WebDAV 密码和 AI API Key 保存在操作系统安全凭据存储中，不写入明文配置文件。
- AI 排版、翻译、对话和云端 OCR 直连你自行配置的服务商；启用后，AI 排版与翻译会随阅读发送调度范围内的相关文字或图片。
- WebDAV 同步由客户端直接连接你选择的网盘服务，不经过任何 Torto 中转服务器。
- 在应用内提交反馈时，会将你填写的描述、可选截图及弹窗中展示的阅读上下文通过 TortoTech 反馈服务发布为公开的 GitHub Issue。详见[隐私政策](PRIVACY.md)。

## 开发者信息

项目使用 Rust `1.97.1`，桌面端包名为 `rebook-desktop`，运行产物为 `Torto`（Windows 下为 `torto.exe`）。

请在仓库根目录运行 Cargo 命令，以应用 `.cargo/config.toml` 配置：

```powershell
cargo run --locked -p rebook-desktop
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

核心阅读管线为 `parser → Reading IR → layout → renderer`，架构与设计文档位于 `docs/`：
- [原生渲染架构决策](docs/adr-0001-native-epub-renderer.md) 与 [Parley 排版引擎评估](docs/parley-engine-evaluation.md)
- [AI 语义排版](docs/ai-semantic-layout.md)、[分批策略](docs/ai-layout-batching.md) 与 [调度机制](docs/ai-layout-scheduling.md)
- [PDF 原生本地重排](docs/pdf-native-reflow.md) 与 [阅读统计设计](docs/reading-statistics.md)
- [WebDAV 同步协议 v1](docs/webdav-sync-v1.md) 与 [上游依赖已知问题](docs/known-upstream-issues.md)

<details>
<summary>品牌资源生成、macOS 签名与反馈服务配置</summary>

### 品牌资源生成

`assets/logo.svg` 为统一矢量母版，修改后可重新生成主 PNG、Windows 安装图及各平台图标：

```powershell
cargo run --locked -p rebook-desktop --example generate_branding
cargo run --locked -p rebook-desktop --example generate_windows_icons
cargo run --locked -p rebook-desktop --example generate_macos_icons
```

### macOS 凭据与发布签名

单元测试使用内存凭据存储，不会读写系统钥匙串；正式构建使用操作系统安全凭据存储。

macOS 打包工作流支持配置 `MACOS_SIGNING_IDENTITY`、`MACOS_CERTIFICATE_P12_BASE64` 与 `MACOS_CERTIFICATE_PASSWORD`（Bundle ID 为 `io.github.l-chris.torto`）。

### 桌面端反馈服务

编译或运行时可通过环境变量 `TORTO_FEEDBACK_URL`（需以 `/api/feedback` 结尾）指定部署的 HTTPS 反馈服务端点。GitHub App 凭据仅保存在服务端，不会打包进客户端。

</details>

## 当前说明

Torto 仍在持续开发中。当前不支持带 DRM 的电子书；原始版式 PDF 使用经典模式，生成本地重排或云端 OCR 内容后即可进入专注模式。原生渲染器不追求完整浏览器级 HTML/CSS 兼容，复杂固定版式、竖排、Ruby 注音及部分书内交互内容可能暂未完整呈现。

如遇可复现的问题，欢迎在 [GitHub Issues](https://github.com/TortoTech/torto/issues) 中反馈并附上格式、截图与复现步骤（请勿上传受版权保护的完整书籍）。

## 开源许可

本项目基于 [GNU Affero General Public License v3.0](LICENSE)（`AGPL-3.0-only`）开源。
