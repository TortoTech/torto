import PhotosUI
import SwiftUI
import ImageIO
import UIKit

struct FeedbackScreen: View {
    let core: MobileCore
    var reader: MobileReader? = nil
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @Environment(\.displayScale) private var scale
    @FocusState private var focused: Bool
    @State private var session: MobileFeedback?
    @State private var report: FeedbackState?
    @State private var kind = "bug"
    @State private var draft = ""
    @State private var photos: [PhotosPickerItem] = []
    @State private var busy = false
    @State private var error: String?
    @State private var saveTask: Task<Void, Never>?
    @State private var waiting = false
    @State private var showsContext = false
    @State private var previewIndex: UInt32?
    @State private var preview: CGImage?

    var body: some View {
        let palette = MobilePalette(scheme)
        let validation = validateFeedbackDescription(description: draft)
        GeometryReader { geometry in
            VStack(spacing: 0) {
                MobileHeader(title: mobileText("反馈问题", "Report an issue"), backIcon: "expand", back: close).disabled(busy)
                SettingsScroll {
                    if report?.status == "submitted" {
                        Text(mobileText("反馈已提交，感谢你的帮助", "Feedback submitted. Thank you.")).font(.system(size: 16, weight: .semibold))
                        if let url = report?.issueUrl.flatMap(URL.init(string:)), url.scheme == "https" {
                            Link(mobileText("查看反馈", "View issue"), destination: url)
                        }
                    } else if let report {
                        HStack(spacing: 12) {
                            category("bug", mobileText("缺陷", "Bug"))
                            category("feature", mobileText("功能", "Feature"))
                        }.disabled(report.locked || busy)
                        VStack(alignment: .leading, spacing: 4) {
                            TextField(kind == "bug" ? mobileText("发生了什么？如何重现？", "What happened? How can we reproduce it?") :
                                        mobileText("希望增加什么功能？", "What feature would help?"), text: $draft, axis: .vertical)
                                .font(.system(size: 16)).focused($focused).lineLimit(1...12).padding(16)
                                .frame(maxWidth: .infinity, minHeight: 180, alignment: .topLeading)
                                .background(palette.surface, in: .rect(cornerRadius: 5))
                                .overlay { RoundedRectangle(cornerRadius: 5).strokeBorder(focused ? palette.accent : palette.controlOutline) }
                                .accessibilityLabel(mobileText("反馈内容", "Feedback description"))
                                .disabled(report.locked || busy)
                            Text(mobileText("至少 10 个非空白字符", "At least 10 non-whitespace characters") + " · \(validation.count)/5000")
                                .font(.system(size: 12)).foregroundStyle(validation.count > 5000 ? .red : palette.secondary)
                                .padding(.horizontal, 16)
                        }
                        screenshots(report)
                        MobileGroup {
                            MobileRow(title: mobileText("附带信息", "Included information"),
                                      value: showsContext ? mobileText("收起", "Hide") : mobileText("展开", "Show")) { showsContext.toggle() }
                            if showsContext {
                                FeedbackContextDetails(json: report.context)
                            }
                        }
                    }
                }
                VStack(spacing: 8) {
                    if let error { Text(error).font(.footnote).foregroundStyle(.red) }
                    if waiting { Text(mobileText("请稍后重试", "Please retry later")).font(.footnote) }
                    Button {
                        if report?.status == "submitted" { dismiss() } else { submit() }
                    } label: {
                        Text(report?.status == "submitted" ? mobileText("完成", "Done") :
                             busy ? mobileText("处理中…", "Working…") : report?.locked == true ? mobileText("重试", "Retry") : mobileText("提交", "Submit"))
                            .frame(maxWidth: .infinity, minHeight: 28)
                    }.buttonStyle(.borderedProminent)
                        .disabled(report == nil || busy || waiting || report?.status != "submitted" && !validation.valid)
                }.padding(16).background(palette.surface.ignoresSafeArea(edges: .bottom))
            }
            .foregroundStyle(palette.ink)
            .background(palette.background.ignoresSafeArea())
            .accessibilityHidden(preview != nil)
            .task {
                do {
                    let device = UIDevice.current
                    let session = try await core.openFeedback(reader: reader, environment: FeedbackEnvironment(
                        osVersion: "iOS " + device.systemVersion, device: device.model, arch: "arm64",
                        width: UInt32(max(1, geometry.size.width * scale)),
                        height: UInt32(max(1, geometry.size.height * scale)), scale: Float(scale)))
                    let state = try await session.snapshot()
                    guard !Task.isCancelled else { return }
                    kind = state.kind; draft = state.description; report = state; self.session = session
                    focused = !state.locked && state.status != "submitted"
                } catch { if !Task.isCancelled { self.error = message(error) } }
            }
        }
        .overlay {
            if let preview { FullImagePreview(image: preview, close: closePreview).transition(.opacity) }
        }
        .animation(.easeInOut(duration: 0.16), value: preview != nil)
        .statusBarHidden(preview != nil)
        .task(id: previewIndex) {
            guard let index = previewIndex, let session else { return }
            do {
                let bytes = try await session.image(index: index)
                let image = await Task.detached(priority: .userInitiated) {
                    guard let source = CGImageSourceCreateWithData(bytes as CFData, nil) else { return nil as CGImage? }
                    return CGImageSourceCreateThumbnailAtIndex(source, 0, [
                        kCGImageSourceCreateThumbnailFromImageAlways: true,
                        kCGImageSourceCreateThumbnailWithTransform: true,
                        kCGImageSourceThumbnailMaxPixelSize: 2048,
                    ] as CFDictionary)
                }.value
                guard !Task.isCancelled, previewIndex == index else { return }
                if let image { focused = false; preview = image }
                else { throw CocoaError(.fileReadCorruptFile) }
            } catch {
                if !Task.isCancelled && previewIndex == index {
                    closePreview()
                    self.error = mobileText("无法打开截图", "Unable to open screenshot")
                }
            }
        }
        .task(id: report?.retryUntil) {
            let remaining = Double(report?.retryUntil ?? 0) / 1000 - Date().timeIntervalSince1970
            waiting = remaining > 0
            if remaining > 0 {
                do { try await Task.sleep(for: .seconds(remaining)) } catch { return }
                waiting = false
            }
        }
        .onChange(of: draft) { _, _ in scheduleDraft() }
        .onChange(of: kind) { _, _ in scheduleDraft() }
        .onChange(of: photos) { _, chosen in if !chosen.isEmpty { addPhotos(chosen) } }
    }

    private func category(_ id: String, _ title: String) -> some View {
        let palette = MobilePalette(scheme)
        return Button { kind = id } label: {
            Text(title).font(.system(size: 14, weight: .medium)).padding(.horizontal, 12).frame(minHeight: 32)
                .background(kind == id ? palette.selection : Color.clear, in: .rect(cornerRadius: 8))
                .overlay { RoundedRectangle(cornerRadius: 8).strokeBorder(kind == id ? .clear : palette.controlOutline) }
                .foregroundStyle(kind == id ? palette.accent : palette.ink)
                .frame(minHeight: 44).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle()).accessibilityAddTraits(kind == id ? .isSelected : [])
    }

    private func screenshots(_ report: FeedbackState) -> some View {
        let palette = MobilePalette(scheme)
        return ScrollView(.horizontal) {
            HStack(spacing: 12) {
                ForEach(report.images, id: \.index) { image in
                    if let bitmap = UIImage(data: image.thumbnail) {
                        VStack {
                            Button { previewIndex = image.index } label: {
                                Image(uiImage: bitmap).resizable().scaledToFit().frame(width: 96, height: 76)
                                    .contentShape(Rectangle())
                            }.buttonStyle(MobileInkButtonStyle())
                                .accessibilityLabel(mobileText("预览截图 \(image.index + 1)", "Preview screenshot \(image.index + 1)"))
                            if !report.locked {
                                Button(mobileText("移除", "Remove")) {
                                    closePreview()
                                    perform { session in try await session.removeImage(index: image.index) }
                                }.font(.system(size: 13))
                            }
                        }
                    }
                }
                if !report.locked, report.images.count < 3 {
                    PhotosPicker(selection: $photos, maxSelectionCount: 3 - report.images.count, matching: .images) {
                        HStack(spacing: 8) {
                            MobileIcon("image", size: 24)
                            Text(mobileText("添加截图", "Add screenshots")).font(.system(size: 14, weight: .medium))
                        }.padding(.horizontal, 14).frame(minHeight: 40)
                            .overlay { Capsule().strokeBorder(palette.controlOutline) }.contentShape(Capsule())
                    }.buttonStyle(MobileInkButtonStyle())
                }
            }
        }.disabled(busy)
    }

    private func scheduleDraft() {
        guard !busy, let session, report?.locked == false,
              report?.description != draft || report?.kind != kind else { return }
        saveTask?.cancel()
        let text = draft, type = kind
        saveTask = Task {
            do { try await Task.sleep(for: .milliseconds(350)) } catch { return }
            do {
                let state = try await session.edit(kind: type, description: text)
                if !Task.isCancelled { report = state }
            } catch { if !Task.isCancelled { self.error = message(error) } }
        }
    }

    private func perform(_ work: @escaping (MobileFeedback) async throws -> FeedbackState, close: Bool = false) {
        guard let session, !busy else { return }
        busy = true
        error = nil
        let task = saveTask
        task?.cancel()
        Task {
            await task?.value
            defer { busy = false }
            do {
                if report?.locked == false { report = try await session.edit(kind: kind, description: draft) }
                report = try await work(session)
                if report?.status == "submitted" { focused = false }
                if let code = report?.error { self.error = mobileText("反馈暂未提交", "Feedback not submitted") + " · " + code }
                if close { dismiss() }
            } catch { self.error = message(error) }
        }
    }

    private func submit() { perform { try await $0.submit() } }
    private func closePreview() { previewIndex = nil; preview = nil }
    private func close() {
        if session == nil || report?.locked == true || report?.status == "submitted" { dismiss() }
        else { perform({ try await $0.snapshot() }, close: true) }
    }

    private func addPhotos(_ chosen: [PhotosPickerItem]) {
        photos = []
        perform { session in
            var result = try await session.snapshot()
            for item in chosen {
                guard let data = try await item.loadTransferable(type: Data.self) else { continue }
                let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
                defer { try? FileManager.default.removeItem(at: url) }
                try await Task.detached(priority: .utility) { try data.write(to: url) }.value
                result = try await session.addImage(path: url.path)
            }
            return result
        }
    }
}

private struct FeedbackContextDetails: View {
    let json: String
    @Environment(\.colorScheme) private var scheme
    @State private var showsRanges = false
    @State private var context: Context?

    private struct Context: Decodable {
        struct App: Decodable { let version: String }
        struct OS: Decodable { let name: String; let version: String; let arch: String }
        struct Display: Decodable { let width: Int; let height: Int; let scale: Double }
        struct Book: Decodable {
            let title: String; let authors: [String]; let languages: [String]; let format: String; let layout: String
        }
        struct Location: Decodable {
            let chapter: String?; let document: String?; let anchor: String?; let page: Int?
            let focus: Focus?
        }
        struct Focus: Decodable {
            let range: Range; let ranges: [Range]; let totalRanges: Int
            struct Range: Decodable {
                let start: Anchor; let end: Anchor
                var display: String { "\(start.display) → \(end.display)" }
            }
            struct Anchor: Decodable {
                let spine: String; let node: String; let textOffset: UInt64
                var display: String { "\(spine) / \(node) : \(textOffset)" }
            }
        }
        struct Reader: Decodable {
            let mode: String; let typesetting: String; let pdfReflow: String?; let translation: Translation
            struct Translation: Decodable { let enabled: Bool; let targetLanguage: String }
        }
        let source: String
        let app: App; let os: OS; let display: Display
        let book: Book?; let location: Location?; let reader: Reader?

        var rows: [(String, String)] {
            var fields = [(String, String)]()
            func add(_ zh: String, _ en: String, _ value: String?) {
                if let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    fields.append((mobileText(zh, en), value))
                }
            }
            add("来源", "Source", source == "reader" ? mobileText("阅读页", "Reader") : mobileText("应用页面", "Application"))
            if let book {
                add("书籍", "Book", book.title)
                add("作者", "Authors", book.authors.joined(separator: ", "))
                add("原书语言", "Book language", book.languages.joined(separator: ", "))
                add("格式", "Format", book.format)
                add("原书布局", "Book layout", book.layout == "fixed" ? mobileText("固定布局", "Fixed") : mobileText("可重排", "Reflowable"))
            }
            if let location {
                add("章节", "Chapter", location.chapter)
                add("书内文档", "Book document", location.document)
                add("当前聚焦块", "Focused block", location.focus?.range.start.node)
                add("章节锚点", "Chapter anchor", location.anchor)
                add("PDF 页码", "PDF page", location.page?.formatted())
            }
            if let reader {
                add("阅读模式", "Reading mode", reader.mode == "focus" ? mobileText("专注模式", "Focus mode") : mobileText("经典模式", "Classic mode"))
                add("排版", "Typesetting", reader.typesetting == "unified" ? mobileText("统一版式", "Unified typesetting") : mobileText("原书版式", "Original typesetting"))
                if let pdf = reader.pdfReflow {
                    add("PDF 视图", "PDF view", pdf == "local" ? mobileText("本地重排", "Local reflow") :
                        pdf == "ai" ? mobileText("AI 重排", "AI reflow") : mobileText("原版", "Original"))
                }
                let translation = reader.translation
                add("翻译", "Translation", translation.enabled ? mobileText("已开启", "On") + " · " + translation.targetLanguage : mobileText("已关闭", "Off"))
            }
            add("应用版本", "App version", app.version)
            add("系统版本", "System version", os.version)
            add("系统架构", "Architecture", os.arch)
            add("屏幕尺寸", "Display", "\(display.width) × \(display.height) px")
            add("显示缩放", "Display scale", display.scale.formatted(.percent.precision(.fractionLength(0))))
            return fields
        }
    }

    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(alignment: .leading, spacing: 10) {
            if let context {
                ForEach(Array(context.rows.enumerated()), id: \.offset) { _, row in
                    HStack(alignment: .top, spacing: 12) {
                        Text(row.0).foregroundStyle(palette.secondary).frame(width: 100, alignment: .leading)
                        Text(row.1).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                if let focus = context.location?.focus {
                    Text(mobileText("聚焦范围", "Focus range")).foregroundStyle(palette.secondary)
                    Text(focus.range.display).textSelection(.enabled)
                    DisclosureGroup(mobileText("成员范围", "Member ranges") + " · \(focus.ranges.count)/\(focus.totalRanges)", isExpanded: $showsRanges) {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(Array(focus.ranges.enumerated()), id: \.offset) { index, range in
                                Text("\(index + 1). \(range.display)").textSelection(.enabled)
                            }
                        }.padding(.top, 8)
                    }
                }
            } else {
                Text(mobileText("暂无附带信息", "No information available")).foregroundStyle(palette.secondary)
            }
        }.font(.system(size: 13)).padding(.horizontal, 16).padding(.bottom, 16)
            .task(id: json) { context = try? JSONDecoder().decode(Context.self, from: Data(json.utf8)) }
    }
}
