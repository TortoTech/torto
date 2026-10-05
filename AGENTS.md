# Repository Instructions

## 桌面端内存与 CPU 性能

后续修改书架、统计总览/详情、阅读器、设置或后台任务时，应保留以下约束。它们来自已定位的性能问题，避免重新引入持续重绘、重复解码和无界缓存。

### 重绘与动画

- 闲置页面在布局和动画稳定后应停止请求重绘。不要在 `ui()`、弹窗或后台状态检查中无条件调用 `request_repaint()`，也不要仅因任务尚未完成而每帧重绘。
- `WindowEvent::RedrawRequested` 已经在处理当前重绘，不应因 egui-winit 的 `response.repaint` 再请求下一帧。重绘事件处理后不要落入通用的立即重绘分支；这两种情况都曾形成高 CPU 的循环。
- 保留 `platform/repaint.rs` 的调度机制：egui 回调发出时计算绝对截止时间，按累计 pass 序号过滤过期请求，合并重复请求。允许当前 pass 和前一个 pass 的请求，以保留布局稳定所需的后续帧。
- 后台任务通知与 egui 的帧请求分开处理。后台结果不能被帧序号过滤掉；事件循环应按最早截止时间使用 `WaitUntil`，没有待处理计时器时明确恢复 `Wait`。
- 加载动画使用 `ui::LoadingSpinner`：聚焦时约 30 FPS，失焦时约 10 FPS，屏幕外停止动画。精确间隔计时使用 `ui::request_repaint_in`，避免 egui 扣除 `predicted_dt` 后短延迟变成零，导致忙循环。
- 自动隐藏、通知过期等逻辑按截止时间唤醒；hover 内容、引用、锚点或可见性真正变化时才重置状态。有限动画和必要的短期等待可以使用定时重绘，但结束后必须停止。

### 后台任务与失效结果

- 数据库、文件系统检查、图片解码和缩略图磁盘读写放入后台；阻塞工作使用 `spawn_blocking`。UI 只读取已完成结果，不等待后台任务，也不在每帧重复提交同一请求。
- 云同步周期检查使用 `shelf/sync_monitor.rs` 的后台计时器；没有变化时保持静默，只有需要同步或刷新数据时通知 UI。保留阅读进度防抖、最大等待时间、失败重试和定期全量同步。
- 后台任务完成或失败后发送可靠的唤醒事件，先发布结果再唤醒。不要用常驻的 UI 轮询定时器替代完成通知。
- 使用任务 ID、generation 或配置快照过滤失效结果。切换书籍、账号/凭据、禁用同步、替换/删除封面或清理缓存后，旧任务不能恢复已移除的数据或纹理。注意中止 async 任务不一定中止已启动的 `spawn_blocking` 工作。

### 封面、列表与纹理

- 书架、统计总览和统计详情共用 `shelf/covers.rs` 的 `CoverCache`。不要另建长期保留整库封面字节或纹理的集合；统计页面通过回调借用原始书库数据。
- 每帧筛选、排序和后台统计快照只复制必要的元数据，避免深拷贝 `LibraryBook.cover_bytes`。只对需要解码的可见封面创建后台请求，并避免重复提交仍在处理的请求。
- 按显示尺寸和量化后的 DPI 生成缩略图：书架 160×228、统计总览 48×72、详情 100×150（逻辑尺寸）。可复用足够大的现有纹理；不要直接上传封面原图，也不要让高 DPI 纹理扩大逻辑布局。
- 保留磁盘缩略图复用、失败请求缓存和 LRU 淘汰。当前共享封面纹理预算为 32 MiB，进入阅读器后保留预算缩小到 4 MiB；可见纹理会被保留，这些预算不代表整个进程的内存上限。
- 书架保留网格虚拟化。统计列表的屏幕外行跳过封面请求和文字绘制；加载期间保留封面占位，避免异步完成导致布局跳动。统计页面的提前返回分支也必须执行共享缓存的淘汰。

### 阅读器与 GPU 内存

- 保留布局、栅格和页面 scene 的缓存预算及淘汰逻辑。当前布局元数据预算为 64 MiB、栅格预算为 128 MiB、桌面页面 scene 预算为 32 MiB；修改缓存策略时同时检查预取和正在显示的内容。
- 图像和公式像素尽量共享，避免每页、每个布局或每次渲染重新解码和复制完整 RGBA 数据。按显示需求使用降采样缓存，需要原始精度时再加载原图。
- 切换/关闭书籍时释放旧书的缓存和后台引用，并使旧结果失效。缓存移除不等于像素立即释放：可见布局、scene 或其他 `Arc` 持有者仍可能拥有它们。
- 区分 Rust 活跃分配、CPU 缓存、GPU 资源和驱动保留容量。不能仅凭工作集高或返回书架后内存未立即下降判断泄漏；GPU 缓冲池可能保留峰值容量。Vello 局部回收/维护 fork 尚未实施，后续需要单独定位和评估。

### 验证与诊断

- 涉及上述逻辑时验证书架、统计两页、阅读器、设置、加载状态和弹窗关闭后的闲置行为。先等待启动同步、封面加载和动画完成，再采样 CPU；失焦/最小化与聚焦状态应分别记录。
- 比较性能时保持同一本书、相同构建配置、窗口尺寸/DPI 和后台任务状态，并记录 GPU/驱动及启用的诊断功能。正式版与源码构建、不同 GPU 的内存数字不能直接视为同一分配行为。
- 调试构建使用 `reader-ui.log`，其中的 `render.activity` 可用于观察帧率和耗时；正式构建使用 `runtime.log`，会过滤高频阅读诊断。结合可用的 `memory.caches`、`memory.rust`、`memory.gpu` 等记录检查缓存和分配，同步状态看 `sync.log`。诊断采样本身也可能唤醒界面，避免新增高频采样。
- 针对改动运行已有性能回归测试（`idle`、`shelf::covers::`、`statistics::`、`platform::` 等相关过滤），覆盖闲置停止重绘、可见性、缓存预算/复用及旧任务失效。先验证编译，再使用新构建实测；测试配置通过不能代替桌面程序构建，`#[cfg(test)]` 可能掩盖生产配置问题。
- 常用验证命令：`cargo test --offline --locked -p rebook-desktop --features memory-profiling <相关过滤>`、`cargo build --offline --locked -p rebook-desktop --features memory-profiling`。记录实际测量结果；仅通过单元测试时，不宣称已测得 CPU 或内存降幅。

## GitHub Release Notes

- The GitHub Release title must exactly match its tag, including the lowercase `v` prefix (for example, `v0.5.0`). Do not add the product name, dates, subtitles, or descriptive suffixes. This is the release title, not a heading in the release notes body.
- When publishing with the GitHub CLI, explicitly use the tag as the title (for example, `gh release create v0.5.0 --title "v0.5.0" --notes-file <path>`), and verify the published title matches the tag.
- Write release notes in English first, followed by the Simplified Chinese translation inside an HTML `<details>` block.
- The summary tag must be exactly `<summary>中文更新说明</summary>`. Do not rename it or add attributes; the desktop updater uses this exact marker to select notes for the current interface language.
- Write for ordinary users. Lead with what changed in their experience and why it is useful, rather than how it was implemented.
- Prefer plain language and avoid library names, code symbols, architecture details, and other technical terminology unless users need them to understand compatibility or take action.
- Keep the notes concise. Use one short sentence per entry, combine closely related changes, and omit internal maintenance that has no meaningful user-facing effect.
- Classify changes under these headings and keep this order in both language sections:
  1. `## Feature` for new user-facing capabilities.
  2. `## Improvement` for enhancements to existing behavior, usability, performance, or quality.
  3. `## Fix` for corrected defects or regressions.
- Assign each change to one primary category and mention it only once per language. Do not repeat the same work under multiple headings with different wording.
- When a new feature includes supporting refinements, compatibility work, or corrections required to deliver that feature, describe them together in the `Feature` entry. Do not duplicate them as separate `Improvement` or `Fix` entries.
- Distinguish `Improvement` from `Fix` by intent: use `Fix` when previously intended behavior was incorrect, and `Improvement` when existing correct behavior was intentionally enhanced.
- Split work across categories only when the changes are independently meaningful to users and can each stand alone. Prefer one concise entry classified by its primary user-facing outcome when the distinction is uncertain.
- Keep the English and Chinese sections as one-to-one translations with the same categories and item order; neither language section should introduce additional or duplicated entries.
- Use the same English category headings in the Chinese section. Omit a category when it has no entries; do not invent filler items merely to include all three headings.
- Use this structure:

  ```markdown
  ## Feature

  - English description of a new capability.

  ## Improvement

  - English description of an enhancement.

  ## Fix

  - English description of a correction.

  <details>
  <summary>中文更新说明</summary>

  ## Feature

  - 新功能的中文说明。

  ## Improvement

  - 现有功能改进的中文说明。

  ## Fix

  - 问题修复的中文说明。

  </details>
  ```

- Keep all English-only content before `<details>`. Put all Chinese-only content inside the matching `<details>` block.
- If a Full Changelog link should appear in both languages, include it in both sections rather than placing it after `</details>`.

### Contributor Attribution

- Before publishing, inspect the commits and associated pull requests between the previous release tag and the new tag. Include only contributions shipped in that release; check GitHub's commit-to-PR associations so squash/rebase merges are also covered.
- Credit externally contributed changes directly in their `Feature`, `Improvement`, or `Fix` entry, with links to the author's GitHub profile and the contributing PR. The maintainer's account is `L-Chris`; omit attribution for the maintainer's own changes.
- Use the PR author and verified co-authors as the contribution source. Do not substitute the person who merged the PR or the merge commit's committer. Confirm the contribution against the included code before assigning credit.
- Preserve identical authors and PR links in the English and Chinese versions of an entry. Use absolute GitHub URLs so the links also work when release notes are mirrored to Gitee.
- If an entry combines work from multiple authors, identify their respective contributions or split independently meaningful changes into separate entries. Do not attribute the maintainer's additional work to an external contributor merely because the entry also includes their PR.
- For a verified external contribution without a PR, link its commit. Do not guess authorship from a display name or an unverified email address.
- Follow the entry-level attribution used by [egui](https://github.com/emilk/egui/releases/tag/0.36.2) and [bat](https://github.com/sharkdp/bat/releases/tag/v0.26.1). Keep the existing release categories and concise user-facing descriptions.
- Example English entry: `Keep the table of contents highlight on the section currently being read. — by [@Catapult291](https://github.com/Catapult291), [#3](https://github.com/TortoTech/torto/pull/3)`.
- 对应中文条目：`目录高亮正确跟随当前阅读的章节。——感谢 [@Catapult291](https://github.com/Catapult291)，[PR #3](https://github.com/TortoTech/torto/pull/3)`。
