# Parley 0.12 / parley_engine 接入评估

核查与实施日期：2026-10-10。正式依赖已升级到 crates.io 原版 `parley 0.12.0` 和 `parley_engine 0.12.0`，移除 `third_party/parley` fork 与 Cargo patch。下方保留迁移前的接口比较和隔离探针结果，实际接入和回归证据见下一节。

## 已实施的调用链

`PreparedText`、renderer、命中测试和选区共同使用产品的 `text_layout` 接口，底层仍由官方 Parley builder 完成样式、字体选择与回退，不改成固定字体。原文与译文按逻辑段落分别设置方向、对齐，并保持原始 UTF-8 源范围；ruby 的额外绘制边界由产品行几何处理。行内图片和公式使用官方 baseline、vertical-align、真实盒子高度以及新的 line box/content bounds。

自定义断行通过 Engine 的 `Analyzer` / `Analysis` 获取合法 Unicode 断点；线程局部分析缓冲区可复用，超过 256 KiB 的输入在分析后释放缓冲区。优化器仅调整安全的单 glyph、LTR 簇间距时，复用已经塑形的 glyph，并同步绘制、光标、命中与选区几何。连字、多 glyph 组合簇和混合 RTL 等不安全情况继续走重塑形路径；尚未成熟的断行后 reshaping API 不作为依赖。

适配 0.12 的 Unicode scalar 断行计数、完整字素簇、字体归一化坐标、inline boxes 迭代器和字体指标。保留 renderer 对上游 #396 的选区宽度兼容，不将它误认为已经修复。

## 迁移前评估结论

值得迁移到新版，但应分阶段完成。优先使用 Parley 0.12 的官方方向、baseline、vertical-align 和字素接口，随后让自定义断行通过产品自己的行几何层使用 Engine。仅在当前 0.11.1 旁边增加一个 Engine 依赖，不能减少补丁，也不会让现有调用自动加速。

Parley 0.12 自身已使用 Engine。直接使用 Engine 可以让中文间距、标点压缩、双端对齐等位置调整脱离“修改样式 → 再次塑形”的路径；这是基于当前调用链和公开 API 的迁移判断，尚未在完整 Torto 排版中验证。Engine 不负责行高、对齐、分页、选区或字体回退，调用方需要拥有这些行为。

## 已验证版本和接口

- [Parley v0.12.0 发布说明](https://github.com/linebender/parley/releases/tag/v0.12.0)：2026-10-09 16:16 UTC 发布；crates.io 最新稳定版为 0.12.0。
- [Engine 0.12 源码和使用说明](https://github.com/linebender/parley/blob/v0.12.0/parley_engine/src/lib.rs)：公开 `Analyzer`、`Analysis`、`Shaper`、`ShapedText`、`FontSelector`；分析与塑形阶段必须使用同一个源字符串。
- Engine 的 Atom 同时尊重字素边界与塑形簇边界，可用来建立自定义断行项目；`ShapedCluster::is_safe_to_break_before` 能区分需要重新塑形的边界。
- 上游仍在开发断行后的重新塑形。当前版本没有可直接替换本地连字断点处理的完整 line reshaping API。
- Engine 的结果不能通过公开接口直接装入现有 `parley::Layout`：`LayoutData`、builder 的组装过程和高层字体选择适配器均为私有。接入需要调整现有 `PreparedText`、渲染器、命中测试与选区的共同数据接口。

## 本地补丁对应关系

与 crates.io 原版 0.11.1 比较，当前 fork 修改了 7 个 Rust 文件，新增 171 行、删除 2 行。这里只统计依赖 fork，不把产品自己的 Knuth–Plass、分页、字体策略或缓存算作补丁。

| 当前能力 | 0.12 可用替代 | 接入判断 |
| --- | --- | --- |
| `RangedBuilder::set_base_level` | 官方 `set_base_direction(BaseDirection)` | 可删除这一部分方向覆盖补丁；隔离探针确认纯 URL 也能声明 RTL。 |
| `reserve_inline_box_paint_bounds` | `InlineBox::baseline`、`vertical_align` 和新的 line box/content bounds | 图片和公式应在排版前声明真实盒子、baseline 与垂直对齐，渲染直接使用其位置。探针确认 50px 图片的上下边界及下一行不会重叠；仍需迁移现有公式/上下标回归，不能只删调用。 |
| `reserve_text_paint_bounds`（ruby） | 高层 Layout 没有对应公共 API | ruby 需要产品行几何层按真实基础文本范围扩展上下边界。不能改回零宽盒占位；此前 wrap 边界归属问题仍需覆盖。 |
| `set_ltr_ranges`（RTL 原文中的译文） | 段落级方向 API；Engine 也接受段落级方向 | 应分开分析逻辑原文/译文段落，并保留源偏移映射。公开 `Analysis` 不允许直接修改局部 bidi levels；整个合并字符串只设置一次 LTR/RTL 不等价。 |
| `align_with_left_ranges`（译文左对齐） | 高层 `Layout::align` 仍是整份 Layout 的对齐 | 分段布局后由产品行几何层分别对齐，可移出 fork。`last_line_alignment` 只控制末行，不能替代译文范围对齐。 |
| 两端对齐选区修正（renderer 中的兼容代码） | 没有可删除的已验证修复 | [#396](https://github.com/linebender/parley/issues/396) 仍开放；0.12 纯上游探针也返回过短的中间行矩形，继续保留。 |

现有 `break_next_with_length`、`set_prior_line_width`、`finish` 本来就是上游公开接口，不属于 fork 补丁。直接 Engine 接入的收益主要是减少反复塑形和依赖内部行几何，不应把这些调用算成“违规调用”或“可删除补丁”。

## 必须处理的兼容变化

隔离副本仅替换依赖、去掉 Parley patch 后，`cargo check -p rebook-renderer` 在排版层发现 **22 处**编译错误；编译尚未进入 renderer，不能将 22 当作整个客户端的迁移总数。

机械接口变化包括 `Cluster::first_style → style`、inline boxes 从切片变为迭代器、坐标改为 `NormalizedCoord`、`Run::metrics → font_metrics`、新增 InlineBox 字段和移除旧行指标。行为变化需要独立处理：

1. `break_next_with_length` 从旧版 cluster 计数改为字符数（Unicode scalar），盒子计 1；实际断点仍落在 Atom 边界。当前优化断行计划中的 cluster 数不能直接传入。探针中 `e + combining acute + x` 在新版是 2 个字素，传入 2 个字符却只消耗第一个字素；CRLF 与 emoji ZWJ 序列也改变了边界。
2. 新版 Cluster 是完整扩展字素，连字 advance 在其覆盖的字素之间分配。断行项目、脚注尾行修复、源范围和选择不能沿用旧的单字符假设。
3. 新行高遵循 CSS line box。分页应使用 line box；文字、选区和注音的可见范围应结合 content bounds。不能把删除的 ascent/descent 字段机械换成一个公共数值。
4. NBSP 不再作为尾部悬挂空白，全角空格会悬挂；旧 `trailing_whitespace` 与新 `hanging_advance` 不仅是改名。
5. out-of-flow 盒不再提供断行机会，带 letter spacing 的文字会禁用可选连字。当前连字断点和间距重塑形需用回归验证其真实效果。

## 隔离运行结果

探针已编译并运行成功：6 类文本的簇/断行差异、公开段落方向、inline box baseline、上游选区；3 个固定字体样例的 Engine 输出与 Parley 0.12 输出逐个比较 glyph id、x/y offset、advance，完全相同。glyph 通过官方 `ShapedSlice::shaped_cluster_glyphs` 遍历，包含以 inline 形式存储的单 glyph 簇。

性能样本取两本书之前使用的小节中的顶层文字段落，固定 Arial、24px，**去掉书籍样式、表格、图片、注音、自定义断行、绘制和 UI 加载**。3 个新进程，各运行 9 轮交替顺序，丢弃每进程前 2 轮后取 21 个样本的中位数；dev opt-level=1。

| 语料 | 0.11.1 高层 builder | 0.12 高层 builder | 0.12 Engine 固定字体 |
| --- | ---: | ---: | ---: |
| Thinking in Systems：165 段 / 54,704 字符 | 20.54 ms | 19.22 ms | 12.91 ms |
| The Chinese Computer：99 段 / 46,471 字符 | 17.32 ms | 15.93 ms | 10.92 ms |

高层 builder 的这个样本约快 6.4% / 8.0%。Engine 列还省掉高层样式解析、字体选择与布局数据组装，两者工作量不同，不能把约 37% 的差值当作产品预期加速。上述结果也不能换算成点击封面到正文显示的改善。完整接入仍需相同窗口、字体、阅读位置的生产路径测量。

## 建议实施顺序

1. 为 PreparedText 引入产品自己的行指标和 glyph 访问接口，保持源字符串、UTF-8 源范围、选区与 inline/ruby 归属明确；先由现有 Parley 实现，不同时改变断行规则。
2. 迁移 Parley 0.12 及其 Engine 依赖。方向和图片/公式位置使用公共 API，更新字符计数适配、字素遍历、字体坐标及 line box。原文/译文作为逻辑段落分别设置方向和对齐；ruby 的边界由共同的产品行几何层承担。回归通过后才能删对应 fork 修改。
3. 在同一数据接口下接入 Engine 的自定义断行：复用文字分析；从 Atom 建立 Knuth–Plass 项目；行内间距和对齐直接计算 glyph 位置，避免仅为 spacing 再次塑形。依赖当前字体选择/回退语义，不能用探针中的固定字体策略替代。
4. 重新塑形 API 未成熟时保留断行处的连字/复杂文字处理。回归覆盖真实两本书、Science of Beauty 图注/表格、RTL 双语、ruby、公式、脚注、组合字符、emoji 和跨行选择；随后重测打开阶段计时及内存，再决定是否完全移除 fork。

原始版本源码、171 行补丁 diff、探针、编译错误和 3 次运行报告位于 `tmp/parley-engine-20261010/`。该目录记录的是迁移前评估；正式接入验证产物位于 `tmp/parley-implementation-20261010/`。

## 正式接入验证

- layout 185 项、reader 69 项、renderer 31 项、desktop 806 项测试通过：共 1091 项通过、45 项沿用忽略。
- 《Thinking in Systems》26 小节、《The Chinese Computer》26 小节、《The Science of Beauty》103 小节，各覆盖统一版式 1216 / 700 px 与原书版式 700 px，共 465 个完整章节布局通过。检查行几何、UTF-8 源范围与簇范围，以及统一版式表格文本的可用宽度。
- 前两本书的 156 个新旧案例另外对照正文、UTF-8 源范围、样式片段、ruby / 引文 / 行内图片元数据和独立图片归属，忽略分页重复的同一正文块，内容一致；报告为 `content-report.json`。
- 空白占位行允许无文本源的原生 strut 范围；旧版亦有这种结构，不将它作为正文越界。新旧分页和浮点几何不要求逐值相同，因为 0.12 已改变行高与簇语义。
- RTL 双语、ruby 换行及边界命中、组合字符 / ZWJ / 国旗字符计数、连字与多 glyph 回退、间距调整后的光标与选区均有针对性回归。
- 正式 `torto.exe` 构建成功，真实 Thinking 书籍的启动、正文渲染、最小化与恢复检查通过。诊断程序仅放在 F 盘忽略目录中，不随产品源码保留。

完整打开测量使用原阅读位置、原设置、内置字体、1216×789 视口、DPI 1、GTX 1080 Vulkan。测试配置保留书库、设置和同步库，省略与打开链路无关的大体积语义搜索索引；每个版本每本书使用独立进程，进程内三次书架打开。因 F 盘空间不足，新版后半段构建设置 `CARGO_INCREMENTAL=0`，旧版可执行文件使用默认增量编译，代码生成参数不完全一致。因此以下原始耗时仅作诊断，不声称升级本身带来某个加速百分比；塑形调用次数是独立的行为证据。

| 章节 / 项目 | 0.11.1 旧构建 | 0.12 新构建 |
| --- | ---: | ---: |
| Thinking 第 12 小节：塑形调用次数 | 463 | 299 |
| Chinese 第 8 小节：塑形调用次数 | 553 | 456 |
| Thinking：书架请求到正文显示均值 | 652.96 ms | 518.83 ms |
| Chinese：书架请求到正文显示均值 | 729.69 ms | 627.06 ms |
| Thinking：塑形 / 断行均值 | 254.79 / 62.50 ms | 108.51 / 99.35 ms |
| Chinese：塑形 / 断行均值 | 226.82 / 55.71 ms | 106.23 / 87.70 ms |

断行时间包含新增的间距几何计算，不能将塑形阶段的下降全部理解为总工作减少。原始计时、调用次数及条件在 `tmp/parley-implementation-20261010/report.json`；三次测量不是首次冷启动的保证值。`cargo check --workspace` 和 layout / renderer 的 Clippy 检查通过，现有非阻断警告继续保留。
