# 核心依赖已知问题

- 最近更新：2026-10-29
- 范围：当前工作区仍需跟踪的依赖问题、兼容措施和待验证事项；版本以 `Cargo.lock` 为准。
- 维护方式：直接更新各条目的当前结论，不追加按日期排列的排查日志、旧版本快照和已完成的升级过程。已解决且无需兼容措施的问题删除。

只有上游修复进入实际使用版本，并在移除对应兼容措施后通过相关回归，才撤销本地处理。上游 issue 关闭或依赖版本号更新本身不能替代验证。

## 字体依赖多版本：暂缓修改

当前仅升级已发布的组件，仍不能统一 Skrifa 和 fontdb。按当前决定先记录，暂不修改这些依赖或引入新的兼容补丁。

### 当前依赖来源

| 依赖版本 | 引入方 |
| --- | --- |
| Skrifa 0.44.0 | egui_system_font_provider / epaint 0.37.0、glifo 0.4.0、parley_engine 0.12.0、usvg 0.48.1、Vello / vello_encoding 0.11.0 |
| Skrifa 0.46.2 | hayro-interpret 0.8.0 |
| Skrifa 0.48.0 | Torto 桌面端、formats、layout 的直接依赖 |
| fontdb 0.23.0 | mermaid-rs-renderer 0.3.1、ratex-unicode-font 0.1.14、system-fonts 0.1.2 |
| fontdb 0.24.0 | Torto 桌面端、formats 的直接依赖，以及 usvg 0.48.1 |

Fontique 已统一为 0.12.0；系统字体提供器的本地改动仅调整依赖清单，继续保留后台字体查询、缺字回退和原生彩色 Emoji。维护说明见 [provider 补丁](../third_party/egui_system_font_provider/PATCH.md)。

### Skrifa 的上游升级进展

统一候选目标为 Skrifa 0.49 / read-fonts 0.46，需要多个组件同步适配。

| 组件 | 当前可用进展 |
| --- | --- |
| Parley / Engine | [#898](https://github.com/linebender/parley/pull/898) 已于 2026-10-28 合并，升级到 Skrifa 0.49、read-fonts 0.46、HarfRust 0.15；尚未发布，正式版仍为 0.12.0。 |
| Vello / glifo | [#1996](https://github.com/linebender/vello/pull/1996) 尚未合并；已发布版本仍使用 Skrifa 0.44。 |
| resvg / usvg | [#1156](https://github.com/linebender/resvg/pull/1156) 尚未合并；已发布的 [0.49.0](https://github.com/linebender/resvg/blob/v0.49.0/crates/usvg/Cargo.toml) 仍使用 Skrifa 0.44，升级至该版不能减少重复。 |
| Hayro | [#1606](https://github.com/LaurenzV/hayro/pull/1606) 尚未合并；hayro-interpret 0.8.0 仍使用 Skrifa 0.46。 |
| egui / epaint / 系统字体提供器 | 已发布的 0.37.0 仍使用 Skrifa 0.44；[主分支依赖说明](https://github.com/emilk/egui/blob/main/Cargo.toml) 仍受 glifo 版本约束。 |

后续等待这一轮上游升级发布，再协调 Torto 的直接依赖及现有本地补丁。提前采用未发布改动需要维护多份补丁，当前不实施。升级后须回归正文排版、PDF、SVG、中文和 Emoji，并实测 Release 体积；上游已提示新版字体依赖可能增大体积，不能根据版本数量推算收益。

### fontdb 的统一条件

[Mermaid](https://github.com/1jehuang/mermaid-rs-renderer/blob/master/Cargo.toml)、[RaTeX Unicode 字体](https://github.com/erweixin/RaTeX/blob/main/crates/ratex-unicode-font/Cargo.toml) 和 [system-fonts](https://github.com/yijehyung/system-fonts/blob/main/Cargo.toml) 仍要求 fontdb 0.23；RaTeX 升级到 0.1.15 也不能消除该版本。尚未找到这三个组件迁移到 0.24 的现成发布版本。

候选方案是同时调整三个组件的依赖清单到 0.24。源码检查显示所用 API 基本保留，但尚未编译或运行验证；本轮不实施。未来验证重点是 Mermaid 文字测量、公式中文回退，以及 Windows/macOS 字体发现。RaTeX 与 Mermaid 仍直接使用 ttf-parser，统一 fontdb 不代表可同时移除 ttf-parser。

## Vello：缓存场景重放时图片缺失

- 当前状态：Vello 0.11.0 仍保留本地处理；上游 [#1809](https://github.com/linebender/vello/issues/1809) 开放。
- 现象：往返翻页或切换专注段落后，图片占位、图注和预览正常，但正文图片可能消失。
- 当前处理：`ReaderScene::new` 按场景是否含图片设置刷新标记；`apps/desktop/src/platform/gpu.rs::render_reader_scene` 在重放前调用 `mark_override_image_dirty`，覆盖全部阅读模式。
- 撤销条件：上游修复进入使用版本后，移除刷新路径，验证经典翻页、滚动、专注模式的图片往返显示及 GPU 内存。保留回归 `every_scene_with_images_refreshes_the_vello_atlas`。

## Vello/Skrifa：Windows 宋体等点阵字体字形空白

- 当前状态：Vello 0.11.0 仍有 `Unpacked mask data in font not yet supported` 分支；[Fontations #1639](https://github.com/googlefonts/fontations/issues/1639) 开放。单独升级 Skrifa 或修复 sbix 定位不能证明该路径已解决。
- 现象：宋体等含嵌入点阵表的字体可完成排版，但中文在绘制时消失。
- 当前处理：`crates/layout/src/lib.rs::has_embedded_bitmap_glyphs` 保守过滤 EBDT/EBLC、CBDT/CBLC、bdat/bloc 字体，并修复不可用的已保存正文字体配置；界面字体管线独立。
- 撤销条件：Vello 接通相应 mask 解码或可靠 outline 回退，并在 Windows 对宋体/新宋体、12–28 px、粗斜体、两种主题和阅读模式验证通过，再删除过滤及对应配置修复。

## Windows：全屏、输入法和窗口缩放时的黑帧

- 当前状态：winit 0.30.13 / wgpu 30.0.1 继续保留本地措施。[winit #3730](https://github.com/rust-windowing/winit/issues/3730)、[wgpu #5374](https://github.com/gfx-rs/wgpu/issues/5374) 开放，属于相关窗口/呈现问题，未证实是 Torto 全屏 + IME 场景的完全相同根因。
- 当前处理：`apps/desktop/src/platform/application.rs` 在 Windows 使用普通无装饰窗口模拟全屏，保存并恢复原窗口状态；尺寸/DPI 事件内提交完整 UI 帧。`platform/gpu.rs` 同帧重试失效 surface；`crates/windows-window-background` 让原生后备背景跟随主题。
- 撤销条件：对应上游路径修复后，分别验证 F11、批注/聊天中的中英文输入法、拖动缩放、最大化/还原、多显示器与不同 DPI；无黑帧、旧帧拉伸或窗口恢复异常后，才移除对应措施。不能用 macOS 启动检查替代 Windows 验证。

## Parley：两端对齐文本的选区宽度不足

- 当前状态：Parley / Engine 0.12.0 仍需本地选区修正；上游 [#396](https://github.com/linebender/parley/issues/396) 开放。
- 现象：跨行选择两端对齐正文时，中间完整行的高亮短于实际文字。
- 当前处理：`crates/renderer/src/lib.rs::ShapedTextRegion::selection_rects` 对完整行按换行原因修正宽度，部分选择仍使用上游几何。
- 撤销条件：移除修正后通过 `selection_covers_the_visual_width_of_justified_middle_lines`、`wrapped_mixed_text_uses_line_width_while_the_last_line_stays_content_sized`，并验证真实 EPUB 的跨行选择。

## egui/epaint：紧凑圆角控件的羽化角线

- 当前状态：egui / epaint 0.37.0 仍保留局部绘制；相关 [#2735](https://github.com/emilk/egui/issues/2735)、[#7424](https://github.com/emilk/egui/issues/7424) 开放，尚非 Torto 场景的一一对应复现。
- 当前处理：`apps/desktop/src/ui/mod.rs` 保留全局抗锯齿和像素对齐；`paint_compact_rounded_background` 对单层圆角背景内缩半个羽化宽度，避免角落残线。
- 撤销条件：上游修复后通过 `rounded_controls_keep_pixel_snapping_and_antialiasing`、`compact_rounding_contains_the_feathering_fringe`，并检查 Windows 多档 DPI 下的 hover、选中和透明背景。

## egui_commonmark/egui：跨组件及离屏文字选区

- 当前状态：egui_commonmark/backend 0.26.0、egui 0.37.0 继续保留处理；跨组件选区 [#80](https://github.com/lampsitter/egui_commonmark/issues/80) 开放。离屏端点问题由当前源码确认，尚无专门的上游跟踪 issue。
- 当前处理：backend 的纯布局换行不可选，避免选区覆盖列表标记；`reader/chat_markdown.rs` 先测量表格整行高度，再统一绘制单元格。`third_party/egui/src/widgets/label.rs` 在存在跨标签选区时继续登记离屏标签，绘制仍受视口裁剪。
- 撤销条件：上游可替代相应行为后，分别验证多段/多级列表选择、窄侧栏表格、拖动自动滚动、松开后保留及完整复制。表格回归为 `markdown_table_row_height_follows_the_tallest_wrapped_cell`。

## egui：目录底部抖动及程序化定位

- 当前状态：egui 0.37.0 仍使用本地虚拟化；[#1787](https://github.com/emilk/egui/issues/1787)、[#3268](https://github.com/emilk/egui/issues/3268) 开放。
- 当前处理：`apps/desktop/src/reader/egui_view.rs::stable_virtual_row_range` 截断尾行而不向前补行；目录使用 `show_viewport` 仅布局可见行，离屏活动项用目标矩形滚动定位。
- 撤销条件：恢复上游 `show_rows` 后，通过 `virtual_toc_range_does_not_backfill_rows_at_the_bottom_boundary`，并验证长目录底部点击、远端定位与滚动动画。

## egui：合法布局触发 ID/矩形诊断误报

- 当前状态：egui 0.37.0 已包含虚拟表格 [#8316](https://github.com/emilk/egui/pull/8316) 修复，旧 egui_extras 补丁已停用；右到左布局问题 [#8343](https://github.com/emilk/egui/issues/8343) 仍开放。
- 当前处理：`apps/desktop/src/ui/mod.rs::configure` 仅在 debug 构建关闭 `warn_if_rect_changes_id`，因此该项诊断不能用于发现其他 ID 不稳定问题。
- 撤销条件：上游覆盖剩余误报后重新开启诊断，验证右到左布局、虚拟列表、侧栏动画，以及相邻行/帧的控件状态稳定性。

## AI 网关：连接失败与完整目录识别仍待验证

- 当前状态：Rig / rig-reqwest 0.44.0 已负责宽容解码、原始条目和签名回传；旧 SDK 签名解析问题已解决，不再保留旧版清洗/恢复方案。Torto 仍在 `plugins/llm/history.rs` 校验端点、凭据、模型和消息内容，HTTP 层保留完整 JSON 转流式桥接。
- 当前处理：`plugins/pdf_toc/agent.rs` 只对明确的 502/503/504 `provider_connection_failed` 最多重试两次，等待 1 秒、2 秒，计入总截止时间；成功前不执行工具。认证、限流、解析及普通 5xx 不走此策略。
- 待验证：在实际 Bifrost 端点完成《The psychology of reading》整本目录识别，检查选页、工具参数和停滞保护；SDK 解码回归通过不等于整本工作流完成。网关提前断连仍需上游连接日志，不能据 SDK 升级删除有限重试。
- 回归重点：签名多轮回传、真正 SSE、端点/凭据/模型切换、结构化结果校验，以及 `connection_retry_*` 的分类、同轮重发和有界失败。

## 本地补丁维护入口

当前仍生效的依赖补丁以根目录 `[patch.crates-io]` 为准，具体维护和撤销条件见 [egui](../third_party/egui/PATCH.md)、[epaint](../third_party/epaint/PATCH.md)、[Markdown backend](../third_party/egui_commonmark_backend/PATCH.md)、[字体提供器](../third_party/egui_system_font_provider/PATCH.md) 和 [Hayro](../third_party/hayro/TORTO_PATCHES.md)。不在本文重复保存补丁升级历史。
