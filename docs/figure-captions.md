# 图片与前置图注

桌面与 Android 共用 HTML 解析层的图注关联。在原书同一组兄弟节点内，编号图注或明确的图注语义紧邻图片时，解析为保留前置顺序的 `Figure`，不依赖出版社的 class 名称，也不需要 AI 请求。

前置图注先与自己的后续图片配对，再继续扫描下一组；普通正文、标题、目录目标和其他容器是边界，不跨越它们猜测关系。连续图片可共享一组图注，明确的续图注和紧随的来源说明保留原顺序。原有图片后置图注关系不变。较长图注中的 “shows”等词不会覆盖 CSS `break-after`／`page-break-after: avoid` 给出的明确前置约束。

分组只解析原节点一次，保留图注、图片、行内符号、脚注、链接、样式及原始 source ID；不会给关联对象重新编号。隐藏、装饰性图片及显式引用无法形成图片组时，保留各自解析结果。解析缓存修订为 17，使旧版缓存自动重建，不清除阅读进度或笔记。

本地《Handbook of Human Factors and Ergonomics》84 个文档复核：此前确认的 513 处前置图注全部关联，未关联计数降为 0，无解析错误。Issue #84 的 Figure 3 仍使用图注 `c01/n297:0–75` 和图片 `c01/n298`。原书回归另覆盖长图注、行内符号、脚注和全书缓存读回；合成回归覆盖容器边界、连续多图、来源说明、前后置顺序、源锚点及共同聚焦范围。

原书只读检查可使用 `TORTO_FIGURE_BOOK` 运行 formats 的 ignored 测试 `local_handbook_front_captions_keep_figure_and_source_identity`；`TORTO_CACHE_AUDIT_BOOK` 用于 `local_book_structure_cache_fidelity`。测试不调用模型，不写个人阅读状态。
