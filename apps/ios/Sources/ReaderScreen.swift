import SwiftUI
import UIKit

struct ReaderScreen: View {
    let book: BookSummary
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    @StateObject private var model: ReaderModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Environment(\.displayScale) private var scale
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("reader.fontSize") private var fontSize = 20
    @AppStorage("reader.focus") private var preferFocus = false
    @AppStorage("reader.unified") private var unified = false
    @AppStorage("reader.theme") private var theme = "system"
    @AppStorage("reader.brightness") private var savedBrightness = -1.0
    @State private var brightness = 0.5
    @State private var originalBrightness: CGFloat?
    @State private var readingScreen: UIScreen?
    @State private var panel: Panel?
    @State private var selectionHint = 0
    @State private var showSelectionHint = false
    @State private var query = ""
    @State private var notesQuery = ""
    @State private var collapsed: Set<String> = []
    @State private var initializedToc = false
    @State private var assistant = false
    @State private var feedback = false
    @State private var leaving = false
    @State private var moving = false
    @State private var showLoadingTurn = false
    @State private var firstFrameVisible = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showFontNotice = false
    @State private var completion: BookStatistics?
    @State private var finishedCount: UInt32?
    @State private var completionError = false
    @State private var completionRetry = 0
    @State private var panelDrag: CGFloat = 0

    enum Panel: String {
        case contents, search, notes, notesSearch, typography, fonts, layout, mode, selection, theme, compose
        var isLarge: Bool { [.contents, .search, .notes, .notesSearch].contains(self) }
        var isTypography: Bool { [.typography, .fonts, .layout, .mode].contains(self) }
    }

    init(core: MobileCore, book: BookSummary, library: LibraryModel) {
        self.core = core
        self.book = book
        self.library = library
        _model = StateObject(wrappedValue: ReaderModel(core: core, book: book))
    }

    private var dark: Bool { theme == "dark" || (theme == "system" && colorScheme == .dark) }
    private var immersive: Bool { model.frame?.info.isScroll ?? preferFocus }
    private var hideStatusBar: Bool {
        (model.preview?.isImage == true && model.preview?.isFormula != true) || immersive && !model.showingChrome && panel == nil && model.previewedNote == nil && !model.finished && !assistant && !feedback
    }
    private var palette: MobilePalette { MobilePalette(dark ? .dark : .light) }
    private var paper: Color {
        if let color = model.frame?.info.focusFadeColor { return MobilePalette.hex(color) }
        return MobilePalette.hex(dark ? 0x1e2229 : 0xf8f6f1)
    }

    var body: some View {
        GeometryReader { safe in
            ZStack {
                paper.ignoresSafeArea()
                GeometryReader { geometry in
                    ZStack {
                        if let frame = model.frame {
                            ReadingCanvas(frame: frame, selection: model.selection,
                                          enabled: !model.working && !model.finished && panel == nil && model.preview?.document == nil && !leaving,
                                          turn: model.turn, activate: model.activate, select: model.select,
                                          splitSentences: model.setSentenceSplit, moving: { moving = $0 },
                                          turnMotion: model.turnMotion, turnPending: model.turnPending, neighbours: model.neighbours,
                                          beginHandle: model.beginHandle, moveHandle: model.moveHandle)
                            .opacity(firstFrameVisible ? 1 : 0)
                            .onAppear {
                                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.15)) { firstFrameVisible = true }
                            }
                        } else if model.openFailed {
                            Button(mobileText("重试", "Try again")) { model.retry() }.buttonStyle(.bordered)
                        } else {
                            ProgressView().tint(palette.accent).accessibilityLabel(mobileText("正在打开书籍", "Opening book"))
                        }
                        if showLoadingTurn {
                            ProgressView().tint(palette.accent)
                                .padding(12).background(palette.surface, in: .circle)
                                .accessibilityLabel(mobileText("正在加载内容", "Loading content"))
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .onChange(of: geometry.size, initial: true) { _, size in update(size) }
                    .onChange(of: scale) { _, _ in update(geometry.size) }
                    .onChange(of: dark) { _, _ in update(geometry.size) }
                }
                .ignoresSafeArea(.container, edges: immersive ? [.top, .bottom] : [])
                .ignoresSafeArea(.keyboard)
                .accessibilityHidden(panel != nil || model.finished || model.preview?.document != nil)

                Group {
                    if model.finished {
                        completionPage.transition(.opacity)
                    } else {
                        readerOverlays(height: safe.size.height)
                        if let selection = model.selection, panel == nil, !moving, !model.working {
                            GeometryReader { surface in
                                anchoredSelection(selection, size: surface.size, safe: safe.safeAreaInsets)
                            }.ignoresSafeArea(.container, edges: immersive ? [.top, .bottom] : [])
                        }
                    }
                }.accessibilityHidden(model.preview?.document != nil)

                if let preview = model.preview, preview.document != nil {
                    Color.black.opacity(0.32).ignoresSafeArea()
                        .onTapGesture(perform: model.closePreview).accessibilityHidden(true)
                        .transition(.opacity)
                    VStack {
                        Spacer(minLength: 0)
                        ReaderReferenceSheet(initialPreview: preview, model: model, fontSize: fontSize,
                                             availableHeight: safe.size.height)
                    }.transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
            }
            .foregroundStyle(palette.ink)
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.18), value: model.preview?.document != nil)
        }
        .statusBarHidden(hideStatusBar)
        .preferredColorScheme(theme == "system" ? nil : (dark ? .dark : .light))
        .environment(\.colorScheme, dark ? .dark : .light)
        .overlay { ReaderWindowScreen { screen in configureBrightness(screen) }.allowsHitTesting(false) }
        .interactiveDismissDisabled()
        .accessibilityAction(.escape, handleBack)
        .background {
            Button(action: handleBack) { EmptyView() }
                .keyboardShortcut(.escape, modifiers: [])
                .opacity(0).accessibilityHidden(true)
        }
        .fullScreenCover(isPresented: $assistant, onDismiss: model.stopChat) {
            ReadingAssistantScreen(book: book, model: model) { assistant = false }
                .preferredColorScheme(dark ? .dark : .light)
        }
        .fullScreenCover(isPresented: $feedback) {
            FeedbackScreen(core: core, reader: model.feedbackReader)
                .preferredColorScheme(dark ? .dark : .light)
        }
        .overlay {
            if model.preview?.isImage == true, let raster = model.preview?.raster {
                Group {
                    if let preview = model.preview, preview.isFormula {
                        FormulaPreview(raster: raster, source: preview.text, close: model.closePreview)
                    } else {
                        FullImagePreview(raster: raster) { model.closePreview() }
                    }
                }.transition(.opacity)
            }
        }
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.15), value: model.preview?.isImage == true)
        .alert("Torto", isPresented: Binding(get: { model.error != nil }, set: { if !$0 { model.error = nil } })) {
            Button("OK", role: .cancel) { model.error = nil }
        } message: { Text(model.error ?? "") }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)) { _ in model.trimMemory() }
        .onChange(of: model.externalURL) { _, url in if let url { openURL(url); model.externalURL = nil } }
        .onChange(of: library.syncRevision) { _, _ in model.checkRemoteProgress() }
        .onChange(of: library.downloadedFonts) { _, count in
            if count > 0 && library.fontsReady && model.frame != nil { showFontNotice = true }
        }
        .onChange(of: panel) { _, _ in panelDrag = 0 }
        .onChange(of: model.editingNote) { _, note in if note != nil { panel = .compose } }
        .onChange(of: model.frame?.info.activeTocPath) { _, path in
            if let path { collapsed.subtract(path) }
        }
        .onChange(of: aiActivity, initial: true) { _, activity in
            model.setAiVisibility(active: activity.active, obscured: activity.obscured)
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { applyBrightness() } else { restoreBrightness() }
        }

        .task(id: model.turnPending) {
            if !model.turnPending { showLoadingTurn = false; return }
            do { try await Task.sleep(for: .milliseconds(170)) } catch { return }
            showLoadingTurn = true
        }
        .task(id: showFontNotice) {
            guard showFontNotice else { return }
            do { try await Task.sleep(for: .seconds(5)) } catch { return }
            showFontNotice = false
        }
        .task(id: model.aiNoticeRevision) {
            guard model.aiNotice != nil else { return }
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            model.clearAiNotice()
        }
        .task(id: selectionHint) {
            guard selectionHint > 0 else { return }
            showSelectionHint = true
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            showSelectionHint = false
        }
        .task(id: "\(model.finished):\(model.markedFinished):\(completionRetry)") {
            guard model.finished else { return }
            completionError = false
            do {
                let (detail, count) = try await model.completionStatistics()
                guard !Task.isCancelled else { return }
                completion = detail
                finishedCount = count
            } catch {
                if !Task.isCancelled { completionError = true }
            }
        }
        .task(id: activityEligible) {
            let eligible = activityEligible
            await model.recordActivity(eligible: eligible, interaction: eligible)
            guard eligible else { return }
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(5)) } catch { return }
                await model.recordActivity(eligible: true, interaction: false)
            }
        }
    }

    private struct AiActivity: Equatable {
        var active: Bool
        var obscured: Bool
    }
    private var aiActivity: AiActivity {
        AiActivity(active: scenePhase == .active && !leaving,
                   obscured: assistant || feedback || panel != nil || moving || model.finished || model.preview?.isImage == true)
    }
    private var activityEligible: Bool {
        aiActivity.active && !aiActivity.obscured && model.frame != nil && model.preview == nil && model.previewedNote == nil
    }

    @ViewBuilder private func readerOverlays(height: CGFloat) -> some View {
        if let panel {
            if !panel.isLarge {
                Color.black.opacity(panel == .compose ? 0.52 : 0.12).ignoresSafeArea()
                    .onTapGesture { closePanel() }.accessibilityHidden(true)
            } else { palette.background.ignoresSafeArea().accessibilityHidden(true) }
        }
        VStack(spacing: 0) {
            if model.previewedNote != nil && panel == nil { noteHeader.padding(.horizontal, 16).padding(.top, 12) }
            else if model.showingChrome && panel == nil { header }
            else if panel?.isLarge == true { Color.clear.frame(height: 12) }
            VStack(spacing: 6) {
                if model.resumeToken != nil && panel == nil {
                    HStack(spacing: 8) {
                        Text(mobileText("发现其他设备进度", "Reading progress from another device")).font(.footnote)
                        Button(mobileText("跳转", "Go to")) { model.resumeRemoteProgress() }
                        MobileIconButton(icon: "close", label: mobileText("关闭", "Dismiss"), size: 16) { model.resumeToken = nil }
                            .frame(width: 30, height: 30)
                    }.padding(.leading, 10).padding(.trailing, 4)
                        .background(palette.surface, in: .capsule).disabled(model.working)
                }
                if let notice = model.aiNotice {
                    Text(notice == "translation" ? mobileText("翻译服务错误", "Translation service error") : mobileText("AI 排版服务错误", "AI layout service error")).font(.system(size: 13)).foregroundStyle(palette.inverseInk)
                        .padding(.horizontal, 12).padding(.vertical, 8).background(palette.inverse, in: .rect(cornerRadius: 8))
                }
            }.padding(.top, 6)
            if panel?.isLarge != true { Spacer(minLength: 0) }
            if panel == nil {
                if model.selection == nil && showFontNotice {
                    HStack {
                        Text(mobileText("新字体已下载完成", "Reading fonts downloaded")).font(.footnote)
                        Button(mobileText("应用", "Apply")) { showFontNotice = false; model.applyFonts() }.disabled(model.working)
                    }.padding(12).background(palette.surface, in: .capsule).padding(.bottom, 8)
                } else if model.selection == nil && model.showingChrome && model.previewedNote == nil && model.frame != nil {
                    floatingActions
                }
            }
            if panel == nil, model.showingChrome, model.selection == nil, model.previewedNote == nil {
                let controls = model.frame?.info.controls.filter { $0.kind != "sentences" } ?? []
                if !controls.isEmpty { ReaderControls(controls: controls, action: model.toggleControl).disabled(model.working) }
            }
            if let panel {
                panelContent(panel, height: height)
                    .frame(maxWidth: .infinity)
                    .background((panel.isLarge ? palette.background : panel == .compose ? palette.background : palette.surface)
                        .ignoresSafeArea(edges: .bottom))
                    .offset(y: panelDrag)
                    .transition(.offset(y: 24).combined(with: .opacity))
            }
            if let note = model.previewedNote, panel == nil { noteFooter(note).padding(.horizontal, 16).padding(.bottom, 12) }
            else if (model.showingChrome || panel != nil) && panel != .compose { footer }
        }
        .animation(.easeOut(duration: 0.15), value: panel)
        .animation(.easeOut(duration: 0.15), value: model.showingChrome)
    }

    private var header: some View {
        HStack {
            MobileIconButton(icon: "back", label: mobileText("书架", "Library"), action: leave).disabled(leaving)
            Spacer()
            MobileIconButton(icon: "feedback", label: mobileText("反馈问题", "Report an issue")) { feedback = true }
                .disabled(model.working)
        }
        .padding(.horizontal, 8).frame(height: 56)
        .background(palette.surface.ignoresSafeArea(edges: .top))
        .overlay(alignment: .bottom) { Rectangle().fill(palette.outline).frame(height: 0.5) }
    }

    private var footer: some View {
        HStack(spacing: 0) {
            footerAction("toc", mobileText("目录", "Contents"), [.contents, .search].contains(panel)) { open(.contents) }
            footerAction("marks", mobileText("笔记", "Notes"), [.notes, .notesSearch].contains(panel)) { open(.notes) }
            footerAction("select", mobileText("文字选择模式", "Selection mode"), panel == .selection) {
                if model.translated { selectionHint += 1 } else { open(.selection) }
            }
            footerAction("sun", mobileText("主题", "Theme"), panel == .theme) { open(.theme) }
            footerAction("type", mobileText("排版", "Typography"), panel?.isTypography == true) { open(.typography) }
        }
        .frame(height: 56)
        .background(palette.surface.ignoresSafeArea(edges: .bottom))
        .overlay(alignment: .top) { Rectangle().fill(palette.outline).frame(height: 0.5) }
        .overlay(alignment: .top) {
            if showSelectionHint {
                Text(mobileText("翻译时仅支持按段选择", "Only paragraph selection is available during translation"))
                    .font(.system(size: 13)).foregroundStyle(palette.inverseInk).multilineTextAlignment(.center)
                    .padding(9).background(palette.inverse, in: .rect(cornerRadius: 6)).offset(y: -46)
            }
        }
        .disabled(model.frame == nil || model.working || leaving)
    }

    private func footerAction(_ icon: String, _ title: String, _ selected: Bool, _ action: @escaping () -> Void) -> some View {
        MobileIconButton(icon: icon, label: title, selected: selected, action: action).frame(maxWidth: .infinity)
    }

    private var floatingActions: some View {
        HStack(spacing: 10) {
            floating("ai", mobileText("阅读助手", "Reading assistant")) { assistant = true }
            if model.offersTranslation {
                floating("translate", model.translated ? mobileText("关闭翻译", "Turn translation off") :
                            mobileText("开启翻译", "Turn translation on"), selected: model.translated, action: model.toggleTranslation)
            }
        }.frame(maxWidth: .infinity, alignment: .trailing).padding(.horizontal, 16).padding(.bottom, 16)
            .disabled(model.working)
    }

    private func floating(_ icon: String, _ label: String, selected: Bool = false, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            MobileIcon(icon, tint: selected ? palette.active : .white)
                .frame(width: 48, height: 48).background(MobilePalette.hex(0x5e646e), in: .circle)
        }.buttonStyle(MobileInkButtonStyle()).accessibilityLabel(label).accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var noteHeader: some View {
        MobileHeader(title: mobileText("笔记", "Notes"), subtitle: model.previewedNote?.chapterTitle, backIcon: "toc",
                     background: .clear, height: 48, backSize: 24, subtitleSize: 11, back: { open(.notes) }) {
            MobileIconButton(icon: "close", label: mobileText("退出笔记预览", "Exit note preview"), size: 24, action: model.closeNotePreview)
        }
        .background { RoundedRectangle(cornerRadius: 10).fill(palette.surface.opacity(0.97)).frame(height: 40) }
    }

    private func noteFooter(_ note: Annotation) -> some View {
        let index = model.notes.firstIndex { $0.id == note.id }
        return HStack(spacing: 0) {
            noteAction("marks", mobileText("笔记列表", "Notes list")) { open(.notes) }
            Rectangle().fill(palette.outline).frame(width: 0.5, height: 20)
            noteAction("previous", mobileText("上一个笔记", "Previous note")) { model.stepNote(false) }
                .disabled(index == nil || index == 0)
            Rectangle().fill(palette.outline).frame(width: 0.5, height: 20)
            noteAction("next", mobileText("下一个笔记", "Next note")) { model.stepNote(true) }
                .disabled(index == nil || index == model.notes.count - 1)
        }.background { RoundedRectangle(cornerRadius: 10).fill(palette.surface.opacity(0.97)).frame(height: 40) }
            .disabled(model.working)
    }

    private func noteAction(_ icon: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 1) {
                MobileIcon(icon, size: 24)
                Text(label).font(.system(size: 11)).lineLimit(1).foregroundStyle(palette.secondary)
            }.frame(maxWidth: .infinity, minHeight: 48).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle())
    }

    @ViewBuilder private func anchoredSelection(_ selection: SelectedText, size: CGSize, safe: EdgeInsets) -> some View {
        let bounds = selection.bounds.filter { $0.y + $0.height > 0 && $0.y < Float(size.height) }
        if let first = bounds.first, let last = bounds.last {
            let width: CGFloat = 216, height: CGFloat = 64
            let top = (immersive ? safe.top : 0) + (model.showingChrome ? 58 : 6)
            let bottom = size.height - (immersive ? safe.bottom : 0) - (model.showingChrome ? 60 : 8)
            let above = CGFloat(first.y) - height - 12
            let below = CGFloat(last.y + last.height) + 12
            let x = min(size.width - width / 2 - 8, max(width / 2 + 8, CGFloat(first.x + first.width / 2)))
            let y = above >= top ? above : below + height <= bottom ? below : max(top, bottom - height)
            selectionBar(selection).position(x: x, y: y + height / 2)
        }
    }

    private func selectionBar(_ selection: SelectedText) -> some View {
        HStack(spacing: 0) {
            selectionAction("copy", mobileText("复制", "Copy")) {
                UIPasteboard.general.string = selection.text
                model.clearSelection()
            }
            selectionAction(selection.highlighted ? "remove_underline" : "underline",
                            selection.highlighted ? mobileText("删除划线", "Remove") : mobileText("划线", "Highlight"), action: model.toggleHighlight)
            selectionAction("thought", mobileText("写想法", "Write a note")) { panel = .compose }
        }
        .fixedSize(horizontal: true, vertical: false)
        .padding(.horizontal, 6)
        .background(MobilePalette.hex(0x3d3d3d), in: .rect(cornerRadius: 18))
        .shadow(color: .black.opacity(0.16), radius: 5, y: 2)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(mobileText("划选工具栏", "Selection actions"))
        .disabled(model.working)
    }

    private func selectionAction(_ icon: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 3) {
                MobileIcon(icon, size: 24, tint: .white)
                Text(label).font(.system(size: 12, weight: .medium)).foregroundStyle(.white)
                    .lineLimit(1).minimumScaleFactor(0.8)
            }.frame(width: 68, height: 64).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle())
            .accessibilityLabel(label).accessibilityIdentifier("selection-" + icon)
    }

    @ViewBuilder private func panelContent(_ panel: Panel, height: CGFloat) -> some View {
        switch panel {
        case .contents: contentsPanel
        case .search: searchPanel
        case .notes: notesPanel(searching: false)
        case .notesSearch: notesPanel(searching: true)
        case .typography:
            ReaderTypographyPanel(size: fontSize, canFocus: model.focusAvailable, focused: model.frame?.info.isScroll == true,
                                  change: { size in
                fontSize = size; model.changeFont(Float(size), dark: dark)
            }, fonts: { self.panel = .fonts }, layout: { self.panel = .layout }, mode: { self.panel = .mode })
                .simultaneousGesture(dragPanel())
        case .fonts:
            VStack(spacing: 0) {
                panelHeader(mobileText("字体样式", "Font styles")) { self.panel = .typography }
                SettingsScroll {
                    FontSettings(core: core, library: library, apply: model.applyFonts) { EmptyView() }
                    if !library.fontsReady { MobileGroup { FontDownloadRow(library: library) } }
                }.frame(maxHeight: height * 0.58)
            }
        case .layout:
            VStack(spacing: 0) {
                panelHeader(mobileText("AI 排版", "AI layout")) { self.panel = .typography }
                AiRoleSettings(core: core, role: .layout, onSave: model.reloadAI, onSaveError: { model.error = $0 })
                    .frame(height: height * 0.54)
            }
        case .mode:
            let focused = model.frame?.info.isScroll == true
            VStack(spacing: 0) {
                panelHeader(mobileText("阅读模式", "Reading mode")) { self.panel = .typography }
                VStack(spacing: 12) {
                    choiceCard(mobileText("专注模式", "Focus mode"), selected: focused) {
                        preferFocus = true
                        if !focused { model.changeMode(true) }
                    }.disabled(!model.focusAvailable || model.working)
                    choiceCard(mobileText("经典模式", "Classic mode"), selected: !focused) {
                        preferFocus = false
                        if focused { model.changeMode(false) }
                    }.disabled(model.working)
                    if !focused {
                        MobileGroup {
                            MobileToggle(title: mobileText("统一版式", "Unified layout"), value: Binding(get: { unified }, set: { value in
                                unified = value
                                model.changeFont(Float(fontSize), dark: dark, unified: value)
                            })).disabled(model.working)
                        }
                    }
                }.padding(16)
            }.simultaneousGesture(dragPanel())
        case .selection:
            VStack(spacing: 0) {
                panelHeader(mobileText("文字选择模式", "Selection mode"), back: closePanel)
                VStack(spacing: 12) {
                    ForEach(selectionModes, id: \.id) { mode in
                        choiceCard(mode.title, selected: mode.id == model.selectionMode) {
                            if mode.id != model.selectionMode { model.chooseSelection(mode.id) }
                            closePanel()
                        }
                    }
                }.padding(16)
            }.simultaneousGesture(dragPanel())
        case .theme:
            VStack(alignment: .leading, spacing: 20) {
                ReadingCapsuleSlider(value: Binding(get: { brightness }, set: {
                    brightness = $0
                    if scenePhase == .active { readingScreen?.brightness = $0 }
                }), range: 0.01...1, label: mobileText("亮度", "Brightness")) { savedBrightness = $0 }
                VStack(alignment: .leading, spacing: 8) {
                    Text(mobileText("颜色", "Color")).font(.system(size: 12)).foregroundStyle(palette.secondary)
                    HStack(spacing: 12) {
                        swatch(false, mobileText("浅色", "Light"), "sun")
                        swatch(true, mobileText("深色", "Dark"), "moon")
                    }
                }
            }.padding(20).simultaneousGesture(dragPanel())
        case .compose:
            NoteComposer(quote: model.editingNote?.quote ?? model.selection?.text ?? "",
                         initial: model.editingNote?.note ?? "", saving: model.working) { text in
                model.writeNote(text) { self.panel = nil }
            }
        }
    }

    private var selectionModes: [MobileChoice] {
        [MobileChoice("free", mobileText("自由", "Free")),
         MobileChoice("word", mobileText("单词", "Word")),
         MobileChoice("sentence", mobileText("句子", "Sentence")),
         MobileChoice("paragraph", mobileText("段落", "Paragraph"))]
            .filter { $0.id != "word" || model.wordSelectionAvailable }
    }

    private func choiceCard(_ title: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Text(title).font(.system(size: 16)).foregroundStyle(selected ? palette.accent : palette.ink)
                Spacer()
                if selected { MobileIcon("check", size: 24, tint: palette.accent) }
            }.padding(16).frame(minHeight: 56)
                .background(selected ? palette.accent.opacity(0.05) : palette.raised.opacity(0.45), in: .rect(cornerRadius: 12))
                .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(selected ? palette.accent.opacity(0.6) : .clear) }
        }.buttonStyle(MobileInkButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func swatch(_ darkChoice: Bool, _ title: String, _ icon: String) -> some View {
        Button { theme = darkChoice ? "dark" : "light" } label: {
            let selected = dark == darkChoice
            let tint: Color = selected ? palette.active : darkChoice ? .white : MobilePalette.hex(0x252a32)
            HStack(spacing: 8) {
                MobileIcon(icon, size: 20, tint: selected || darkChoice ? tint : palette.icon)
                Text(title).font(.system(size: 14)).foregroundStyle(tint)
            }.padding(12).frame(maxWidth: .infinity, minHeight: 56)
                .background(darkChoice ? MobilePalette.hex(0x222831) : .white, in: .rect(cornerRadius: 12))
                .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(selected ? palette.active : palette.outline.opacity(0.4),
                                                                            lineWidth: selected ? 2 : 1) }
        }.buttonStyle(MobileInkButtonStyle()).accessibilityAddTraits(dark == darkChoice ? .isSelected : [])
    }

    private func panelHeader(_ title: String, back: @escaping () -> Void) -> some View {
        MobileHeader(title: title, background: .clear, height: 48, inset: 8, back: back)
    }

    private var contentsPanel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                Button { panel = .search } label: {
                    HStack(spacing: 6) {
                        MobileIcon("search", size: 24)
                        Text(mobileText("搜本书", "Search book")).font(.system(size: 14)).foregroundStyle(palette.secondary)
                    }.frame(maxWidth: .infinity, minHeight: 34).contentShape(Rectangle())
                }.buttonStyle(MobileInkButtonStyle())
                Text(mobileText("目录", "Contents")).font(.system(size: 14, weight: .medium))
                    .frame(maxWidth: .infinity, minHeight: 34)
                    .background(palette.surface, in: .capsule)
                    .accessibilityAddTraits(.isHeader)
            }.padding(3).background(palette.raised, in: .capsule).padding(.vertical, 4)
                .padding(.horizontal, 16).simultaneousGesture(dragPanel())
            Text(book.title).font(.system(size: 18, weight: .semibold)).lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.leading, 48).padding(.trailing, 24).padding(.top, 20).padding(.bottom, 16)
            Divider().padding(.horizontal, 20)
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 0) {
                        if model.toc.isEmpty && !model.working {
                            Text(mobileText("本书暂无目录", "No contents available")).font(.system(size: 14)).padding(32)
                        }
                        ForEach(model.toc.filter { collapsed.isDisjoint(with: $0.ancestors) }, id: \.id) { row in
                            let current = model.frame?.info.activeTocId == row.id
                            HStack(spacing: 0) {
                                if row.hasChildren {
                                    MobileIconButton(icon: collapsed.contains(row.id) ? "next" : "expand",
                                                     label: mobileText("展开或收起", "Expand or collapse"), size: 20) {
                                        if collapsed.contains(row.id) { collapsed.remove(row.id) } else { collapsed.insert(row.id) }
                                    }
                                } else { Spacer().frame(width: 48) }
                                Button {
                                    closePanel(); model.closeNotePreview(); model.goToToc(row.id)
                                } label: {
                                    Text(row.title).font(.system(size: 16, weight: current ? .semibold : .regular))
                                        .lineLimit(2).padding(.vertical, 14).frame(maxWidth: .infinity, minHeight: 52, alignment: .leading)
                                        .foregroundStyle(current ? palette.accent : palette.ink).contentShape(Rectangle())
                                }.buttonStyle(MobileInkButtonStyle()).disabled(row.href.isEmpty)
                            }.padding(.leading, CGFloat(min(row.depth, 5)) * 16)
                                .padding(.trailing, 24)
                                .id(row.id)
                            Rectangle().fill(palette.outline.opacity(0.35)).frame(height: 0.5).padding(.horizontal, 20)
                        }
                    }.padding(.vertical, 8)
                }
                .onScrollGeometryChange(for: Bool.self) { $0.contentOffset.y + $0.contentInsets.top <= 1 } action: { _, atTop in tocAtTop = atTop }
                .simultaneousGesture(dragPanel(allowed: { tocAtTop }))
                .task(id: model.toc.isEmpty) {
                    if !model.toc.isEmpty {
                        let path = Set(model.frame?.info.activeTocPath ?? [])
                        if !initializedToc {
                            collapsed = Set(model.toc.filter(\.hasChildren).map(\.id)).subtracting(path)
                            initializedToc = true
                        } else { collapsed.subtract(path) }
                    }
                    if let id = model.frame?.info.activeTocId {
                        await Task.yield()
                        proxy.scrollTo(id, anchor: .center)
                    }
                }
            }
        }.frame(maxHeight: .infinity)
    }

    private var searchPanel: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                MobileSearchField(title: mobileText("搜本书", "Search this book"), query: $query, autofocus: true, dismissOnSubmit: true) {
                    model.search(query)
                }
                Button(mobileText("取消", "Cancel")) { panel = .contents }.font(.system(size: 14))
            }.padding(.horizontal, 16).padding(.vertical, 4)
            switch model.searchState {
            case .idle: Spacer()
            case .searching:
                searchStatus(mobileText("正在搜索…", "Searching…"))
                Spacer()
            case .failed:
                searchStatus(mobileText("暂时无法搜索本书", "Unable to search this book"))
                Button(mobileText("重试", "Retry")) { model.search(query) }.padding(.horizontal, 20)
                Spacer()
            case .results(let hits):
                searchStatus(mobileText("找到 \(hits.count) 处结果", hits.count == 1 ? "1 result" : "\(hits.count) results"))
                searchResults(hits)
            }
        }.frame(maxHeight: .infinity)
            .onChange(of: query) { _, _ in model.clearSearch() }
            .onDisappear { model.clearSearch() }
    }

    private func searchStatus(_ message: String) -> some View {
        Text(message).font(.system(size: 13)).foregroundStyle(palette.secondary)
            .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 20).padding(.vertical, 8)
    }

    private func searchResults(_ hits: [SearchHit]) -> some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(Array(hits.enumerated()), id: \.offset) { _, hit in
                    Button {
                        closePanel(); model.closeNotePreview(); model.goToSource(hit.source)
                    } label: {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(hit.title).font(.system(size: 12)).foregroundStyle(palette.accent)
                            Text(hit.excerpt).font(.system(size: 14))
                        }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 16).contentShape(Rectangle())
                    }.buttonStyle(MobileInkButtonStyle())
                    Divider()
                }
            }.padding(.horizontal, 20)
        }.scrollDismissesKeyboard(.interactively)
    }

    private func notesPanel(searching: Bool) -> some View {
        let term = notesQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        let matches = searching ? (term.isEmpty ? [] : model.notes.filter {
            $0.quote.localizedCaseInsensitiveContains(term) || ($0.note ?? "").localizedCaseInsensitiveContains(term)
        }) : model.notes
        return VStack(spacing: 0) {
            if !searching {
                Text(mobileText("笔记", "Notes")).font(.system(size: 16)).frame(maxWidth: .infinity)
                    .padding(.vertical, 12).contentShape(Rectangle()).accessibilityAddTraits(.isHeader)
                    .simultaneousGesture(dragPanel())
            }
            if searching {
                HStack(spacing: 4) {
                    MobileSearchField(title: mobileText("搜索笔记", "Search notes"), query: $notesQuery, autofocus: true, dismissOnSubmit: true)
                    Button(mobileText("取消", "Cancel")) { panel = .notes }.font(.system(size: 14)).padding(.trailing, 12)
                }.background(palette.raised, in: .capsule).padding(.horizontal, 16).padding(.vertical, 4)
                if !term.isEmpty {
                    searchStatus(mobileText("找到 \(matches.count) 条笔记", matches.count == 1 ? "1 note found" : "\(matches.count) notes found"))
                }
            }
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    if !searching {
                        MobileGroup {
                            HStack {
                                VStack(alignment: .leading, spacing: 8) {
                                    Text(book.title).font(.system(size: 18, weight: .semibold)).lineLimit(2)
                                    Text(mobileText("共 \(model.notes.count) 条笔记", "\(model.notes.count) notes"))
                                        .font(.system(size: 14)).foregroundStyle(palette.secondary)
                                }.frame(maxWidth: .infinity, alignment: .leading)
                                BookCover(id: book.id, core: core, revision: library.coverRevision).frame(width: 54)
                            }.padding(16)
                            Divider().padding(.horizontal, 16)
                            MobileRow(title: mobileText("搜索笔记", "Search notes"), icon: "search") {
                                notesQuery = ""; panel = .notesSearch
                            }
                        }
                    }
                    ForEach(noteGroups(matches), id: \.id) { group in
                        Text(group.title).font(.system(size: 16, weight: .medium)).padding(.horizontal, 4).padding(.top, 12).padding(.bottom, 2)
                        ForEach(group.notes, id: \.id) { note in
                            MobileGroup {
                                Button {
                                    closePanel(); model.beginNotePreview(note)
                                } label: {
                                    ReaderNoteCard(note: note)
                                }.buttonStyle(MobileInkButtonStyle())
                            }
                        }
                    }
                }.padding(16)
            }
            .scrollDismissesKeyboard(.interactively)
            .onScrollGeometryChange(for: Bool.self) { $0.contentOffset.y + $0.contentInsets.top <= 1 } action: { _, atTop in notesAtTop = atTop }
            .simultaneousGesture(dragPanel(allowed: { notesAtTop && !searching }))
        }.frame(maxHeight: .infinity)
    }

    @State private var notesAtTop = true
    @State private var tocAtTop = true
    @State private var dragStartedAtTop: Bool?
    private func dragPanel(allowed: @escaping () -> Bool = { true }) -> some Gesture {
        DragGesture(minimumDistance: 14).onChanged { value in
            if dragStartedAtTop == nil { dragStartedAtTop = allowed() }
            guard dragStartedAtTop == true,
                  value.translation.height > abs(value.translation.width) * 1.4 else { return }
            panelDrag = max(0, value.translation.height)
        }.onEnded { value in
            let dismiss = panelDrag > 86 || (panelDrag > 24 && value.predictedEndTranslation.height > 170)
            dragStartedAtTop = nil
            if dismiss { closePanel() }
            withAnimation(.easeOut(duration: 0.15)) { panelDrag = 0 }
        }
    }

    private struct NoteGroup {
        let id: String
        let title: String
        var notes: [Annotation]
    }

    private func noteGroups(_ matches: [Annotation]) -> [NoteGroup] {
        var indexes: [String: Int] = [:], groups: [NoteGroup] = []
        for note in matches {
            if let index = indexes[note.chapterId] { groups[index].notes.append(note) }
            else {
                indexes[note.chapterId] = groups.count
                groups.append(NoteGroup(id: note.chapterId, title: note.chapterTitle.isEmpty ? book.title : note.chapterTitle, notes: [note]))
            }
        }
        return groups
    }

    private var completionPage: some View {
        let done = model.markedFinished || completion?.status == "finished"
        return VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 18) {
                    Text(done ? mobileText("已读完", "Finished") : mobileText("全书完", "End of book"))
                        .font(.system(size: 29, weight: .bold, design: .serif))
                    ZStack {
                        Circle().fill(palette.selection).frame(width: 122, height: 122)
                        if done {
                            VStack(spacing: 4) {
                                Text(mobileText("读完的第", "Books finished")).font(.system(size: 13))
                                Text(finishedCount?.formatted() ?? "—").font(.system(size: 45, weight: .bold, design: .serif))
                            }.foregroundStyle(palette.accent)
                        } else { MobileIcon("book", size: 58, tint: palette.accent) }
                    }
                    if !done {
                        Button(mobileText("标记读完", "Mark as finished"), action: model.markFinished)
                            .font(.system(size: 16, weight: .semibold)).disabled(model.working || completion == nil)
                    }
                    ReadingCompletionSummary(statistics: completion, failed: completionError) { completionRetry += 1 }
                }.frame(maxWidth: 600).padding(18).padding(.top, 12).frame(maxWidth: .infinity)
            }
            HStack {
                Button(mobileText("返回正文", "Back to reading")) { model.finished = false }.frame(maxWidth: .infinity)
                    .keyboardShortcut(.leftArrow, modifiers: [])
                Button(mobileText("退出阅读", "Exit reading"), action: leave).frame(maxWidth: .infinity)
            }.buttonStyle(.bordered).padding(16).background(palette.surface.ignoresSafeArea(edges: .bottom))
        }.background(palette.background.ignoresSafeArea())
            .simultaneousGesture(DragGesture(minimumDistance: 36).onEnded { if $0.translation.width > 36 && abs($0.translation.height) < $0.translation.width { model.finished = false } })
    }

    private func open(_ target: Panel) {
        if panel == target || (target == .typography && panel?.isTypography == true) { closePanel(); return }
        if target == .contents { model.loadToc() }
        if target == .notes { model.loadNotes() }
        panel = target
    }

    private func closePanel() {
        if panel == .compose {
            guard !model.working || model.editingNote == nil && model.selection == nil else { return }
            model.stopEditingNote()
        }
        panel = nil
        dragStartedAtTop = nil
    }

    private func update(_ size: CGSize) {
        model.updateViewport(size: size, scale: scale, dark: dark, font: Float(fontSize),
                             focus: preferFocus, unified: unified)
    }

    private func configureBrightness(_ screen: UIScreen) {
        guard readingScreen !== screen else { return }
        readingScreen = screen
        originalBrightness = screen.brightness
        brightness = savedBrightness < 0 ? max(0.01, screen.brightness) : savedBrightness
        applyBrightness()
    }
    private func applyBrightness() {
        if savedBrightness >= 0 { readingScreen?.brightness = savedBrightness }
    }
    private func restoreBrightness() {
        if let originalBrightness { readingScreen?.brightness = originalBrightness }
    }

    private func handleBack() {
        if model.preview != nil { model.closePreview() }
        else if panel == .search { panel = .contents }
        else if panel == .notesSearch { panel = .notes }
        else if panel == .fonts || panel == .layout || panel == .mode { panel = .typography }
        else if panel != nil { closePanel() }
        else if model.selection != nil { model.clearSelection() }
        else if model.previewedNote != nil { open(.notes) }
        else { leave() }
    }

    private func leave() {
        guard !leaving else { return }
        leaving = true
        restoreBrightness()
        Task { await model.close(); dismiss() }
    }
}

private struct ReaderWindowScreen: UIViewRepresentable {
    let screen: (UIScreen) -> Void
    func makeUIView(context: Context) -> ScreenObserver { ScreenObserver() }
    func updateUIView(_ view: ScreenObserver, context: Context) { view.onScreen = screen }
    final class ScreenObserver: UIView {
        var onScreen: ((UIScreen) -> Void)?
        override func didMoveToWindow() {
            super.didMoveToWindow()
            if let screen = window?.windowScene?.screen {
                Task { @MainActor [weak self] in self?.onScreen?(screen) }
            }
        }
    }
}
