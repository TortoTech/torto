# Android：Rust core + Kotlin

Android 已纳入 `torto` workspace；独立的 `torto-app` Flutter 仓库仅作为功能与交互参考。本次不迁移旧 App 的本地数据库、缓存、配置和密钥，也不保留 Dart/Flutter 桥接层。桌面端的现有文件名、同步协议和凭据标识保持兼容。

## 边界

```text
apps/desktop (egui / winit)       apps/android (Kotlin / Compose)
          │                               │
          │                    crates/android-bridge (UniFFI)
          └──────────────┬────────────────┘
                  crates/app-core
         library / search / AI / translation / fonts
         WebDAV / annotations / statistics / persistence
                         │
       formats → publication → layout → reader → renderer
```

- `app-core` 不依赖 egui、winit、JNI 或 Android。桌面端保留薄的 re-export，实际实现只有一份。`reading::ReadingPipeline` 统一 rewrite → translation → semantic layout → paragraph structure 的组合顺序。
- 宿主在首次服务调用前指定数据、配置、缓存目录，并安装 `SecretStore`。当前一个进程只允许一个 profile；重复初始化相同路径可用，更换 profile 会明确失败。
- 桌面端统一使用下述加密凭据库，显式 smoke profile 使用独立内存凭据以避免访问个人钥匙串；Android 使用 Android Keystore 的 AES-GCM 密钥，加密内容写入私有 preferences，服务/账户作为 AAD。普通设置 JSON 不包含 API Key 或 WebDAV 密码，禁止备份应用数据。
- UniFFI 只传书籍、目录、位置、文本区域、结果和命令。文字位置由 Rust 管理，不将 Kotlin 的 UTF-16 索引当成原文偏移。
- 页面用共享 CPU 渲染器绘制，通过手写 JNI 接口复制到 `Bitmap`。注册表保存弱引用并验证帧 ID、尺寸、像素格式和 stride；不把裸指针或整页 RGBA 字节数组放进 Kotlin DTO。
- `MobilePage.close()` 释放原生像素，Android 保留当前 Bitmap。阅读会话显式 shutdown/close；ViewModel 跨旋转保留，关闭时后台释放。所有解析、数据库、网络和绘制在 worker 上运行。
- UI 命令串行，切书时递增 generation，旧结果不回写。取消 Kotlin 协程不等于取消阻塞 Rust 工作，因此 native 调用完成前不会提前释放串行锁。
- 阅读缓存默认 24 MiB layout + 48 MiB raster，内存通知后缩减到 4/8 MiB 并停止预取；当前可见页允许超过软预算。桌面端默认预算不变。页面仅在内容或选择变化时重绘。

## 桌面端安全凭据库

macOS 与 Windows 通过同一 `VaultStore` 保存 AI 提供商、搜索、OCR 与 WebDAV 凭据。系统 keyring 只保存 `Torto Credential Vault / desktop-master-v1` 的 32 字节主密钥；`config/credentials.v1.vault` 保存 AES-256-GCM 加密内容（每次独立随机 96 位 nonce、版本头作为 AAD）。Windows 的单条凭据容量限制不再约束所有 Key 的总长度。此文件属于本机配置，不参与书库云同步。

- 首次读取某个旧条目时按原 service/account 迁移；加密文件原子写入并回读校验后才删除旧条目。迁移或清理失败时保留可恢复的数据，错误不会降级为“没有 Key”。旧版本不能读取新的凭据库；降级前需自行保留可重新配置的凭据。
- 不存在凭据的 profile 不创建主密钥或凭据文件。已读取的主密钥、解密内容及缺失结果在进程内复用，文件变化时重新校验。内容相同的保存跳过写入；删除保留 tombstone，防止旧条目重新出现。
- 进程内互斥和跨进程非阻塞文件锁保护读改写；每次修改先合并磁盘最新内容。凭据文件最多 2 MiB、512 条，每项新写入最多 128 KiB。
- 拒绝授权或文件损坏后暂停自动重试；设置显示“重试解锁安全存储”，由后台任务执行，或放弃本次更改。解锁失败时保留公开模型配置，并禁止把未读到的 Key 当作清空操作保存。
- 旧条目首次迁移仍可能逐项授权；ad-hoc 新构建仍可能要求重新授权主密钥。Developer ID 签名配置沿用现有脚本，只有配置稳定证书后才能改善跨版本身份识别。

Android 保留 Android Keystore 不可导出密钥及逐项 AES-GCM 密文，使用同一个 core 凭据接口；桌面端加密文件格式不用于 Android。

## 已接通的 Android 功能

- 系统文件选择器导入，三列封面书架、最近阅读标记、长按删除确认，格式解析与桌面共用
- 阅读、翻页、目录、全书搜索与源位置跳转、长按选择/复制，支持自由/单词/句子/段落粒度
- 字号、字重、中日韩／拉丁书籍的主字体与混排字体、等宽字体、统一版式、深色阅读与本地设置保存；助手代码跟随阅读等宽字体，不内置重复字体
- 稳定阅读位置写入共享 SyncStore，关闭/重新打开及横竖屏重排后恢复
- WebDAV 手动／前台／后台自动同步书籍、阅读进度、注释和统计；共享 7 种云盘预设，支持设备名称及前台／后台独立开关，导入／删除书籍后合并触发同步
- AI 提供商增删改、类型预设、模型刷新／搜索／手动添加及各用途独立的模型、思考等级；Key 只进入安全存储，设置返回值仅带“已保存”状态，空输入保留原 Key
- 本页语义排版及翻译；启用后随可见页继续，按“翻译 → 排版”处理并可预处理下一页；显示阶段／批次进度，支持停止、暂停后继续、失败重试及退到后台取消；预处理完成／失败／取消均恢复阅读位置
- 原文／双语展示、目标语言、目录翻译与专家翻译；翻译开关跨会话保存，目录复用共享翻译和缓存服务
- 默认字体从 lab Release 的 GitHub Asset API 下载，校验后缓存，沿用共享镜像回退
- 沿用旧 App 的米白/棕色与深色主题，“书架 / 我”底部导航、分组设置和阅读底部六按钮；支持界面主题/语言设置
- 高亮/批注的新建、编辑、删除及源位置跳转，重排后仍能定位并绘制高亮
- “我”页统计摘要，周／月／年／全部周期、历史周期切换、分布及分书排行；总览沿用旧 App 的分段切换、四张指标卡、刻度分布图与封面排行；单书详情包含封面、状态、日期、进度、时长、高亮／批注和按天汇总的阅读历史；阅读计时仅在前台无弹层时工作，后台/长时间无操作暂停，不触发每秒重绘
- 阅读助手流式回复、停止生成、按书保存最近 60 条历史、选区提问；支持标题、列表、引用、代码、链接、横向表格及共享 LaTeX 公式展示；工具修改先展示确认，正文临时改写可还原
- 阅读助手支持从系统文件选择器或书中图片预览添加附件、移除草稿附件和查看历史图片；失败/取消后恢复提问草稿。每条最多 8 张、输入单文件 20 MiB，复用共享缩放及 PNG 编码，发送图片最大边 2048 像素。每书图片存储最多 64 MiB；清空对话同时移除附件，已被历史引用的图片不会被“移除草稿”删除
- 对话引用使用共享 `link://j/` 标识与 Unicode 解码，能跳到对应源段落并返回；未知章节/段落明确失败。外部链接由系统浏览器打开，不自动加载远程 Markdown 图片
- 图片触摸预览、双指缩放/双击复位；公式通过共享 LaTeX 渲染器重新生成清晰预览，并可查看源代码。预览位图上限 2048×2048，原图解码预算 64 MiB
- 内嵌／链接脚注及引用弹窗支持强调、列表、链接和 LaTeX 公式，提供源位置跳转；普通内部链接与网站打开；目录/搜索/链接跳转保留最多 32 个返回位置，点击已有高亮可编辑批注
- 分页／滚动专注按语义块切换，复用共享列表／段落组合与按句分段；默认周边按桌面相同的 45% 不透明度向本页背景淡化，可切换高亮样式。滚动保留最多 8 个附近页面，只绘制一屏；有限惯性与拖动停止后无动画循环
- 选区拖动手柄与边缘自动滚动，由共享 Rust 源位置计算跨页选区；手柄拖动结束后不再调度，字体重排／跳转清除失效选择；点按正文中央收起／恢复工具栏
- 翻译批次原子缓存到本机 cache/translations，按原文输入、目标语言、模型、端点和思考配置校验；API Key 不写入缓存。单文件 4 MiB，条目上限 4096，内存保留文本约 2 MiB。切换配置清空旧的内存译文，缓存命中可在未配置有效模型 Key 时离线复用
- 检查更新、自动／GitHub／Gitee 更新源、更新说明、下载进度／取消和系统安装入口；诊断日志脱敏导出及可搜索的完整开源许可页面
- 页面文本的 TalkBack 虚拟节点、前后翻页无障碍操作

## 尚未接入的界面与发布工作

核心页面、批注、统计、同步和应用维护已接通；以下工作待续：

- 对话输入中的 @ 引用选择、历史消息编辑/重发、Mermaid/SVG 等复杂可视化，以及会话间持久正文改写；当前改写在阅读会话内临时保存
- PDF 本地重排/OCR 操作界面、表格/图表交互及单独的链接脚注翻译
- 书籍导出／分享与更多平板交互（封面按需读取，已有磁盘缩略图及 8 MiB 内存缓存）
- 真机功耗与大书内存测量；完整 TalkBack 人工走查；正式签名/上架

上述未接入项不通过复制 Dart 或桌面 UI 逻辑来实现。下一步继续补 Kotlin 展示层和共享服务的粗粒度命令；涉及调度顺序的规则先沉到 core 再接两端。

## 本机构建

前置：仓库固定 Rust toolchain、JDK 17、Android SDK 36、NDK `28.2.13676358`、`cargo-ndk 4.1.2`。

Linux 构建宿主还需安装 `pkg-config` 和 `libfontconfig1-dev`（Debian／Ubuntu）；它们供宿主上的 UniFFI 生成器使用，不进入 Android 安装包。

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
sdkmanager 'platforms;android-36' 'build-tools;36.0.0' 'ndk;28.2.13676358'
export ANDROID_HOME=/path/to/android/sdk
export JAVA_HOME=/path/to/jdk17
cd apps/android
./gradlew :app:assembleDebug
# 分别生成 ARM64 和 x86_64 APK，不生成通用包
./gradlew :app:assembleDebug -PrustAbis=arm64-v8a,x86_64
./gradlew :app:assembleDebugAndroidTest
./gradlew :app:connectedDebugAndroidTest
./gradlew :app:assembleRelease
```

默认仅 ARM64；可用 `-PrustAbis=arm64-v8a,x86_64` 同时构建，分别生成 `app-arm64-v8a-{variant}.apk` 与 `app-x86_64-{variant}.apk`（未签名 Release 文件名额外带 `-unsigned`），每个 APK 只含一种架构。当前不提供 32 位 ARM／x86。最低 Android 8 / API 26，target/compile API 36。调试包 `org.tortotech.torto.debug`，正式包 `org.tortotech.torto`，数据和 Keystore 隔离。旧 App 不被替换或清理。

Gradle 自动调用 `scripts/build-android.py`，每个 variant 分别生成 `.so` 与 Kotlin binding；绑定读取本次 Android `.so` 的元数据，不依赖手工维护 JNI 方法清单。生成文件不提交。修改核心后不要跳过原生构建任务。

Release 使用 Rust release profile，保留独立 `.dbg` 后剥离库符号，R8 mapping 另存。启用 R8 与未使用资源收缩，仅保留界面支持的中文／英文语言资源。Android 链接使用兼容 API 23 起的 APS2 重定位压缩及 safe ICF，保留异常展开与恢复；ELF 继续按 16 KiB 对齐。签名通过以下环境变量提供，缺少时生成 unsigned APK，绝不回退使用 debug key：

- `TORTO_ANDROID_KEYSTORE`
- `TORTO_ANDROID_STORE_PASSWORD`
- `TORTO_ANDROID_KEY_ALIAS`
- `TORTO_ANDROID_KEY_PASSWORD`

APK：`apps/android/app/build/outputs/apk/`；Rust 符号：`apps/android/app/build/generated/rust/release/symbols/`。`.github/workflows/android.yml` 在 PR/main 构建 debug/device-test APK 与 unsigned release，并保存产物，不自动发布。

## APK 体积与打包方式

默认将原生库以未压缩形式放入 APK，供 Android 直接映射，避免安装后保留第二份原生库。GitHub／Gitee 直接分发时，可显式选择压缩 APK：

```sh
./gradlew :app:assembleRelease -PcompressNativeLibraries=true
```

压缩方式保留全部功能，但安装时需解压原生库，安装后占用会增加；不是引擎本身体积降到压缩后的大小。两种方式均保留原生库 16 KiB ELF 对齐（[Android 说明](https://developer.android.com/guide/practices/page-sizes)）。默认不改变现有安装方式。

当前 Release 实测（未签名）：

| ABI | 常规 APK | 压缩 APK |
| --- | ---: | ---: |
| arm64-v8a | 39.24 MiB | 19.36 MiB |
| x86_64 | 45.12 MiB | 20.79 MiB |

ARM64 优化前为 41.27 MiB；原生库重定位数据及资源实际减少约 2.03 MiB，其余差异来自打包压缩。压缩版安装后总占用比常规版约增加 17.6 MiB，不把它算作运行时内存下降。

用优化后的生产原生库在独立预览包验证，不需要正式签名：

```sh
./gradlew :app:assembleDebug :app:assembleDebugAndroidTest -PreleaseNativePreview=true
# 验证压缩库的安装／加载
./gradlew :app:assembleDebug :app:assembleDebugAndroidTest -PreleaseNativePreview=true -PcompressNativeLibraries=true
```

此选项只将 Debug 的原生库与绑定切到 Release 产物；预览包名、Keystore 与数据仍保持隔离。关闭该选项后，普通 Debug 构建继续使用调试原生库。

## 验证

```sh
cargo test --locked --workspace
python3 scripts/check-android-alignment.py apps/android/app/build/outputs/apk/release/app-release-unsigned.apk
```

对齐检查要求每个 APK 只含一种受支持的 ABI，核对 ELF 实际架构及每个 `.so`（含 JNA 和 AndroidX）的 ELF LOAD 16 KiB 对齐；未压缩库还需核验 ZIP 起始位置的 16 KiB 对齐。压缩库由系统安装时解压，不适用 ZIP mmap 对齐要求。Debug 与 Release 使用不同输出目录，切换 ABI 时清理旧生成结果。

Rust bridge 回归覆盖导入去重、搜索定位、绘制、选择、批注增删改和重排定位、统计、流式助手与取消、工具修改确认/放弃、原图/公式预览、脚注、链接历史、专注切换、取消 AI 处理、跨会话离线译文缓存及模型切换、帧释放和关闭后调用；reader 回归覆盖移动缓存策略在刷新后的保留。Android instrumentation 覆盖实际 JNI Bitmap 复制、错误帧/尺寸拒绝、阅读重排与位置恢复，以及 Keystore 加密与账户隔离。测试不调用付费模型、不使用个人 WebDAV 账户。

### 当前验证结果（2026-10-31）

- `cargo test --locked --workspace`：1,802 通过、0 失败；124 项真实书库／外部服务等用例按原配置忽略。新增覆盖云同步取消／账户切换、统计周期、富文本脚注、更新元数据及诊断脱敏
- macOS 生产代码构建、隔离 EPUB smoke test 通过（Apple M6 / Metal，54 帧）；桌面端更新检查继续由原有回归覆盖
- Android API 36 ARM64 模拟器（SwiftShader）使用 Release 原生库，以常规 mmap 和安装解压两种方式各通过 12 项 instrumentation 测试：Keystore 隔离、JNI 帧复制、选择／批注／重排、专注与链接、字体和 AI 设置、富文本脚注／公式、统计周期、后台任务实际执行、日志共享／APK 拦截、许可资源及 HTTPS 不受信任证书拦截。测试不调用付费模型或个人 WebDAV 服务
- 本轮使用模拟器验证；此前 Redmi K60（Android 15 / API 35）8 项阅读及设置测试通过。home 暂时离线，不把新增 4 项记为真机验证
- 手动走查脚注、统计总览／详情、云盘预设、更新源、检查更新及许可弹窗；统计另核对周期切换、图表提示关闭、浅／深色及 130% 系统字号；阅读页退到后台再恢复正常，稳定后采样没有新增绘制帧
- Debug／Release APK 构建及 APK 原生库 ELF／ZIP 16 KiB 对齐检查通过，Release 保持 unsigned；尚未执行正式签名 APK 的覆盖升级、16 KiB 页真机测试或真机功耗测量。x86_64 Release 已构建并通过架构／16 KiB 检查，尚未执行该 ABI 的设备测试

## 调度、维护与边界

同步采用进程级串行事务与原生书库锁；账户／配置变更取消旧请求并丢弃过期结果。前台按后台计时器处理进度同步，首次、书库变更及定期执行完整同步；退出阅读后先落盘统计，再安排后台工作。WorkManager 周期任务最短 15 分钟并要求网络连接，实际执行受系统电量与后台限制影响，不保证精确时刻（[Android 调度说明](https://developer.android.com/develop/background-work/background-tasks/persistent/getting-started/define-work)）。关闭同步时取消相关工作，不让未启用功能持续轮询。

更新逻辑从桌面提取至 `app-core/updates`，Android 根据 ABI 选择 `Torto-{version}-android-{abi}.apk`；GitHub 优先 Asset API，Gitee 使用 `torto-android-update.json`，默认源自动回退。下载前验证来源、文件名及元数据，完成后核对大小／SHA-256；调用系统安装器前额外核对包名、版本号与本机签名。Debug／正式包互相隔离，正式覆盖升级需要同一正式证书签名的发布包。

Android 启动时先向 `rustls-platform-verifier` 注册 Application，再启动 HTTP 服务；JVM 验证器的 AAR 版本读取 Cargo.lock，R8 保留其 JNI 入口。不关闭证书验证；本地 HTTPS 回归确认自签名证书被拒绝，实际 GitHub 更新检查确认可信 HTTPS 可用。

诊断导出仅收集有限的运行／阅读／同步日志和设备版本，不打包书库、聊天、配置或凭据文件；进一步替换已存 API Key／WebDAV 密码及本机目录。每个共享报告使用独立文件，最多保留最近 3 份，FileProvider 只开放报告和更新缓存目录。构建时从实际 Cargo／Gradle 依赖生成许可资源（当前 543 个组件），相同上游许可全文去重保存，支持离线查看及项目链接。

Android 调试构建显式内嵌公式字体，避免 RaTeX 读取编译主机路径；原生公式与识别公式均注册预览区域。对话和脚注使用共享 Markdown 规则与公式渲染器；可见消息在后台解析，流式更新合并延迟 120 ms，公式缓存 8 MiB，每条可见消息最多 24 个公式／8 MiB 位图，单个公式最大 1024×1024，超限或失败退回 LaTeX 文本。
