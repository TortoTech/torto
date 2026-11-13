import SwiftUI
import UIKit

struct ReadingAssistantScreen: View {
    let book: BookSummary
    @ObservedObject var model: ReaderModel
    let close: () -> Void
    @Environment(\.colorScheme) private var scheme
    @FocusState private var focused: Bool
    @State private var confirmClear = false
    @State private var following = true
    @State private var atBottom = true
    @State private var userScrolling = false
    @State private var assets = AssistantFormulaCache()
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            MobileHeader(title: mobileText("AI 问书", "Ask this book"), subtitle: book.title, backIcon: "expand", back: close) {
                Menu {
                    Button(mobileText("还原临时改写", "Undo rewrites"), systemImage: "arrow.uturn.backward", action: model.undoChatRewrites)
                        .disabled(model.working || model.chatStream != nil || !model.chatChanges.isEmpty)
                    Button(mobileText("清空本次对话", "Clear conversation"), systemImage: "trash", role: .destructive) { confirmClear = true }
                        .disabled(model.working || model.chatStream != nil || model.chatMessages.isEmpty)
                } label: { MobileIcon("more", size: 24).frame(width: 48, height: 48) }
                    .accessibilityLabel(mobileText("助手菜单", "Assistant menu"))
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 12) {
                        ForEach(model.chatMessages, id: \.id) { entry in
                            if entry.user { question(entry.content) }
                            else {
                                AssistantAnswer(entry: entry, model: model, assets: assets, close: close, clear: { confirmClear = true })
                            }
                        }
                        if let stream = model.chatStream {
                            question(stream.question)
                            MobileGroup {
                                VStack(alignment: .leading, spacing: 12) {
                                    DisclosureGroup(mobileText("思考中 …", "Thinking …")) {
                                        Text([stream.reasoning, stream.activity].filter { !$0.isEmpty }.joined(separator: "\n"))
                                            .foregroundStyle(palette.secondary).padding(.top, 8)
                                    }.font(.system(size: 14)).tint(palette.secondary)
                                    if !stream.content.isEmpty {
                                        AssistantMarkup(source: stream.content, streaming: true, assets: assets, follow: { _ in })
                                    }
                                }.padding(18)
                            }
                        }
                        if !model.chatChanges.isEmpty {
                            MobileGroup {
                                VStack(alignment: .leading, spacing: 10) {
                                    Text(mobileText("待确认的正文改动", "Proposed changes"))
                                    ForEach(Array(model.chatChanges.enumerated()), id: \.offset) { _, change in
                                        Text(change.text).font(.system(size: 14))
                                    }
                                    HStack {
                                        Button(mobileText("拒绝", "Reject")) { model.resolveChatChanges(false) }
                                        Button(mobileText("应用", "Apply")) { model.resolveChatChanges(true) }
                                    }.buttonStyle(.bordered).disabled(model.working)
                                }.padding(16)
                            }
                        }
                        Color.clear.frame(height: 1).id("chat-end")
                    }.frame(maxWidth: 680).padding(16).frame(maxWidth: .infinity)
                }
                .scrollDismissesKeyboard(.interactively)
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    geometry.contentSize.height + geometry.contentInsets.bottom - geometry.visibleRect.maxY <= 24
                } action: { _, near in
                    atBottom = near
                    if userScrolling { following = near }
                }
                .onScrollPhaseChange { _, phase in
                    userScrolling = phase == .interacting || phase == .decelerating
                }
                .onScrollGeometryChange(for: CGFloat.self) { $0.contentSize.height } action: { _, _ in
                    if following && !userScrolling { proxy.scrollTo("chat-end", anchor: .bottom) }
                }
                .onChange(of: model.chatStream?.id) { _, id in
                    if id != nil { following = true }
                    if following { proxy.scrollTo("chat-end", anchor: .bottom) }
                }
                .overlay(alignment: .bottom) {
                    if !following && !atBottom {
                        Button {
                            following = true
                            withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("chat-end", anchor: .bottom) }
                        } label: {
                            Label(mobileText("回到底部", "Back to bottom"), systemImage: "chevron.down")
                                .font(.system(size: 13)).padding(.horizontal, 14).padding(.vertical, 8)
                                .background(palette.surface, in: .capsule).shadow(color: .black.opacity(0.12), radius: 4, y: 1)
                        }.buttonStyle(MobileInkButtonStyle()).padding(.bottom, 8)
                    }
                }
            }
            if let error = model.chatError { Text(error).font(.system(size: 13)).foregroundStyle(.red).padding(.horizontal, 16) }
            VStack(spacing: 12) {
                HStack(spacing: 6) {
                    quick(mobileText("书籍亮点", "Highlights"), mobileText("这本书有哪些亮点？", "What are this book’s highlights?"))
                    quick(mobileText("背景解读", "Background"), mobileText("请解读这本书的背景。", "Explain this book’s background."))
                    quick(mobileText("关键概念", "Key concepts"), mobileText("这本书有哪些关键概念？", "What are the key concepts in this book?"))
                }
                HStack(alignment: .bottom, spacing: 8) {
                    TextField(mobileText("针对本书提出你的问题", "Ask a question about this book"), text: $model.chatDraft, axis: .vertical)
                        .font(.system(size: 16)).lineLimit(1...4).focused($focused).padding(.vertical, 8)
                    Button {
                        if model.chatStream != nil { model.stopChat() }
                        else { model.sendChat(model.chatDraft); focused = false }
                    } label: {
                        Group {
                            if model.chatStream != nil { RoundedRectangle(cornerRadius: 2).fill(.white).frame(width: 12, height: 12) }
                            else { MobileIcon("up", size: 22, tint: .white) }
                        }.frame(width: 40, height: 40).background(palette.active, in: .circle)
                    }.buttonStyle(MobileInkButtonStyle())
                        .disabled(model.chatStream == nil && (model.working || model.chatDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty))
                        .accessibilityLabel(model.chatStream != nil ? mobileText("停止", "Stop") : mobileText("发送", "Send"))
                }.padding(.vertical, 8).padding(.leading, 14).padding(.trailing, 8).frame(minHeight: 52)
                    .background(palette.surface, in: .rect(cornerRadius: 20))
            }.frame(maxWidth: 680).padding(.horizontal, 16).padding(.bottom, 12)
                .frame(maxWidth: .infinity).background(palette.background.ignoresSafeArea(edges: .bottom))
        }
        .foregroundStyle(palette.ink).background(palette.background.ignoresSafeArea())
        .interactiveDismissDisabled()
        .accessibilityAction(.escape, close)
        .background {
            Button(action: close) { EmptyView() }
                .keyboardShortcut(.escape, modifiers: [])
                .opacity(0).accessibilityHidden(true)
        }
        .task { focused = true }
        .alert(mobileText("清空全部记录？", "Clear the conversation?"), isPresented: $confirmClear) {
            Button(mobileText("清空全部记录", "Clear all"), role: .destructive) { model.deleteChat(nil) }
            Button(mobileText("取消", "Cancel"), role: .cancel) {}
        }
    }

    private func quick(_ title: String, _ prompt: String) -> some View {
        Button { model.sendChat(prompt); focused = false } label: {
            Text(title).font(.system(size: 13)).lineLimit(1).minimumScaleFactor(0.7).padding(.horizontal, 4)
                .frame(maxWidth: .infinity, minHeight: 40)
                .background(palette.surface, in: .rect(cornerRadius: 12))
                .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(palette.outline.opacity(0.5), lineWidth: 0.5) }
        }.buttonStyle(MobileInkButtonStyle()).disabled(model.chatStream != nil || model.working)
    }

    private func question(_ content: String) -> some View {
        Text(content).font(.system(size: 16)).lineSpacing(4).padding(14)
            .background(palette.selection, in: .rect(cornerRadius: 16))
            .frame(maxWidth: .infinity, alignment: .trailing).padding(.leading, 35)
    }
}

private struct AssistantAnswer: View {
    let entry: ChatMessage
    @ObservedObject var model: ReaderModel
    let assets: AssistantFormulaCache
    let close: () -> Void
    let clear: () -> Void
    @Environment(\.colorScheme) private var scheme
    @State private var sources: [ChatSource] = []
    @State private var expanded = false
    @State private var more = false

    var body: some View {
        let palette = MobilePalette(scheme)
        MobileGroup {
            VStack(alignment: .leading, spacing: 16) {
                if !entry.progress.isEmpty || entry.thinkingSeconds != nil {
                    DisclosureGroup(entry.thinkingSeconds.map { mobileText("已思考（用时 \($0) 秒）", "Thought for \($0) seconds") } ??
                                    mobileText("思考详情", "Reasoning")) {
                        Text(entry.progress.joined(separator: "\n")).frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 6).padding(.leading, 8)
                            .overlay(alignment: .leading) { Rectangle().fill(palette.outline).frame(width: 2) }
                    }.font(.system(size: 13)).tint(palette.secondary).foregroundStyle(palette.secondary)
                }
                AssistantMarkup(source: entry.content, streaming: false, assets: assets, follow: follow)
                if entry.stopped { Text(mobileText("已停止", "Stopped")).font(.system(size: 13)).foregroundStyle(palette.secondary) }
                if !entry.citations.isEmpty {
                    DisclosureGroup(mobileText("书籍引源", "Book sources") + " · \(entry.citations.count)", isExpanded: $expanded) {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(Array(entry.citations.enumerated()), id: \.offset) { index, locator in
                                let source = sources.first { $0.locator == locator }
                                Button { follow(locator) } label: {
                                    VStack(alignment: .leading, spacing: 5) {
                                        Text(citationNumber(index + 1) + " " + (source?.title ?? mobileText("书中位置", "Location in book")))
                                            .font(.system(size: 14, weight: .medium)).foregroundStyle(palette.accent).lineLimit(1)
                                        if let source {
                                            Text(source.excerpt).font(.system(size: 13)).lineLimit(2).foregroundStyle(palette.secondary)
                                        }
                                    }.frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                                }.buttonStyle(MobileInkButtonStyle())
                                if index < entry.citations.count - 1 { Divider() }
                            }
                        }.padding(.top, 8)
                    }.font(.system(size: 14)).tint(palette.secondary)
                        .task(id: expanded) { if expanded && sources.isEmpty { sources = await model.sources(entry.citations) } }
                }
                HStack {
                    Spacer()
                    MobileIconButton(icon: "more", label: mobileText("更多", "More"), size: 22) { more.toggle() }
                        .frame(width: 36, height: 38).disabled(model.chatStream != nil || model.working)
                        .overlay(alignment: .bottomTrailing) {
                            if more {
                                VStack(spacing: 0) {
                                    menuRow("delete", mobileText("删除", "Delete")) { model.deleteChat(entry.id) }
                                    menuRow("clear", mobileText("清空全部记录", "Clear all")) { clear() }
                                }
                                .frame(width: 184).background(palette.surface, in: .rect(cornerRadius: 13))
                                .shadow(color: .black.opacity(0.17), radius: 12, y: 4)
                                .alignmentGuide(.bottom) { $0[.bottom] + 42 }
                                .zIndex(2)
                            }
                        }
                }.zIndex(2)
            }.padding(18)
        }
    }

    private func menuRow(_ icon: String, _ title: String, action: @escaping () -> Void) -> some View {
        Button { more = false; action() } label: {
            HStack {
                MobileIcon(icon, size: 20)
                Text(title).font(.system(size: 14)).foregroundStyle(MobilePalette(scheme).ink)
                Spacer()
            }.padding(.horizontal, 12).frame(height: 44).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle())
    }

    private func follow(_ target: String) {
        guard !model.working, model.chatStream == nil else { return }
        close()
        model.followCitation(target)
    }
}

private struct AssistantBlock: Decodable, Sendable {
    let kind: String
    let level: Int
    let prefix: String
    let runs: [AssistantRun]
    let rows: [[[AssistantRun]]]
}
private struct AssistantRun: Decodable, Sendable {
    let text: String
    let style: Int
    let link: String
    let math: Bool
}

@MainActor
private final class AssistantFormulaCache {
    static let byteLimit = 8 * 1024 * 1024
    private final class Formula {
        let image: UIImage
        let cost: Int
        init(image: UIImage, cost: Int) { self.image = image; self.cost = cost }
    }
    private let cache = NSCache<NSString, Formula>()

    init() { cache.totalCostLimit = Self.byteLimit; cache.countLimit = 64 }

    func render(_ latex: String, scale: CGFloat, foreground: UInt32) async -> (UIImage, Int)? {
        let key = "\(scale):\(foreground):\(latex)" as NSString
        if let cached = cache.object(forKey: key) { return (cached.image, cached.cost) }
        guard let page = try? await renderChatFormula(latex: latex, fontSize: Float(16 * scale), foreground: foreground),
              !Task.isCancelled, let raster = try? ReaderRaster(page) else { return nil }
        let size = raster.image.bytesPerRow * raster.image.height
        // The shared preview renderer adds 12 units of padding and normally rasterizes at 2×.
        let bitmap = raster.image
        let crop = CGRect(x: 20, y: 20, width: bitmap.width - 40, height: bitmap.height - 40)
        let inline = bitmap.width < 1024 && bitmap.height < 1024 && !crop.isEmpty ? bitmap.cropping(to: crop) ?? bitmap : bitmap
        let image = UIImage(cgImage: inline, scale: 2 * scale, orientation: .up).withBaselineOffset(fromBottom: 3)
        cache.setObject(Formula(image: image, cost: size), forKey: key, cost: size)
        return (image, size)
    }
}

private struct AssistantMarkup: View {
    let source: String
    let streaming: Bool
    let assets: AssistantFormulaCache
    let follow: (String) -> Void
    @Environment(\.colorScheme) private var scheme
    @Environment(\.displayScale) private var scale
    @State private var updates: AsyncStream<String>.Continuation?
    @State private var blocks: [AssistantBlock]?
    @State private var formulas: [String: UIImage] = [:]

    var body: some View {
        let foreground: UInt32 = scheme == .dark ? 0xffe5e8ec : 0xff2b3039
        Group {
            if let blocks { AssistantRichAnswer(blocks: blocks, formulas: formulas, follow: follow) }
            else { Text(source).font(.system(size: 16)).lineSpacing(5) }
        }
        .onChange(of: source) { _, text in if streaming { updates?.yield(text) } }
        .onDisappear { blocks = nil; formulas = [:] }
        .task(id: streaming ? "stream:\(foreground):\(scale)" : "\(foreground):\(scale):\(source)") {
            if !streaming { await prepare(source, foreground: foreground); return }
            let (stream, sink) = AsyncStream<String>.makeStream(bufferingPolicy: .bufferingNewest(1))
            updates = sink
            defer { sink.finish() }
            sink.yield(source)
            for await text in stream {
                await prepare(text, foreground: foreground)
                do { try await Task.sleep(for: .milliseconds(120)) } catch { return }
            }
        }
    }

    private func prepare(_ text: String, foreground: UInt32) async {
        guard let json = try? await chatMarkdown(source: text), !Task.isCancelled,
              let decoded = await Task.detached(priority: .utility, operation: {
                  try? JSONDecoder().decode([AssistantBlock].self, from: Data(json.utf8))
              }).value, !Task.isCancelled else { return }
        var images: [String: UIImage] = [:], seen = Set<String>()
        var remaining = AssistantFormulaCache.byteLimit
        for run in decoded.lazy.flatMap({ $0.runs + $0.rows.flatMap { $0.flatMap { $0 } } }) where run.math && run.link.isEmpty {
            if Task.isCancelled { return }
            if !seen.insert(run.text).inserted { continue }
            if seen.count > 24 || remaining <= 0 { break }
            if let (image, bytes) = await assets.render(run.text, scale: scale, foreground: foreground), bytes <= remaining {
                images[run.text] = image
                remaining -= bytes
            }
        }
        if !Task.isCancelled { blocks = decoded; formulas = images }
    }
}

private struct AssistantRichAnswer: View {
    let blocks: [AssistantBlock]
    let formulas: [String: UIImage]
    let follow: (String) -> Void

    var body: some View {
        let numbered = citationMap()
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                if block.kind == "table" {
                    ScrollView(.horizontal) {
                        Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 8) {
                            ForEach(Array(block.rows.enumerated()), id: \.offset) { _, row in
                                GridRow {
                                    ForEach(Array(row.enumerated()), id: \.offset) { _, runs in
                                        inline(runs, numbers: numbered).padding(4)
                                    }
                                }
                            }
                        }
                    }
                } else if block.kind == "rule" { Divider() }
                else {
                    inline(block.runs, numbers: numbered, prefix: block.prefix)
                        .font(block.kind == "heading" ? .system(size: 17, weight: .semibold) : .system(size: 16))
                        .lineSpacing(5)
                        .padding(.leading, block.kind == "quote" ? 10 : 0)
                }
            }
        }.environment(\.openURL, OpenURLAction { url in
            if url.scheme == "link", url.host == "j" { follow(url.absoluteString); return .handled }
            return ["https", "http"].contains(url.scheme ?? "") ? .systemAction : .discarded
        })
    }

    private func citationMap() -> [String: Int] {
        var values: [String: Int] = [:]
        for block in blocks {
            for run in block.runs + block.rows.flatMap({ $0.flatMap { $0 } }) where run.link.hasPrefix("link://j/") {
                if values[run.link] == nil { values[run.link] = values.count + 1 }
            }
        }
        return values
    }

    private func inline(_ runs: [AssistantRun], numbers: [String: Int], prefix: String = "") -> Text {
        var result = Text(prefix)
        var text = AttributedString()
        for run in runs {
            if run.math, let formula = formulas[run.text] {
                if !text.characters.isEmpty { result = result + Text(text); text = AttributedString() }
                result = result + Text(Image(uiImage: formula))
                continue
            }
            var part = AttributedString(numbers[run.link].map(citationNumber) ?? run.text)
            var intent: InlinePresentationIntent = []
            if run.style & 1 != 0 { intent.insert(.stronglyEmphasized) }
            if run.style & 2 != 0 { intent.insert(.emphasized) }
            if run.style & 8 != 0 || run.math { intent.insert(.code) }
            part.inlinePresentationIntent = intent
            if let url = URL(string: run.link), ["link", "https", "http"].contains(url.scheme ?? "") { part.link = url }
            text.append(part)
        }
        return result + Text(text)
    }
}

private func citationNumber(_ value: Int) -> String {
    let scalar: Int
    switch value {
    case 1...20: scalar = 0x2460 + value - 1
    case 21...35: scalar = 0x3251 + value - 21
    case 36...50: scalar = 0x32b1 + value - 36
    default: return "(\(value))"
    }
    return UnicodeScalar(scalar).map(String.init) ?? "(\(value))"
}
