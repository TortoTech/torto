# 核心依赖已知问题

- 最近更新：2026-10-09
- 记录范围：已经在 Torto 中复现、确认与上游依赖、Windows 图形栈或渲染帧时序有关，并需要本地兼容代码或长期回归检查的问题

依赖升级时应逐项检查本文。只有在上游修复已经进入当前版本，并且移除本地兼容代码后相关回归测试仍能通过，才删除对应兼容代码和本文条目。

## Rig：OpenAI 兼容网关的签名扩展导致响应解析失败或回传丢失

- 影响版本：`rig-core 0.42.0`、`0.43.0`；项目已升级到 `rig-core 0.44.0` 和 `rig-reqwest 0.44.0`。
- 核查日期：2026-10-09。[0.44.0](https://github.com/0xPlaygrounds/rig/releases/tag/v0.44.0)（2026-10-07）已包含 PR #2713 的 JSON-first 协议层和原始条目回传。
- 本地位置：`apps/desktop/src/plugins/llm.rs` 的 `dispatch`、`execute`、`response_message`；`apps/desktop/src/plugins/llm/history.rs` 的当前内存消息适配与作用域校验；`apps/desktop/src/plugins/llm/transport.rs` 的 JSON 回复转流式桥接和搜索来源收集。
- 官方跟踪：[Rig #2591](https://github.com/0xPlaygrounds/rig/issues/2591)（兼容响应解析过严，同类问题，非完全相同复现）、[PR #2713](https://github.com/0xPlaygrounds/rig/pull/2713)（2026-10-04 合并到主分支，宽容 JSON 解析和原始条目回传，已进入 0.44.0，未进入 0.43.0）。未找到专门报告 Bifrost 此签名结构的官方 issue；本次没有向上游创建 issue。

### 复现证据

目录识别使用自定义 OpenAI 兼容端点的 `/chat/completions`，模型 ID 为 `gemini/lite`。Bifrost 负责转换 Gemini 原生协议，客户端并未直接调用 Gemini API。

网关真实工具调用响应包含如下扩展：

```json
{"reasoning_details":[{"type":"reasoning.encrypted","id":"call-example","index":0,"signature":"<opaque signature>"}]}
```

旧版 Rig 的 `ReasoningDetails::Encrypted` 强制要求 `data: String`，因此整个响应报 `missing field data`。实验中人为补 `data` 可通过解析，但 `signature` 不属于该类型的字段，经过 SDK 序列化会消失；这不能作为修复。Bifrost 的 `reasoning_details` 是标准之外的扩展，`signature` 与 `data` 是不同字段，不能互相假定等价。

2026-10-07 使用相同网关、凭据和模型，完整目录工具声明的首轮请求及原样回传后的第二轮请求均返回 HTTP 200；真实首轮响应交给项目锁定版本的 SDK，仍复现上述错误。签名值没有写入诊断日志或本文。

2026-10-07 旧版兼容补丁后用本地《The psychology of reading》（554 页）实测：首轮再次遇到上述连接失败，一次重试后恢复，随后运行到第 19 轮模型请求，可正常处理页面概览与阅读调用，没有 SDK 解码或签名回传错误。但模型也出现一次 `read_pages` 请求 6 页、超过声明的 5 页上限的无效调用；约 96 秒后因重复查看页面、没有形成有效目录/元数据草稿而触发停滞保护，返回“连续调用未取得新进展，已保留草稿”。这是尚待调查的模型识别/工具决策问题，本轮实测不能记为整本目录识别通过；报告位于本地 `target/pdf-agent-live/report.json`，不随代码提交。

### 0.44.0 的实现与保留的适配

1. SDK 负责宽容解码、原始条目及签名回传、联网工具与业务工具合并。已删除本地原始 assistant 副本、`reasoning_details` 清洗和发送前逐消息恢复代码，不再假造密文字段或丢弃未知扩展。
2. 现有调用方仍用 JSON 组织当前请求，`history.rs` 将其适配为新版消息。私有消息只保留一份 SDK 消息、内容指纹和提供商/端点/凭据/模型摘要；内部标记不发送到 API，凭据不存入消息。
3. SDK 的来源校验不覆盖端点和凭据，因此 Torto 继续校验这些摘要。换端点、凭据、模型或修改 SDK 消息后，去除推理/不透明条目及原始签名，保留普通文本、调用和结果。
4. 对话及请求内工具历史不持久化，没有旧版对话数据迁移或兼容流程。SDK 消息和指纹仅服务于当前请求的多轮工具调用。
5. HTTP 层保留网关忽略 `stream`、返回完整 JSON 时的流式桥接，以及搜索来源收集。真正 SSE 直接由 SDK 解码。非联网普通请求不额外解析一份完整 HTTP 响应。
6. 新版宽容解码允许无效工具参数进入响应；普通业务工具仍在执行前拒绝，结构化结果工具沿用应用层 JSON 修复与 Schema 校验。未正常完成的响应不应用到书籍或继续执行工具。

回归覆盖：多轮网关签名、未知扩展和密文回传，真正 SSE 的签名调用及文字增量，作用域/内容变更失效，xAI 的版本路径、图片和原生 Schema 映射，以及异常参数和未完成响应拒绝。此次升级验证使用本地 HTTP 夹具，不代表《The psychology of reading》整本目录识别已通过。

2026-10-09 验证：桌面客户端构建通过；插件回归 308 项通过，25 项需要真实服务或本地书库的测试保持忽略。

### 另一个独立故障：网关连接上游时提前断开

2026-10-07 客户端日志记录：目录识别第一轮约 1045 ms 返回 HTTP 502，错误类型 `provider_connection_failed`，网关底层错误为 `fasthttp: the server closed connection before returning the first response byte`。当时尚未发送 PDF 页面图片，request ID 为 `aedb1e0a-cc8e-4c6e-9542-f43a0320d958`。

该错误与 SDK 解码无关，具体断连根因仍需网关的上游连接日志。`pdf_toc/agent.rs::complete_round` 对明确的 502/503/504 `provider_connection_failed` 最多重试两次，等待 1 秒和 2 秒；重发相同轮次，成功前不执行工具，等待计入原有总截止时间。认证、限流、请求格式、响应解析错误和普通 5xx 不按此策略重试；最终失败保留原始错误和已有草稿。

回归测试：`connection_retry_classification_excludes_auth_schema_and_generic_server_errors`、`connection_retry_resends_same_round_before_tool_execution`、`connection_retry_is_bounded_and_preserves_final_error`、`connection_retry_does_not_retry_authentication_errors`。

### 后续跟踪

1. 在实际 Bifrost 端点核对目录识别的完整工作流，重点观察模型选页、工具参数和停滞保护。不得把局部解码通过记为整本识别完成。
2. 后续 SDK 升级继续回归多轮不透明字段回传、真正 SSE、跨端点/凭据/模型切换、翻译和排版的结构化输出。若上游增加端点和凭据保护，再评估移除本地作用域校验。
3. 网关上游连接故障独立跟踪。SDK 升级不能替代上游根因排查，也不能据此删除有限重试。

## 2026-10-09 核心依赖升级

已将 GPU 栈协调升级到 Vello / vello_encoding 0.11.0、wgpu 30.0.1、egui / epaint / egui_extras / egui-winit / egui-wgpu 0.36.2；PDF 栈升级到 Hayro / hayro-interpret / hayro-syntax 0.8.0。同时更新 resvg / usvg 0.48.1、image 0.25.10、fontdb 0.24.0、ICU segmenter 2.3.0，以及项目直接使用的 Skrifa 0.48.0 / read-fonts 0.45.0。上游组件自行约束的字体/SIMD 版本仍分别保留，不能强制合并不兼容类型。Parley、Vello CPU 和 egui_commonmark 没有新的稳定版。

- egui-wgpu 改回官方 crate；旧 wgpu 29 适配目录不再被 `[patch.crates-io]` 引用，仅作为历史源代码保留。桌面适配 wgpu 30 的 adapter 参数和队列呈现接口，保留同帧 surface 恢复、原生背景和模拟全屏处理。
- 回移已合并但尚未发布的 [egui #8316](https://github.com/emilk/egui/pull/8316)，同时修改 egui 和 egui_extras，保留上游诊断/虚拟占位回归测试。0.36.2 使用 `UiBuilder::id` / `Ui::id` 表达新版的稳定 scope ID；#8343 仍开放，暂不撤销现有全局误报兼容设置。
- Hayro 区域渲染改用官方 `render_into`，删掉旧 renderer 模块和自定义区域/解码向量补丁。客户端仍保持整数像素原点、256×4 tile 网格及裁剪保护边。文字提取与图片导出同步迁移到 positioned glyph run 和逐次 draw props，继承资源使用新版已解析的页面 Resources。
- Hayro 有界 outline 缓存、逐页解释器释放和权威 PDF 解码后的独立图片采样继续保留。原有图片图集刷新、EBDT 字体过滤、两端对齐选区、离屏 Markdown 选区以及目录虚拟化兼容代码未删除。
- PDF 本地重排生成版本提高到 **V13**，防止旧解释器生成的像素/语义产物与新版混用。旧文件和已有 OCR 资源不在本次升级中批量重写。

验证：最终生产路径 `cargo build -p rebook-desktop --locked` 和 `cargo check --workspace --locked` 通过；核心库 519 项、桌面 772 项、隔离运行的 egui/egui_extras/epaint 104 项单元回归共 **1,395 项**通过。回移的控件移动/合法替换诊断及虚拟占位 ID 测试通过；有界缓存、旋转/非零原点、遮罩、裁剪、独立图片及源映射回归通过。隔离上游测试启用项目实际使用的 PNG/JPEG/GIF/WebP 编解码功能。

真实书籍验证：151,782,128 字节的《Pick, Click, Flick!》14 个物理页共 27 张图片，区域导出与新版整页对应像素完全一致；《The Science of Beauty》的产品表格图片及两项复杂图注诊断通过；《Thinking in Systems》Figure 15–17 在窄宽度自然折行时左对齐、宽宽度单行时居中。三本 PDF 直接调用公开 `assess` 接口，新旧生成版本的 12 页抽样结果逐项一致：Designing Type 为 Native（11 Native / 1 Sparse），Pick, Click, Flick! 为 Native（10 / 2），Thinking with Type 为 Ocr（4 Native / 7 Unreliable / 1 Sparse）。既有 `local_quality_signals` 诊断要求至少 8 个 Native 样本，不适用于最后一本；没有因此修改其实际 OCR 判断规则。

Windows 运行检查使用 NVIDIA GTX 1080、NVIDIA 582.66、Vulkan。合成 PDF、《The Science of Beauty》和《Designing Type》通过 GPU 提交/内容呈现及最小化恢复后的持续帧检查。Science 曾有一次恢复后再次被最小化而超时，重跑通过。空书架启动检查通过；最小化恢复检查在新旧客户端均恢复后未满足持续帧要求而超时（旧版 40 帧、新版 36 帧），没有据此修改生产调度，也未将该场景记为通过；具体原因仍需单独排查。普通客户端使用现有书库进行空闲抽样：初次失焦运行存在间歇渲染/CPU 活动，旧版对照样本同时发生最小化，不能用作等价性能对比。重新启动新版后等待 20 秒加载，再连续采样 15.03 秒，CPU 时间增量为 0，日志未出现新的渲染记录；仅据此确认该次安静样本没有持续忙转，不声称性能提升。截图辅助通道缺少 native pipe，未完成画面/弹窗的人工式交互检查。Parley 随 ICU 2.3 的现有 BidiClass API 出现弃用警告，不影响构建；没有声称严格零警告 clippy 或速度/内存改善。

日志及版本解析报告位于忽略目录 `tmp/dependency-upgrade-20261009/` 和 `tmp/dependency-upgrade-*.log`。

## 2026-10-09 上游版本核查（升级前）

以下记录是升级前的核查快照。本次核对 `Cargo.lock` 的实际解析版本、本地 `third_party` 补丁、crates.io 最新非撤回稳定版、官方 issue/PR 和发布版本源码。没有更新依赖或移除兼容代码，也没有运行新版的客户端回归；下列“可替代”表示源码层面的迁移候选，不代表 Torto 已验证新版行为。上一轮 2026-09-10 的结果保留在后面作为历史记录。

### 当前可用版本

| 依赖 | 项目实际版本 | 最新稳定版 | 核查结论 |
| --- | --- | --- | --- |
| egui / epaint / egui-wgpu / egui-winit / egui_extras | 0.36.1；前三项为本地 fork | [0.36.2](https://github.com/emilk/egui/releases/tag/0.36.2)，2026-09-08 | 已修复 TextEdit 最小高度、细长倾斜矩形、大写图片扩展名等。官方 egui-wgpu 0.36.2 [依赖 wgpu 30](https://github.com/emilk/egui/blob/0.36.2/Cargo.toml)，当前本地适配使用 wgpu 29，不能仅修改版本号并覆盖 fork。 |
| Vello / vello_encoding | 0.10.0 | [0.11.0](https://github.com/linebender/vello/releases/tag/v0.11.0)，2026-10-02 | 已修复两类 sbix 位图字形定位，并升级到 wgpu/naga 30；不是宋体 EBDT 或图片重放问题的修复。 |
| wgpu / wgpu-core / wgpu-hal | 29.0.4 | [30.0.1](https://github.com/gfx-rs/wgpu/releases/tag/v30.0.1)，2026-08-22 | 可与 Vello 0.11 和官方 egui-wgpu 0.36.2 对齐；未找到覆盖当前 Windows 黑帧问题的已发布修复。 |
| Hayro / hayro-interpret / hayro-syntax | 0.7.1 本地 fork / 0.7.0 / 0.7.2 | [0.8.0](https://crates.io/crates/hayro/0.8.0)，2026-10-04 | 新增自定义变换/缓冲区渲染，修复图片解码尺寸受平移影响；包含解码、颜色转换、缓存和图片处理改进。解释器 Device API 也有变化，需迁移 PDF 提取与渲染两条路径。 |
| Vello CPU / vello_common | 0.3.0 | [0.3.0](https://crates.io/crates/vello_cpu/0.3.0)，2026-10-02 | 已是最新稳定版。当前 Hayro fork 和 AnyRender CPU 后端已使用此版，不能把其中已有的修复算作再次升级 Hayro 的收益。 |
| Parley | 0.11.1 本地 fork | [0.11.1](https://crates.io/crates/parley/0.11.1) | 无新版；选区 issue 仍开放，本地行内图片/ruby 边界和方向补丁也需保留。 |
| egui_commonmark / backend | 0.25.0 本地 fork | [0.25.0](https://crates.io/crates/egui_commonmark/0.25.0) | 无新版，多组件选区 issue 仍开放。 |
| winit | 0.30.13 | [0.30.13](https://crates.io/crates/winit/0.30.13) | 无新版稳定版；0.31.0-beta.3 仍为预发行版。 |
| Skrifa / read-fonts | 0.44.0、0.42.1 / 0.41.0、0.39.2 | [0.48.0](https://crates.io/crates/skrifa/0.48.0) / [0.45.0](https://crates.io/crates/read-fonts/0.45.0)，2026-10-03 | 有新版，但 Vello 0.11 仍声明 Skrifa 0.44；单独增加最新版不会接通 Vello 的 EBDT mask 绘制。 |
| resvg / usvg | 0.47.0 | [0.48.1](https://github.com/linebender/resvg/releases/tag/v0.48.1)，2026-08-02 | 0.48 改用 Skrifa/Harfrust，修复字形 advance、文本及嵌套 SVG 变换、渐变和过滤器崩溃；值得做 SVG 像素/文字回归，未对应本文主要兼容问题。见 [changelog](https://github.com/linebender/resvg/blob/v0.48.1/CHANGELOG.md)。 |
| image | 精确锁定 0.25.6 | [0.25.10](https://github.com/image-rs/image/blob/v0.25.10/CHANGES.md) | 有新版；包含 PNG/GIF/WebP/BMP 解码修复，0.25.9 将 zune-jpeg 升到 0.5。更新前需核对现有颜色、透明度、JPEG 和封面像素，不保证速度改善。 |
| fontdb | 0.23.0 | [0.24.0](https://github.com/RazrFalcon/fontdb/blob/master/CHANGELOG.md) | 新增 iOS 系统字体支持并去除 ttf-parser 依赖；没有本文 Windows 空白字形问题的对应修复。 |
| ICU segmenter | 2.2.0 | [2.3.0](https://crates.io/crates/icu_segmenter/2.3.0) | 有新版；与本文图形栈兼容问题无直接对应关系。 |
| AnyRender / AnyRender Vello CPU、Peniko、Kurbo、RaTeX、Rig | 0.14.0 / 0.18.0、0.6.1、0.13.1、0.1.14、0.44.0 | 与当前相同 | 已是最新稳定版；Rig 网关签名解析问题已在当前 0.44.0 升级中处理，见本文首节。 |

### 记录问题与新修复的对应关系

| 本地问题 | 2026-10-09 核查结果与处理 |
| --- | --- |
| 虚拟表格控件 ID/矩形误报 | [egui #8092](https://github.com/emilk/egui/issues/8092) 已由 [PR #8316](https://github.com/emilk/egui/pull/8316) 在 2026-10-05 合并后关闭。修复稳定表格自动 ID、避免虚拟占位行消耗 ID，并加入局部诊断豁免 API；尚未进入 0.36.2。可优先评估单独回移该修复，不能仅升 0.36.2 就撤销兼容设置。另一个 [#8343](https://github.com/emilk/egui/issues/8343) 仍开放。 |
| 显式高度 TextEdit | [#8420](https://github.com/emilk/egui/pull/8420) 已进入 0.36.2；修复最小高度被忽略。已有感知区域和 hint 对齐修复不是此次新增；设计要求的显式 vertical_align 仍需保留。 |
| Hayro 区域渲染与图片解码尺寸 | 0.8.0 已包含 [#1375](https://github.com/LaurenzV/hayro/pull/1375) 的 `render_into`，可以传入独立 RenderContext 和任意 Affine；[#1384](https://github.com/LaurenzV/hayro/pull/1384) 排除解码尺寸计算中的平移。两项能力与本地区域渲染/变换向量补丁相对应，是迁移候选；仍须验证 256×4 tile 对齐、旋转、非零页原点、遮罩和裁剪的像素一致性。0.8.0 的 RenderCache 仍无公开预算或逐页释放接口，不能删除有界缓存和独立图片导出等补丁。 |
| Vello 图片重放缺失 | [#1809](https://github.com/linebender/vello/issues/1809) 仍开放，0.11.0 发布说明没有对应修复。保留图片图集刷新。 |
| 宋体等 EBDT 字形空白 | [Fontations #1639](https://github.com/googlefonts/fontations/issues/1639) 仍开放；0.11.0 [发布源码](https://github.com/linebender/vello/blob/v0.11.0/research/vello_research/src/scene.rs) 仍遇到 unpacked mask 就跳过字形。sbix 定位修复是不同格式的问题，保留字体过滤。 |
| Windows 全屏/IME 黑帧、窗口放大黑边 | [winit #3730](https://github.com/rust-windowing/winit/issues/3730)、[wgpu #5374](https://github.com/gfx-rs/wgpu/issues/5374) 仍开放，未找到完全覆盖 Torto 全屏/IME 场景的发布修复。wgpu 30.0.1 的 acquire fence 修复针对非 Windows；模拟全屏、同帧 surface 恢复和原生背景处理继续保留。 |
| 两端对齐选区不足 | [Parley #396](https://github.com/linebender/parley/issues/396) 仍开放，无新版；保留选区几何修正。 |
| 紧凑圆角羽化角线 | [#2735](https://github.com/emilk/egui/issues/2735)、[#7424](https://github.com/emilk/egui/issues/7424) 仍开放；0.36.2 的细长倾斜矩形修复不是圆角控件的一一对应修复。 |
| Markdown 跨组件选区、离屏端点清空 | [egui_commonmark #80](https://github.com/lampsitter/egui_commonmark/issues/80) 仍开放；egui 0.36.2 的 Label 可见区域检查和端点未遇到时清空选区逻辑仍存在。保留不可选布局换行、本地 Label 与 AI 表格布局。 |
| 虚拟目录底部抖动 | [#1787](https://github.com/emilk/egui/issues/1787)、[#3268](https://github.com/emilk/egui/issues/3268) 仍开放；0.36.2 的 `show_rows` 仍在末行越界时回补首行。保留目录虚拟化。 |
| 专注模式滚轮跨小节闪首图 | 已有本地输入时序修复，不是依赖版本更新可替代的问题。 |

### 建议的升级顺序

1. 先评估 egui #8316 的回移及 0.36.2 的具体修复，保留现有选区、Markdown、epaint 和 wgpu 29 适配。#8316 同时修改 egui 和 egui_extras，不能只合并 Context 的一部分；恢复诊断前回归虚拟表格、反向布局和动态 UI。
2. 独立评估 Hayro 0.8.0，迁移 PDF 提取和渲染的 API，并逐项对照 `third_party/hayro/TORTO_PATCHES.md`。有机会减少区域渲染/图片解码补丁，但有界缓存等补丁仍需维护。先跑旋转、裁剪、透明度、资源尺寸/来源审计，再比较同一构建配置的整本耗时和峰值内存，不能从上游优化提交推断 Torto 已更快。
3. resvg 0.48.1、image 0.25.10 可以分别验证，避免一次改动多条像素路径后无法定位差异。字体栈升级需要和文本/图形依赖一起协调；fontdb、ICU 升级不作为本文主要 issue 的解决方案。
4. Vello 0.11 + wgpu 30.0.1 + 官方 egui-wgpu 0.36.2 应作为一个图形栈迁移单独验证。主要已知问题仍需本地兼容代码；另有未关闭的 Windows [wgpu #9937](https://github.com/gfx-rs/wgpu/issues/9937) 报告 29→30 后 present 耗时增加，属于需要本机等条件回归的风险，不能据此断定 Torto 会发生同样退化。

## 2026-09-10 上游版本核查（历史）

本次对照 `Cargo.lock`、crates.io 的最新稳定版、GitHub issue/PR 状态、发布说明和相关源码核查；没有升级依赖，也没有移除本地兼容代码。以下结论是上游核查结果，不代表已经在 Torto 中验证新版行为。

### 可用版本

| 依赖 | 项目当前版本 | 最新稳定版 | 与本文问题有关的结论 |
| --- | --- | --- | --- |
| egui / epaint | 0.36.1 | [0.36.2](https://github.com/emilk/egui/releases/tag/0.36.2)，2026-09-08 | 新修复 `TextEdit` 忽略最小高度、细长倾斜矩形绘制；适合优先做补丁升级验证，但不等于已有兼容代码均可删除。 |
| egui_commonmark | 0.25.0 | [0.25.0](https://crates.io/crates/egui_commonmark/0.25.0) | 没有更新的稳定版，多行跨组件选区问题仍开放。 |
| Vello | 0.10.0 | [0.10.0](https://github.com/linebender/vello/releases/tag/v0.10.0) | 没有更新的稳定版，图片重放问题仍开放，unpacked bitmap mask 仍未支持。 |
| Parley | 0.11.1 | [0.11.1](https://github.com/linebender/parley/releases/tag/v0.11.1) | 没有更新的稳定版，两端对齐选区问题仍开放。 |
| Skrifa | 0.44.0；锁文件另含 0.42.1 | [0.47.0](https://crates.io/crates/skrifa/0.47.0)，2026-09-08 | 单独升级不能修复 Vello 的宋体绘制路径；Vello 0.10.0 仍依赖 Skrifa 0.44。 |
| wgpu | 29.0.4 | [30.0.1](https://github.com/gfx-rs/wgpu/releases/tag/v30.0.1)，2026-08-22 | 30.0.1 的 acquire fence 修复针对非 Windows，不能据此认定本文 Windows 黑帧问题解决。 |
| winit | 0.30.13 | [0.30.13](https://github.com/rust-windowing/winit/releases/tag/v0.30.13) | 另有 [0.31.0-beta.3](https://github.com/rust-windowing/winit/releases/tag/v0.31.0-beta.3)（2026-09-04），仍为预发行版。 |

### 已确认的新修复与仍需保留的兼容逻辑

| 本地问题 | 最新核查结果 |
| --- | --- |
| 显式高度的 TextEdit | 新版已合入 [#8420](https://github.com/emilk/egui/pull/8420)，修复忽略 `min_size.y`。旧的感知区域修复 [#7436](https://github.com/emilk/egui/pull/7436) 和 hint 对齐修复 [#8332](https://github.com/emilk/egui/pull/8332) 早于本次检查，当前 0.36.1 已包含。0.36.2 的默认对齐仍为 `LEFT_TOP`，因此显式 `.vertical_align(Center)` 仍是必要的设计设置。 |
| 紧凑圆角控件羽化角线 | [#8482](https://github.com/emilk/egui/pull/8482) 修复的是细长矩形快速路径忽略旋转角度，不是本文圆角边界场景的一一对应修复。[#2735](https://github.com/emilk/egui/issues/2735)、[#7424](https://github.com/emilk/egui/issues/7424) 仍开放；可升级后验证，但暂保留局部几何处理。 |
| Vello 图片再次出现时不显示 | [#1809](https://github.com/linebender/vello/issues/1809) 仍开放；0.10.0 释放图片 GPU 资源的修复不能替代图片重放修复。保留图集刷新。 |
| 宋体等嵌入点阵字体空白 | [Fontations #1639](https://github.com/googlefonts/fontations/issues/1639) 仍开放；[#1839](https://github.com/googlefonts/fontations/pull/1839) 已于 2026-04-20 合并，不是本次新增修复。[Vello 0.10.0](https://github.com/linebender/vello/blob/v0.10.0/vello/src/scene.rs) 及本次查看的 [主干对应路径](https://github.com/linebender/vello/blob/main/research/vello_research/src/scene.rs) 仍直接跳过 unpacked mask。保留字体过滤。 |
| Windows 全屏/IME 黑帧、放大窗口黑边 | [winit #3730](https://github.com/rust-windowing/winit/issues/3730)、[wgpu #5374](https://github.com/gfx-rs/wgpu/issues/5374) 仍开放，且仍无已确认覆盖 Torto 全屏/IME 场景的发布修复。winit beta.3 的键盘布局切换冻结、DPI HDC 泄漏修复是不同问题。保留模拟全屏、surface 恢复和原生背景处理。 |
| Parley 两端对齐选区过短 | [#396](https://github.com/linebender/parley/issues/396) 仍开放。保留选区宽度修正。 |
| Markdown 多组件选区覆盖行首 | [egui_commonmark #80](https://github.com/lampsitter/egui_commonmark/issues/80) 仍开放，暂无新版可解决该问题。保留不可选择的布局换行与本地表格布局。 |
| 离屏端点导致选区清空 | 核查 [egui 0.36.2 的 Label](https://github.com/emilk/egui/blob/0.36.2/crates/egui/src/widgets/label.rs) 和 [选区状态](https://github.com/emilk/egui/blob/0.36.2/crates/egui/src/text_selection/label_text_selection.rs)：可见区域检查和未遇到两端点时清空选区的逻辑仍存在。保留本地 Label 补丁。 |
| 虚拟列表底部抖动 | [#1787](https://github.com/emilk/egui/issues/1787)、[#3268](https://github.com/emilk/egui/issues/3268) 仍开放；[0.36.2 的 show_rows](https://github.com/emilk/egui/blob/0.36.2/crates/egui/src/containers/scroll_area.rs) 仍在末行越界时向前补首行。保留本地虚拟化。 |
| 控件 ID/矩形变化误报 | [#8343](https://github.com/emilk/egui/issues/8343)、[#8092](https://github.com/emilk/egui/issues/8092) 仍开放，0.36.2 发布说明没有对应修复。保留当前调试设置。 |
| 专注模式滚轮跨小节闪首图 | 本地输入时序问题，仍按下文已完成的本地修复维护，不属于等待依赖升级解决的项目。 |

### 升级优先级

1. 优先评估 egui/epaint 0.36.2：合并官方补丁到本地 `third_party/egui`、`third_party/egui-wgpu`，核对配套 egui crates 和锁文件，再运行输入框、选区、目录滚动、圆角与 DPI 回归。不能直接丢弃本地 fork。
2. 暂不为了这些问题单独升级 wgpu 30：除未确认解决目标问题外，[Vello 0.10.0 的清单](https://github.com/linebender/vello/blob/v0.10.0/Cargo.toml) 仍使用 wgpu 29；[wgpu 30](https://github.com/gfx-rs/wgpu/releases/tag/v30.0.0) 还改变了 surface 配置和 present API，需作为图形栈整体迁移评估。
3. Skrifa 0.47 和 winit 0.31 beta 继续跟踪；本次没有找到足以直接撤销字体过滤、Windows 窗口兼容或其他主要 workaround 的证据。

## Vello：重复渲染同一 `ImageData` 时图片只在首次出现

- 影响版本：`vello 0.9.0`、`vello 0.10.0`；0.10 发布说明未包含对应修复
- 上游状态：截至 2026-08-17 仍为 Open
- 上游问题：[linebender/vello#1809](https://github.com/linebender/vello/issues/1809)
- 本地位置：`apps/desktop/src/platform/gpu.rs` 中的 `render_reader_scene`，以及 `apps/desktop/src/reader/render/scene.rs` 中的 `ReaderScene`
- 回归测试：`every_scene_with_images_refreshes_the_vello_atlas`；各阅读模式的图片往返切换仍需人工检查；开发环境可检查 `render.reader_images action=refresh_atlas` 日志

### 表现

在专注模式切换段落，或在经典模式来回翻页后，返回包含图片的页面时图片可能不再显示；但图片占位、图注、正文以及放大预览仍然正常。这说明图片数据和命中区域仍存在，缺失发生在 Vello 的 GPU 图片缓存重放阶段。

### 原因

Vello 的持久图片图集会缓存 `ImageData` 与 GPU 纹理上传状态。上游 #1809 记录了同一个 `ImageData` 被后续场景再次使用时，没有错误但只在第一次渲染中出现的行为。Torto 的页面场景缓存会在所有阅读模式中复用已解析的图片数据；之前的兼容逻辑却只在专注模式启用，因此经典模式回放缓存页面时仍会触发相同的缓存生命周期问题。

### 当前规避方案

`ReaderScene::new` 根据场景实际引用的图片自动决定是否刷新图集，不再依赖阅读模式。只要待渲染场景包含图片，GPU 层就在调用 `render_to_texture` 前对这些图片执行 `mark_override_image_dirty`，强制 Vello 重新上传对应图集内容；无图片场景不做额外处理。构造器维持这一不变量，避免以后新增阅读模式或场景入口时再次漏掉刷新。

### 升级检查

1. 检查 #1809 是否关闭，并确认修复进入的 Vello 版本；不能只根据新版中“释放图片 GPU 资源”的改动判断问题已经解决。
2. 升级 Vello 后临时移除 `mark_override_image_dirty` 和 `ReaderScene::refresh_image_atlas` 路径。
3. 在同一张图片上分别以经典翻页、经典滚动和专注模式连续往返，跨小节往返并退出后重新进入，确认正文图片和放大预览始终一致。
4. 检查长时间阅读时的 GPU 内存占用，确认上游修复没有以保留所有图片纹理为代价。
5. 全部通过后删除本地图片图集刷新兼容逻辑，并删除本条记录。

## Vello/Skrifa：Windows 宋体等嵌入点阵字体的字形不显示

- 影响版本：`vello 0.10.0`、`skrifa 0.44.0`，Windows 自带 `simsun.ttc`（`SimSun`/`宋体`、`NSimSun`/`新宋体`）
- 上游状态：截至 2026-08-31，Vello 尚无完全对应的独立 issue，当前源码仍明确拒绝 unpacked bitmap mask；Fontations 的 EBDT/EBLC 跟踪问题仍为 Open
- 相关上游记录：[linebender/vello#641](https://github.com/linebender/vello/pull/641)、[googlefonts/fontations#1639](https://github.com/googlefonts/fontations/issues/1639)、[googlefonts/fontations#1839](https://github.com/googlefonts/fontations/pull/1839)，以及 [Vello 当前的未支持分支](https://github.com/linebender/vello/blob/7df2f0c5bf4dfbdeeb7515da9d563671773dfb3b/vello/src/scene.rs)
- 本地位置：`crates/layout/src/lib.rs` 中的 `has_embedded_bitmap_glyphs`、`LayoutEngine::available_reader_font_families` 和 `ReaderFontFamilies::repair_typography`；设置初始化与阅读会话打开时分别再次修复旧配置
- 回归测试：`unavailable_cjk_preference_is_repaired_to_a_validated_family`、`embedded_fonts_register_with_reader_family_names`，并需在 Windows 开发预览中检查中文正文

### 表现

在排版设置中选择“宋体”后，中文正文、标题等字形会全部消失，但英文、数字和部分标点仍可显示。Parley 的排版结果仍包含完整的中文 glyph id、advance 和选区几何，问题只出现在 Vello 绘制阶段。`宋体` 与规范族名 `SimSun` 都会复现，因此不是中文显示名或 TTC family alias 匹配失败。

### 原因

Windows 的 `simsun.ttc` 含有 EBDT/EBLC 嵌入点阵表。Vello 在检测到字体带位图 strike 后，会优先按当前字号读取 bitmap glyph；其 0.10 实现遇到不支持的 unpacked mask 会直接跳过该字形，不会继续使用同一字体中存在的矢量 outline。Fontations #1839 已为 Skrifa 增加 packed 与 byte-aligned mask 的统一解码能力，但 Vello 0.10 尚未接入该接口，源码仍保留 `Unpacked mask data in font not yet supported` 分支。关闭 Vello hinting 不会改变该选择路径，因而不能解决问题。

### 当前规避方案

正文字体枚举不再仅检查 family name、PANOSE 分类和中文 charmap。凡默认 face 包含 EBDT/EBLC、CBDT/CBLC 或 Apple 旧式 `bdat`/`bloc` 嵌入点阵表的字体，均保守地从排版页的衬线、无衬线、中文和代码字体选项中排除。已经保存的不兼容或当前不可用字体会按对应类别恢复到经过验证的内置默认字体；阅读会话还会在首次分页前再次修复，避免旧配置直接产生空白正文。界面字体走 egui 自身的字体管线，不受此排版字体筛选影响。

### 升级检查

1. 检查 Fontations #1639 是否关闭，以及 #1839 的 mask 解码 API 是否已经进入 Vello 使用的 Skrifa 版本。
2. 检查 Vello 的 bitmap glyph 路径是否移除了 unpacked mask 拒绝分支，并在位图解码失败时回退到 outline。
3. 升级后临时移除 `has_embedded_bitmap_glyphs` 过滤，分别选择 `宋体`、`SimSun`、`新宋体` 和其他带 EBDT/EBLC 的系统字体。
4. 在 12–28 px 的全部可配置字号、浅色/深色主题及经典/专注模式下检查中英文、标点、粗体和斜体。
5. 所有字体均能绘制且不再出现空白字形后，才删除保守过滤与旧配置修复逻辑，并更新或删除本条记录。

## Torto/egui：专注模式滚轮跨小节时短暂闪现目标小节首图

- 影响版本：Torto `0.3.2` 开发版的专注模式
- 上游状态：本地输入处理时序缺陷，不需要等待 egui 或 Vello 上游修复；已于 2026-08-17 在工作区修复
- 本地位置：`apps/desktop/src/reader/egui_view.rs` 中的 `focus_wheel_interaction`，`apps/desktop/src/reader/ui_controller.rs` 中的 `apply_pending_focus_wheel_turn`
- 回归测试：目前需要人工检查滚轮与方向键跨小节的一致性

### 表现

使用方向键跨小节时页面定位正常；使用鼠标滚轮从后一小节返回前一小节末尾时，会先短暂显示目标小节的第一张图片，再定位到最后一个可激活单元。例如从 `Appendix: Dealing with Pests` 向上滚回 `Tillandsia` 末尾时，会闪现 `Tillandsia` 的第一张大图。

### 原因

方向键在 `DesktopReader::ui` 开始阶段处理，正文绘制前就完成阅读单元切换、焦点单元重建和末尾定位。鼠标滚轮原本在正文 viewport 的回调尾部直接切换小节，此时旧布局和纹理已经完成绘制；GPU 会先观察到新的阅读单元，但新的焦点单元和目标偏移要到下一帧才重建，因此暴露了一帧目标小节的默认起始位置。

### 当前修复

滚轮达到翻页阈值后不再在 viewport 回调中直接切换，而是记录 `pending_focus_wheel_turn` 并请求下一帧重绘。下一帧在布局和正文绘制之前调用 `apply_pending_focus_wheel_turn`，使滚轮与方向键遵循相同的状态更新顺序，再进入目标小节末尾。

### 回归检查

1. 分别用滚轮和方向键执行“下一小节”和“上一小节”，确认两种输入最终激活同一个单元。
2. 从后一小节向上滚回包含首图的前一小节末尾，录屏逐帧检查是否仍出现首图闪帧。
3. 检查高于视口的长段落，确认滚轮仍会先在段落内部滚动，到达边界后才跨单元。
4. 打开目录、带历史数据的段落聊天框和图片预览，确认滚轮仍只作用于当前前景控件。

## winit/wgpu/Windows：原生全屏切换及输入期间出现黑帧

- 影响版本：`winit 0.30.13`、`wgpu 29.0.4`（由清单中的 `29.0.3` 版本要求解析得到），Windows 10/11
- 上游状态：截至 2026-08-17，尚未找到与“无边框全屏 + IME/文本输入”完全一致的上游 issue；现象涉及 winit 的 Win32 全屏窗口状态、DWM 和 wgpu flip-model surface 的组合行为
- 相关上游讨论：[rust-windowing/winit#3730](https://github.com/rust-windowing/winit/issues/3730)（Windows 窗口装饰控制）；surface 在 Windows 合成状态变化时的相近问题另见 [gfx-rs/wgpu#5374](https://github.com/gfx-rs/wgpu/issues/5374)
- 本地位置：`apps/desktop/src/platform/application.rs` 中的 `toggle_fullscreen`、`compositor_fullscreen_bounds`，以及 `apps/desktop/src/platform/gpu.rs` 中的 `acquire_surface_frame` 和 `render`
- 回归测试：`compositor_fullscreen_overscans_the_monitor_by_one_pixel`；输入与显卡合成行为仍需人工检查

### 表现

在 Windows 上按 `F11` 进入或退出全屏时，窗口可能短暂整屏变黑。进入全屏后，在批注输入框或 AI Chat 输入框中打字、唤起输入法候选窗口时也可能再次出现黑帧。问题与具体书籍和输入框实现无关，普通窗口状态下通常不出现。

### 原因

Windows 上调用 winit 的原生 `set_fullscreen(Borderless)` 不仅会改变窗口边框和尺寸，还会让窗口进入由任务栏与 DWM 识别的全屏合成路径。IME 候选窗等额外原生窗口出现时，这条路径可能令 wgpu 的 flip-model surface 在相邻帧间变为 `Outdated` 或 `Lost`；如果本帧未能立即重新配置并呈现完整内容，DWM 会短暂显示黑色后备画面。

### 当前规避方案

Windows 不再调用原生 `set_fullscreen`，而是保存原窗口位置、尺寸和最大化状态，移除装饰后将普通窗口扩展到显示器边界，并在退出时完整恢复。边界额外外扩一个物理像素，避免 DWM 在屏幕边缘露出缝隙。每次渲染前都按当前客户区尺寸重新检查 surface；首次获取遇到 `Outdated` 或 `Lost` 时立即重新配置并在同一帧重试。其他平台继续使用 winit 原生无边框全屏。

### 升级检查

1. 检查 winit 是否提供不会切换 Windows 特殊全屏合成状态的独立装饰/铺满屏幕 API，并搜索是否新增对应 IME 黑帧 issue。
2. 检查 wgpu/DXGI surface 在全屏、IME 子窗口出现和 `Outdated`/`Lost` 恢复方面的更新。
3. 临时恢复 Windows 原生 `set_fullscreen`，连续切换 `F11`，并分别在批注和 AI Chat 输入框中使用中英文输入法输入。
4. 在多显示器、不同 DPI 和窗口原本已最大化的状态下验证进入与退出全屏，确认无黑帧且窗口位置能够恢复。
5. 上游路径稳定后，才移除 Windows 模拟全屏和 surface 同帧重试逻辑，并删除本条记录。

## wgpu/Windows：窗口放大时新暴露区域短暂显示黑色

- 影响版本：`wgpu 29.0.4`（由清单中的 `29.0.3` 版本要求解析得到），Windows 10/11 的 DX12/Vulkan surface
- 上游状态：截至 2026-08-17 仍为 Open
- 上游问题：[gfx-rs/wgpu#5374](https://github.com/gfx-rs/wgpu/issues/5374)；相近历史问题见该 issue 引用的 [#3868](https://github.com/gfx-rs/wgpu/issues/3868)、[#3756](https://github.com/gfx-rs/wgpu/issues/3756) 和 [#1168](https://github.com/gfx-rs/wgpu/issues/1168)
- 本地位置：`apps/desktop/src/platform/application.rs` 中的 `render_window_state`、`WindowEvent::Resized` 和 `WindowEvent::ScaleFactorChanged`，`apps/desktop/src/platform/gpu.rs` 中的 `resize` 和 `render`，以及 `crates/windows-window-background`
- 回归测试：surface 尺寸和背景色逻辑由单元测试覆盖；DWM 呈现时序仍需人工检查

### 表现

拖动窗口边缘放大、点击右上角最大化或通过 `F11` 扩大窗口时，新增加的客户区可能先显示黑色，再被下一帧应用界面覆盖。较早的处理还会先横向拉伸旧画面、再纵向完成布局，视觉上像内容被短暂拉长。缩小窗口通常不容易看到同样的问题。

### 原因

Windows 的 DWM 可以在应用排队的 `RedrawRequested` 得到处理之前，先按新的客户区尺寸合成窗口。此时 wgpu swapchain 仍保存旧尺寸或尚未提交新尺寸的完整帧，DWM 只能拉伸旧帧，或者暴露非透明窗口默认的黑色客户区。阅读器重新分页和构建场景所需的时间会放大这个窗口期。该现象与上游 #5374 对 Windows DX12/Vulkan surface 的描述一致。

### 当前规避方案

原生 Windows 客户区后备背景跟随当前主题色，避免 swapchain 尚未覆盖的像素使用系统黑色默认值。收到 `Resized` 或 `ScaleFactorChanged` 后，立即更新 surface，并在该事件处理中同步构建和提交完整 UI 帧，而不是只请求稍后的重绘或仅提交一张纯背景帧。正常渲染开始时也会再次以窗口当前尺寸校准 surface，并在提交前调用 `pre_present_notify`。

### 升级检查

1. 检查 wgpu #5374 及其关联问题是否已有 Windows DX12/Vulkan 修复，并确认进入的版本。
2. 升级 wgpu/winit 后，临时移除尺寸事件中的同步完整渲染，恢复普通的 `request_redraw` 路径。
3. 分别拖动四边与四角、点击最大化/还原、切换 `F11`，并在浅色和深色主题下录屏逐帧检查。
4. 在 100%、125%、150% 和 200% DPI，以及跨不同 DPI 显示器拖动窗口时，确认没有黑色新区域、旧帧拉伸或横纵分阶段变化。
5. 上游行为稳定后，才移除同步 resize 呈现和原生背景兼容层，并删除本条记录。

## Parley：两端对齐文本的选区宽度不足

- 影响版本：`parley 0.11.1`
- 上游状态：截至 2026-08-17 仍为 Open
- 上游问题：[linebender/parley#396](https://github.com/linebender/parley/issues/396)
- 本地位置：`crates/renderer/src/lib.rs` 中的 `ShapedTextRegion::selection_rects`
- 回归测试：`selection_covers_the_visual_width_of_justified_middle_lines`

### 表现

跨多行选择两端对齐的正文时，首行和末行通常正常，中间整行的高亮矩形会短于实际文字，导致行尾文字没有被高亮。

### 原因

Parley 会把两端对齐产生的额外空白宽度加入字簇 advance，但 `LineMetrics::advance` 仍保留调整前的宽度。`Selection::geometry_with` 对选区中间行直接使用该值，因此返回了过短的矩形。

### 当前规避方案

对于被完整选择的行，渲染器根据换行原因修正 Parley 返回的选区矩形：普通自动换行直接使用正文行宽，段落末行、显式换行及超长内容产生的紧急换行仍按调整后的字簇和行内盒计算实际文字宽度。首尾部分选择和非两端对齐文本仍沿用 Parley 原始几何。

### 升级检查

1. 确认上游问题已关闭，并找到修复进入的 Parley 版本。
2. 升级依赖后临时移除 `selection_rects` 中带上游链接的兼容逻辑。
3. 运行 `cargo test --locked -p rebook-renderer selection_covers_the_visual_width_of_justified_middle_lines` 和 `cargo test --locked -p rebook-renderer wrapped_mixed_text_uses_line_width_while_the_last_line_stays_content_sized`。
4. 使用包含长英文两端对齐段落的真实 EPUB 检查跨行选择。
5. 全部通过后删除兼容逻辑，并删除本条记录。

## egui/epaint：全局羽化导致紧凑圆角控件出现角线

- 影响版本：`egui 0.36.1`
- 上游状态：截至 2026-08-17，相关问题仍为 Open；上游尚无与 Torto 紧凑图标按钮完全相同的最小复现
- 相关上游问题：[emilk/egui#2735](https://github.com/emilk/egui/issues/2735)、[emilk/egui#7424](https://github.com/emilk/egui/issues/7424)
- 本地位置：`apps/desktop/src/ui/mod.rs` 中的 `configure_tessellation`、`painted_icon_button` 和 `paint_compact_rounded_background`
- 回归测试：`rounded_controls_keep_pixel_snapping_and_antialiasing`、`compact_rounding_contains_the_feathering_fringe`

### 表现

全局启用羽化后，小尺寸圆角图标按钮在 hover 或选中状态下可能在角落留下短斜线或残余边角。直接关闭羽化虽然能消除角线，却会让选择框、单选按钮和小圆角控件重新出现明显锯齿，尤其是在 Windows 100% DPI 下。

### 原因

epaint 的羽化由全局 tessellation 选项控制，当前不能针对单个 shape 选择是否使用。圆角路径的羽化带会向路径内外各扩展半个羽化宽度；紧凑控件的路径刚好落在分配矩形边界时，外侧碎片可能与裁剪、相邻背景或像素取整共同形成可见角线。相关上游 issue 还记录了羽化 tessellator 在其他几何形状上产生线状伪影的问题，但 Torto 的具体圆角场景尚未有一一对应的上游 issue。

### 当前规避方案

保留一像素全局羽化和矩形像素对齐，避免整个界面的圆角退化。紧凑图标按钮不再使用 egui 原生按钮的多层 frame/stroke，而是绘制单层背景；先把外边界对齐到物理像素，再将实际圆角路径向内缩半个羽化宽度，并对这个已经对齐的 shape 关闭二次 `round_to_pixels`。

### 升级检查

1. 检查上游是否提供按 shape 控制羽化的 API，或是否修复圆角/多边形羽化伪影。
2. 升级 egui 后，尝试移除 `paint_compact_rounded_background`，恢复普通圆角背景或原生按钮 frame。
3. 运行两个圆角回归测试。
4. 在 Windows 100%、125%、150%、175% 和 200% DPI 下检查 hover、选中和透明状态，确认既无角线也无圆角锯齿。
5. 全部通过后删除局部几何兼容代码，并删除本条记录。

## egui_commonmark/egui：跨组件多行选区覆盖行首内容

- 影响版本：`egui_commonmark 0.25.0`、`egui 0.36.1`
- 上游状态：截至 2026-08-17 仍为 Open；`egui_commonmark 0.25.0` 已正式支持 `egui 0.36`，但多组件选区问题尚未修复
- 上游问题：[lampsitter/egui_commonmark#80](https://github.com/lampsitter/egui_commonmark/issues/80)；布局限制另见 [emilk/egui#4378](https://github.com/emilk/egui/issues/4378)
- 本地位置：`third_party/egui_commonmark_backend/src/elements.rs` 中的 `newline`，以及 `apps/desktop/src/reader/chat_markdown.rs` 中的 `show_markdown_table`
- 回归测试：`markdown_table_row_height_follows_the_tallest_wrapped_cell`；列表选区需人工检查

### 表现

跨多个 Markdown 组件选择 AI 回复时，纯布局换行也会被当成可选文字，并在下一行开头绘制一个选区矩形。列表编号和项目符号是独立绘制的图形，该矩形可能覆盖它们。上游报告还记录了多行选择吞掉每行首字符的问题，表格内更明显。表格使用 `egui::Grid` 时，较短单元格的背景和边框也不会自动撑到同一行中最高单元格的高度。

### 原因

egui 的多组件文字选择按各个 `Label` 独立生成选区网格，无法识别 egui_commonmark 用来驱动布局的空换行不是文档内容。表格方面，立即模式布局在绘制单元格时尚不知道该行最终最大高度；`Grid` 之后虽然会统一行布局高度，已经绘制的 `Frame` 不会回填。

### 当前规避方案

将 egui_commonmark 的纯布局换行标记为不可选择，真实文本仍保留跨组件选择和复制。AI 表格不再依赖 `Grid` 回填单元格：先按列宽测量每个单元格的换行高度，取整行最大值，再用相同高度的显式矩形绘制该行所有背景和边框。

### 升级检查

1. 检查上游 #80 是否已修复，并确认修复所需的 egui/egui_commonmark 版本。
2. 升级后临时恢复可选择的布局换行，跨多段、多级有序/无序列表拖动选择，确认行首不再被覆盖。
3. 尝试将 AI 表格恢复为上游表格实现，检查长短文本混排、引用链接和窄侧栏换行。
4. 运行 `cargo test -p rebook-desktop markdown_table_row_height_follows_the_tallest_wrapped_cell`。
5. 全部通过后删除本地兼容代码，并删除本条记录。

## egui：滚动时离屏端点导致跨组件文字选区被清空

- 影响版本：`egui 0.36.1`
- 上游状态：截至 2026-08-17，egui 0.36.1 源码仍包含该清理逻辑，尚未找到专门跟踪此行为的 issue
- 上游代码：[`LabelSelectionState::on_end_pass`](https://github.com/emilk/egui/blob/0.36.0/crates/egui/src/text_selection/label_text_selection.rs)
- 本地位置：`third_party/egui/src/widgets/label.rs` 中的 `Label::ui`

### 表现

在 AI Chat 的长回复中从视口边缘继续拖动选区时，内容区虽然会自动滚动，但只要选区的起点或终点滚出可见区域，整个选区就会被取消，无法继续向上或向下扩展。

### 原因

`Label::ui` 默认只会把可见的标签提交给跨组件选区状态。滚动后，离屏端点所在的标签仍参与 `ScrollArea` 布局，却不会更新选区状态；`LabelSelectionState::on_end_pass` 在一帧内没有同时遇到两个端点时会主动清空选区，以规避虚拟化列表中的位置错乱。

### 当前规避方案

本地接管 `egui 0.36.1`：只要仍存在跨标签选区，`Label::ui` 就继续将裁剪区外的可选择标签提交给选区状态。标签和高亮仍受原有 painter 裁剪，不会绘制到滚动视口之外；已完成的选区在松开鼠标后也能保留并复制。

### 升级检查

1. 检查上游 `Label::ui` 与 `LabelSelectionState::on_end_pass` 是否已经支持离屏端点，或是否新增对应 issue。
2. 升级 egui 后临时移除 `third_party/egui` 与 `[patch.crates-io]` 覆盖。
3. 在长 AI 回复中从开头拖到视口顶部或底部，确认内容持续滚动、选区持续扩展。
4. 松开鼠标后反向滚动，确认离屏选区仍保留，并验证 `Ctrl+C` 能复制完整内容。
5. 全部通过后删除本地 egui 副本与本条记录。

## egui：`ScrollArea::show_rows` 在列表底部抖动

- 影响版本：`egui 0.36.1`
- 上游状态：截至 2026-08-17 仍为 Open
- 上游问题：[emilk/egui#1787](https://github.com/emilk/egui/issues/1787)；程序化定位限制另见 [emilk/egui#3268](https://github.com/emilk/egui/issues/3268)
- 本地位置：`apps/desktop/src/reader/egui_view.rs` 中的 `stable_virtual_row_range` 和 `DesktopReader::toc`
- 回归测试：`virtual_toc_range_does_not_backfill_rows_at_the_bottom_boundary`

### 表现

目录滚动到底部后点击目录项，列表可能在相邻帧间上下抖动一次。长目录更容易观察到，但问题与目录层级和条目数量本身无关。

### 原因

`ScrollArea::show_rows` 会根据视口计算首尾可见行；当末行超过总行数时，它会把尾行截断，并向前移动首行以维持原范围长度。视口底边在行边界附近发生微小变化时，首行会在两个值之间来回切换，进而改变子 UI 的布局范围并产生抖动。这与上游 #1787 的复现一致。

### 当前规避方案

目录保留 `ScrollArea::show_viewport`，但使用本地固定行高虚拟化：尾行只截断到总行数，不再向前补行，因此相同首行在底部边界两侧保持稳定。只有可见行会被创建和绘制。活动目录项不在视口内时，使用合成的目标矩形调用 `scroll_to_rect`，继续沿用 egui 的滚动定位与动画。

### 升级检查

1. 确认 #1787 已关闭，并找到修复进入的 egui 版本。
2. 升级依赖后尝试把目录恢复为原生 `show_rows`，保留正常的活动项定位。
3. 运行 `cargo test -p rebook-desktop virtual_toc_range_does_not_backfill_rows_at_the_bottom_boundary`。
4. 使用两千项以上的真实目录，在底部连续点击当前项、相邻项和远端项，确认底部不抖动且远端定位动画正常。
5. 全部通过后删除本地虚拟化兼容代码，并删除本条记录。

## egui：合法布局触发控件 ID/矩形变化误报

- 影响版本：`egui 0.36.1`
- 上游状态：截至 2026-10-09，#8092 已由 PR #8316 在 2026-10-05 修复并关闭，但尚未进入稳定版 0.36.2；#8343 仍为 Open。合入修复并完成本地回归前继续保留兼容设置。
- 上游问题：[emilk/egui#8343](https://github.com/emilk/egui/issues/8343)、[emilk/egui#8092](https://github.com/emilk/egui/issues/8092)
- 本地位置：`apps/desktop/src/ui/mod.rs` 中的 `configure`

### 表现

在 debug 构建中，右到左子布局、虚拟化列表或动画区域可能被 `warn_if_rect_changes_id` 判断为 ID 不稳定，界面会出现明亮的红色边框并输出警告。实际控件状态和交互并没有发生串用。

### 原因

egui 的调试检查只看到相同屏幕矩形在不同 pass 或帧中对应了不同 ID，无法区分真正的 ID 不稳定和虚拟化、反向布局导致的合法矩形复用。

### 当前规避方案

仅在 debug 构建中关闭 `style.debug.warn_if_rect_changes_id`。这会同时关闭该项 ID 稳定性诊断，因此新增复杂动态布局时需要通过稳定 ID、交互状态和滚动行为测试补足检查。

### 升级检查

1. 确认两个上游问题的修复状态以及修复进入的 egui 版本。
2. 升级依赖后重新启用 `warn_if_rect_changes_id`。
3. 检查右侧工具栏、虚拟化列表、侧栏动画和滚动区域是否仍出现红框或误报警告。
4. 运行桌面端测试，并手动验证控件状态不会在相邻行或相邻帧之间串用。
5. 确认无误报后删除关闭诊断的兼容设置，并删除本条记录。

## egui：显式高度 `TextEdit` 的垂直对齐与感知区域问题

- 影响版本：`egui 0.36.1`
- 上游状态：默认顶部对齐仍属于当前 API 行为；相关感知区域 bug 已由上游修复，0.36 另行修复了 hint 文本未遵循水平/垂直对齐的问题
- 上游问题：[emilk/egui#7433](https://github.com/emilk/egui/issues/7433)、修复 [emilk/egui#7436](https://github.com/emilk/egui/pull/7436)；hint 对齐修复 [emilk/egui#8332](https://github.com/emilk/egui/pull/8332)
- 本地位置：`apps/desktop/src/reader/egui_view.rs` 中的 `pdf_toc_editor_table`、`centered_assistant_text_edit` 和搜索输入框，以及 `apps/desktop/src/shelf/mod.rs` 中的 `shelf_search_field`

### 表现

将单行 `TextEdit` 放进高度高于默认文本行高的固定矩形时，未指定垂直对齐会沿用 `LEFT_TOP`，文字视觉上偏上，与同一行的按钮、数值输入框不居中。较早的 egui 实现即使指定了垂直对齐，点击和拖动的感知区域仍可能停留在控件顶部；后一个问题是上游确认的 #7433。

### 原因

`TextEdit` 的默认 `Align2` 是 `LEFT_TOP`，`ui.add_sized` 只扩大控件矩形，不会自动把单行文字改为垂直居中。上游旧实现还只在水平方向保存文本偏移量，命中测试没有纳入垂直对齐偏移；#7436 将偏移扩展为二维并同步修复了交互区域。

### 当前规避方案

所有放入显式高度容器、且设计上要求居中的单行输入框都显式调用 `.vertical_align(egui::Align::Center)`。不要只依赖外层 `horizontal_centered` 或 `add_sized`，它们只控制控件矩形，不改变 `TextEdit` 内部文字对齐。AI 输入框在升级到 0.36 后恢复使用原生 `hint_text`，其提示文字现在会遵循同一个垂直对齐设置。

### 升级检查

1. 检查 `TextEdit` 的默认垂直对齐是否发生变化，以及 #7436 的二维偏移逻辑是否仍存在。
2. 检查书架搜索、阅读器搜索、AI 输入框和 AI 目录编辑弹窗中文字、光标、点击与拖动选区是否位于同一垂直位置。
3. 在 Windows 100%、125%、150% 和 200% DPI 下重复检查单行输入框。
4. 只有上游默认行为能够满足设计时，才移除显式 `vertical_align`；否则保留该声明并更新本文版本信息。
