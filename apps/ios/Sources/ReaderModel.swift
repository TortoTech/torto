import CoreGraphics
import CoreText
import Foundation
import SwiftUI

enum ReferenceSheetStyle {
    static let inset: CGFloat = 20
    static let maxDocumentHeight: CGFloat = 520
    static let controlHeight: CGFloat = 48
    static func background(dark: Bool) -> UInt32 { dark ? 0xff121416 : 0xfff4f5f7 }
    static func ink(dark: Bool) -> UInt32 { dark ? 0xffe5e8ec : 0xff2b3039 }
    static func markerSize(_ fontSize: CGFloat) -> CGFloat { max(14, fontSize * 0.8) }
    static func indent(marker: String, fontSize: CGFloat) -> Float {
        guard !marker.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return 0 }
        let size = markerSize(fontSize)
        let system = CTFontCreateUIFontForLanguage(.system, size, nil) ?? CTFontCreateWithName("Helvetica" as CFString, size, nil)
        let font = CTFontCreateCopyWithAttributes(system, size, nil,
            CTFontDescriptorCreateWithAttributes([kCTFontTraitsAttribute: [kCTFontWeightTrait: 0.23]] as CFDictionary))
        let text = NSAttributedString(string: marker, attributes: [NSAttributedString.Key(kCTFontAttributeName as String): font])
        return Float(ceil(CTLineGetTypographicBounds(CTLineCreateWithAttributedString(text), nil, nil, nil)) + size * 0.3)
    }
}

struct ReadingFrame: Sendable {
    let raster: ReaderRaster
    let info: PageInfo
    let viewport: Viewport

    init(_ page: MobilePage, viewport: Viewport, retaining previous: ReadingFrame?) throws {
        if let previous, previous.raster.id == page.rasterId() {
            raster = previous.raster
        } else {
            raster = try ReaderRaster(page)
        }
        info = page.info()
        self.viewport = viewport
    }
}

struct ReaderPreview: Identifiable {
    var id = UUID()
    let text: String
    let raster: ReaderRaster?
    var document: TouchAction? = nil
    var documentID: String? = nil
    var pages = 1
    var contentHeight: CGFloat = 56
    var firstBaseline: Float? = nil
    var controls: [ReadingControl] = []
    var isImage = false
    var isFormula = false
    var regions: [TextRegion] = []
}

@MainActor
final class ReaderModel: ObservableObject {
    @Published private(set) var frame: ReadingFrame?
    @Published private(set) var working = false
    @Published private(set) var openFailed = false
    @Published private(set) var focusAvailable = false
    @Published private(set) var selection: SelectedText?
    @Published private(set) var toc: [TocItem] = []
    enum SearchState {
        case idle, searching, results([SearchHit]), failed
    }
    @Published private(set) var searchState: SearchState = .idle
    private var searchRevision: UInt64 = 0
    private var pendingSearch: (String, UInt64)?
    @Published private(set) var notes: [Annotation] = []
    @Published var preview: ReaderPreview?
    @Published var error: String?
    @Published var resumeToken: String?
    @Published var externalURL: URL?
    @Published var finished = false
    @Published private(set) var bookLanguages: [String] = []
    @Published private(set) var selectionMode = "free"
    @Published private(set) var translated = false
    @Published private(set) var aiNotice: String?
    @Published private(set) var aiNoticeRevision = 0
    @Published private(set) var documentRevision: UInt64 = 0
    @Published private(set) var showingChrome = false
    @Published private(set) var turnPending = false
    @Published private(set) var neighbours = Neighbours()
    @Published private(set) var editingNote: Annotation?
    @Published private(set) var documentMedia: ReaderPreview?
    private var previewTask: Task<Void, Never>?
    private var previewGeneration: UInt64 = 0
    private var previewedFrame: UInt64?
    private var turnSerial: UInt64 = 0
    private var queuedTurn: (Bool, Bool, Bool)?
    private var pendingSelection: SelectionInput?
    private var selectionTask: Task<Void, Never>?
    private var selectionEpoch: UInt64 = 0
    private var handleStart: Bool?
    private var documentInteraction = false

    struct Neighbours {
        var previous: ReadingFrame?
        var next: ReadingFrame?
    }
    private enum SelectionInput {
        case range(CGPoint, CGPoint)
        case handle(Bool, CGPoint, Float)
    }
    @Published private(set) var turnMotion: TurnMotion?
    @Published private(set) var previewedNote: Annotation?
    @Published private(set) var chatMessages: [ChatMessage] = []
    @Published private(set) var chatChanges: [ChatChange] = []
    @Published private(set) var chatStream: ChatStream?
    @Published var chatError: String?
    @Published var chatDraft = ""
    @Published private(set) var markedFinished = false

    private var reader: MobileReader?
    private var operation: Task<Void, Never>?
    private var desiredViewport: Viewport?
    private var closing = false
    private var trimPending = false
    private var checkResumePending = false
    private var paperDark = false
    private var initialFont: Float = 20
    private var prefersFocus = false
    private var unified = false
    private var aiEnabled = false
    private var active = false
    private var obscured = false
    private var aiDirty = false
    private var aiDemand: Task<Void, Never>?
    private var aiSink: ReaderUpdateSink?
    private var aiDelivery: Task<Void, Never>?
    private var aiEpoch: UInt64 = 0
    private var aiVersion: UInt64 = 0
    private var aiDocumentRevision: UInt64 = 0
    private var lastAIError: UInt64?
    private var pendingOptions: ReadingOptions?
    private var reloadPending = false
    private var chatRequest: ChatRequest?
    private var chatTask: Task<Void, Never>?
    private var pendingDocumentClose: String?
    private var knownDocumentID: String?
    private var lastPreview: TouchAction?
    private var noteIndent: Float = 0
    private var documentViewport: Viewport?
    var documentWidth: CGFloat { CGFloat(documentViewport?.width ?? 100) }
    private var preparedReferences = false
    private var referencePreparation: Task<Void, Never>?
    private var refreshReferencesPending = false
    var feedbackReader: MobileReader? { reader }

    struct TurnMotion: Equatable {
        var serial: UInt64
        var changedUnit: Bool
        var changed: Bool
        var forward: Bool
        var horizontal: Bool
    }

    struct ChatStream {
        let id: String
        let question: String
        var content = ""
        var reasoning = ""
        var activity = ""
    }
    private let core: MobileCore
    private let book: BookSummary

    init(core: MobileCore, book: BookSummary) {
        self.core = core
        self.book = book
    }

    func updateViewport(size: CGSize, scale: CGFloat, dark: Bool, font: Float, focus: Bool, unified: Bool) {
        guard size.width.isFinite, size.height.isFinite, size.width >= 100, size.height >= 100 else { return }
        let width = UInt32(min(size.width, 4096))
        let height = UInt32(min(size.height, 4096))
        // Bound the visible raster even on a Retina iPad. The shared bridge
        // applies a separate memory budget to any offscreen scroll strip.
        let rasterScale = min(scale, sqrt(4_000_000 / (CGFloat(width) * CGFloat(height))))
        desiredViewport = Viewport(width: width, height: height, scale: Float(max(1, rasterScale)))
        let appearanceChanged = paperDark != dark
        paperDark = dark
        initialFont = font
        prefersFocus = focus
        self.unified = unified
        if appearanceChanged, reader != nil { appearancePending = true }
        reconcile()
    }

    private var appearancePending = false

    private func reconcile() {
        guard !closing, operation == nil, chatTask == nil, selectionTask == nil, let viewport = desiredViewport else { return }
        if reader == nil, !openFailed {
            let dark = paperDark, font = initialFont, focus = prefersFocus, unified = unified
            run { [self] in
                let opened = try await core.openBook(id: book.id, viewport: viewport)
                do {
                    let available = try await opened.focusModeAvailable()
                    let language = try await opened.bookLanguages()
                    let autoLayout = (try? await core.aiOptions().layoutEnabled) ?? false
                    try await opened.setTranslationEnabled(enabled: false)
                    let configured = try await opened.configure(fontSize: font, unified: unified, dark: dark)
                    let page = focus && available ? try await opened.focusMode(enabled: true) : configured
                    let mode = Self.preferredSelection(focus && available, words: Self.supportsWords(language))
                    try await opened.selectionMode(mode: mode)
                    let rendered = try ReadingFrame(page, viewport: viewport, retaining: nil)
                    guard !closing else {
                        try await opened.shutdown()
                        return
                    }
                    reader = opened
                    focusAvailable = available
                    bookLanguages = language
                    selectionMode = mode
                    aiEnabled = autoLayout
                    appearancePending = dark != paperDark
                    frame = rendered
                    prepareReferences(opened)
                    checkResumePending = true
                } catch {
                    try? await opened.shutdown()
                    throw error
                }
            }
        } else if let reader, frame?.viewport != viewport {
            run { [self] in
                do { try show(try await reader.resize(viewport: viewport), viewport: viewport) }
                catch {
                    if desiredViewport == viewport { desiredViewport = frame?.viewport }
                    throw error
                }
            }
        } else if appearancePending {
            appearancePending = false
            changeFont(initialFont, dark: paperDark)
        } else if let id = pendingDocumentClose, let reader {
            pendingDocumentClose = nil
            run { try await reader.closeDocument(id: id) }
        } else if refreshReferencesPending, let reader, preview == nil, selection == nil {
            refreshReferencesPending = false
            run { [self] in try show(try await reader.refreshMediaReferences()) }
        } else if let options = pendingOptions, let reader {
            pendingOptions = nil
            run { [self] in try show(try await reader.applyReadingOptions(options: options)) }
        } else if reloadPending, let reader {
            reloadPending = false
            run { [self] in
                aiEnabled = try await core.aiOptions().layoutEnabled
                try show(try await reader.reloadAiOptions())
            }
        } else if trimPending, let reader {
            trimPending = false
            run { try await reader.trimMemory() }
        } else if checkResumePending, let reader {
            checkResumePending = false
            // Checking for remote progress does not change the current frame.
            run(refreshAI: false) { [self] in resumeToken = try await reader.cloudResumeOffer()?.token }
        } else if let (query, revision) = pendingSearch, let reader {
            pendingSearch = nil
            run(refreshAI: false) { [self] in
                do {
                    let hits = try await reader.search(query: query)
                    if !closing && searchRevision == revision { searchState = .results(hits) }
                } catch {
                    if !closing && searchRevision == revision { searchState = .failed }
                }
            }
        } else if let next = queuedTurn {
            queuedTurn = nil
            turn(next.0, wholeUnit: next.1, horizontal: next.2)
        } else {
            publishAiDemand()
            prepareNeighbours()
        }
    }

    private func invalidateNeighbours() {
        previewGeneration &+= 1
        previewTask?.cancel()
        previewedFrame = nil
        if neighbours.previous != nil || neighbours.next != nil { neighbours = Neighbours() }
    }

    private func prepareNeighbours() {
        guard active, !obscured, !finished, preview == nil, selection == nil,
              let reader, let frame, previewTask == nil else { return }
        let id = frame.raster.id
        guard previewedFrame != id else { return }
        let v = frame.viewport
        let bytes = ceil(Double(v.width) * Double(v.scale)) * ceil(Double(v.height) * Double(v.scale)) * 4
        guard bytes <= 12 * 1024 * 1024 else { return }
        previewedFrame = id
        let generation = previewGeneration
        previewTask = Task { [weak self] in
            defer {
                self?.previewTask = nil
                if self?.previewGeneration != generation { self?.reconcile() }
            }
            do { try await Task.sleep(for: .milliseconds(140)) } catch { return }
            for forward in [true, false] {
                guard let self, !Task.isCancelled, self.previewGeneration == generation,
                      self.active, !self.obscured, self.operation == nil else {
                    if self?.previewGeneration == generation { self?.previewedFrame = nil }
                    break
                }
                if let page = try? await reader.previewTurn(forward: forward),
                   !Task.isCancelled, self.previewGeneration == generation,
                   self.frame?.raster.id == id,
                   let rendered = try? ReadingFrame(page, viewport: v, retaining: nil) {
                    if forward { self.neighbours.next = rendered }
                    else { self.neighbours.previous = rendered }
                }
                await Task.yield()
            }
        }
    }

    func retry() {
        error = nil
        openFailed = false
        reconcile()
    }

    private func run(refreshAI: Bool = true, _ work: @escaping @MainActor () async throws -> Void) {
        guard !closing, operation == nil, chatTask == nil else { return }
        if refreshAI { invalidateAiDemand() }
        if previewTask != nil { previewedFrame = nil; previewTask?.cancel() }
        working = true
        operation = Task { [weak self] in
            guard let self else { return }
            do {
                await previewTask?.value
                await selectionTask?.value
                if refreshAI {
                    await aiDemand?.value
                    if !active { reader?.stopAi() }
                }
                try await work()
            } catch {
                if !closing {
                    openFailed = frame == nil
                    self.error = message(error)
                }
            }
            operation = nil
            working = false
            if !openFailed { reconcile() }
        }
    }

    private func show(_ page: MobilePage, viewport: Viewport? = nil) throws {
        guard !closing, let viewport = viewport ?? frame?.viewport else { return }
        let rendered = try ReadingFrame(page, viewport: viewport, retaining: frame)
        if frame?.raster.id != rendered.raster.id || frame?.info != rendered.info { invalidateNeighbours() }
        frame = rendered
        selection = nil
    }

    func turn(_ forward: Bool, wholeUnit: Bool, horizontal: Bool = true) {
        guard let reader, chatTask == nil, !closing else { return }
        if finished {
            if !forward { finished = false; reconcile() }
            return
        }
        if turnPending { queuedTurn = (forward, wholeUnit, horizontal); return }
        guard operation == nil, selectionTask == nil else { return }
        turnPending = true
        showingChrome = false
        let before = frame
        run { [self] in
            defer { turnPending = false }
            do {
                let result = try await reader.turnWithResult(forward: forward, wholeUnit: wholeUnit)
                let info = result.page.info()
                turnSerial &+= 1
                turnMotion = TurnMotion(serial: turnSerial,
                    changedUnit: before?.info.scrollContext != info.scrollContext || before?.info.activeTocId != info.activeTocId,
                    changed: !result.reachedEnd && (before?.info != info || before?.raster.id != result.page.rasterId()),
                    forward: forward, horizontal: horizontal)
                try show(result.page)
                try await reader.readingActivity(eligible: true, activity: true)
                if result.reachedEnd { finished = true; queuedTurn = nil }
            } catch {
                queuedTurn = nil
                throw error
            }
        }
    }

    func completionStatistics() async throws -> (BookStatistics, UInt32) {
        if let reader { try await reader.readingActivity(eligible: false, activity: false) }
        async let detail = core.bookStatistics(id: book.id)
        async let all = core.statisticsPeriod(kind: "all", offset: 0)
        return try await (detail, all.finished)
    }

    func markFinished() {
        guard let reader else { return }
        run { [self] in
            try await reader.markFinished()
            markedFinished = true
        }
    }

    func changeMode(_ focus: Bool) {
        guard let reader, focusAvailable else { return }
        run { [self] in
            let page = try await reader.focusMode(enabled: focus)
            let mode = Self.preferredSelection(focus, words: wordSelectionAvailable)
            try await reader.selectionMode(mode: translated ? "paragraph" : mode)
            selectionMode = mode
            try show(page)
        }
    }

    func changeFont(_ size: Float, dark: Bool, unified: Bool? = nil) {
        initialFont = size
        paperDark = dark
        if let unified { self.unified = unified }
        guard let reader else { return }
        if operation != nil { appearancePending = true; return }
        run { [self] in
            try show(try await reader.configure(fontSize: size, unified: self.unified, dark: dark))
        }
    }

    func applyFonts() {
        guard let reader else { return }
        run { [self] in
            var options = try await core.readingOptions()
            options.translated = translated
            try show(try await reader.applyReadingOptions(options: options))
        }
    }

    func applyFonts(_ value: ReadingOptions) {
        var options = value
        options.translated = translated
        pendingOptions = options
        reconcile()
    }

    func reloadAI() {
        reloadPending = true
        reconcile()
    }

    private func prepareReferences(_ reader: MobileReader) {
        guard !preparedReferences else { return }
        preparedReferences = true
        referencePreparation = Task { [weak self] in
            let changed = (try? await reader.prepareMediaReferences()) ?? false
            guard let self, !self.closing, changed, !Task.isCancelled else { return }
            self.refreshReferencesPending = true
            self.reconcile()
        }
    }

    func toggleChrome() { showingChrome.toggle() }

    func beginNotePreview(_ note: Annotation) {
        guard let reader else { return }
        run { [self] in
            try show(try await reader.goToSource(source: note.source))
            previewedNote = note
        }
    }

    func closeNotePreview() { previewedNote = nil }

    func stepNote(_ forward: Bool) {
        guard let previewedNote, let index = notes.firstIndex(where: { $0.id == previewedNote.id }) else { return }
        let next = index + (forward ? 1 : -1)
        if notes.indices.contains(next) { beginNotePreview(notes[next]) }
    }

    var wordSelectionAvailable: Bool { Self.supportsWords(bookLanguages) }

    func chooseSelection(_ mode: String) {
        guard let reader, !translated, mode != "word" || wordSelectionAvailable else { return }
        let key = frame?.info.isScroll == true ? "reader.focusSelection" : "reader.classicSelection"
        run { [self] in
            try await reader.selectionMode(mode: mode)
            UserDefaults.standard.set(mode, forKey: key)
            selectionMode = mode
            selection = nil
        }
    }

    private static func preferredSelection(_ focus: Bool, words: Bool) -> String {
        let fallback = focus ? "paragraph" : "free"
        let mode = UserDefaults.standard.string(forKey: focus ? "reader.focusSelection" : "reader.classicSelection") ?? fallback
        return mode == "word" && !words ? fallback : mode
    }

    private static func supportsWords(_ languages: [String]) -> Bool {
        for tag in languages {
            guard let code = languageCode(tag) else { continue }
            let identifier = tag.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "_", with: "-")
            let script = Locale.Language(identifier: identifier).script ??
                Locale.Language(identifier: code + "-" + identifier.split(separator: "-").dropFirst().joined(separator: "-")).script
            if let value = script?.identifier, !["Zzzz", "Zyyy", "Zinh", "Zxxx"].contains(value) {
                return value == "Latn"
            }
        }
        return true
    }

    var offersTranslation: Bool {
        guard !translated, !bookLanguages.isEmpty, let system = Locale.preferredLanguages.first,
              let code = Self.languageCode(system) else { return true }
        return bookLanguages.contains { Self.languageCode($0) != code }
    }

    private static func languageCode(_ identifier: String) -> String? {
        let code = Locale(identifier: identifier.replacingOccurrences(of: "_", with: "-")).language.languageCode?.identifier
        let aliases = ["chi": "zh", "zho": "zh", "eng": "en", "jpn": "ja", "kor": "ko", "fra": "fr", "fre": "fr",
                       "deu": "de", "ger": "de", "spa": "es", "ita": "it", "por": "pt", "rus": "ru"]
        guard let code, !["und", "mul", "zxx"].contains(code) else { return nil }
        return aliases[code] ?? code
    }

    func toggleTranslation() {
        guard let reader else { return }
        run { [self] in
            let enabled = !translated
            try await reader.setTranslationEnabled(enabled: enabled)
            try await reader.selectionMode(mode: enabled ? "paragraph" : selectionMode)
            translated = enabled
            lastAIError = nil
            aiNotice = nil
            try show(try await reader.page())
        }
    }

    func setAiVisibility(active: Bool, obscured: Bool) {
        guard active != self.active || obscured != self.obscured else { return }
        self.active = active
        self.obscured = obscured
        if !active { stopChat(); reader?.stopAi(); invalidateNeighbours() }
        invalidateAiDemand()
        reconcile()
    }

    private func invalidateAiDemand() {
        aiEpoch &+= 1
        aiDirty = true
        if aiEnabled || translated || aiDemand != nil {
            reader?.suspendAi()
        }
    }

    private func publishAiDemand() {
        guard aiDirty, aiDemand == nil, !closing, let reader, frame != nil else { return }
        aiDirty = false
        guard active, aiEnabled || translated else {
            if aiSink != nil {
                reader.stopAi()
                aiSink = nil
                aiDelivery?.cancel()
                aiDelivery = nil
            }
            return
        }
        let activity = ReaderAiActivity(active: true,
            interacting: obscured || selection != nil || documentInteraction || finished || previewedNote != nil,
            manualLayout: false, uiEpoch: aiEpoch)
        let sink: ReaderUpdateSink
        if let current = aiSink {
            sink = current
        } else {
            let (stream, continuation) = AsyncStream<ReaderAiUpdate>.makeStream()
            sink = ReaderUpdateSink(continuation)
            aiSink = sink
            aiDelivery = Task { [weak self] in
                for await event in stream { self?.receiveAI(event) }
            }
        }
        aiDemand = Task {
            do { try await reader.updateAiDemand(activity: activity, listener: sink) }
            catch {
                if activity.uiEpoch == aiEpoch && !closing && !activity.interacting {
                    showAiError(translated ? "translation" : "layout")
                }
            }
            if !active || closing { reader.stopAi() }
            aiDemand = nil
            if aiDirty { reconcile() }
        }
    }

    private func receiveAI(_ update: ReaderAiUpdate) {
        guard !closing, active, update.uiEpoch == aiEpoch, update.version > aiVersion else { return }
        aiVersion = update.version
        if let toc = update.toc { self.toc = toc }
        if let error = update.error, lastAIError != update.errorSerial, !error.isEmpty {
            lastAIError = update.errorSerial
            showAiError(update.errorKind)
        }
        if aiDocumentRevision != update.documentRevision {
            aiDocumentRevision = update.documentRevision
            if preview?.document != nil { documentRevision &+= 1 }
        }
        if let page = update.page, !working, !obscured, selection == nil, preview == nil, !finished {
            do { try show(page) } catch { self.error = message(error) }
        }
    }

    private func showAiError(_ kind: String) {
        aiNotice = kind == "translation" ? "translation" : "layout"
        aiNoticeRevision &+= 1
    }

    func clearAiNotice() { aiNotice = nil }

    func sendChat(_ raw: String) {
        let question = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !question.isEmpty, !closing, operation == nil, chatTask == nil, let reader else { return }
        invalidateAiDemand()
        let request = ChatRequest(), id = request.id()
        chatRequest = request
        chatDraft = ""
        chatError = nil
        chatStream = ChatStream(id: id, question: question)
        let sink = AssistantUpdateSink { [weak self] content, reasoning, activity in
            guard let self, self.chatStream?.id == id else { return }
            self.chatStream = ChatStream(id: id, question: question, content: content, reasoning: reasoning, activity: activity)
        }
        chatTask = Task {
            defer {
                chatStream = nil; chatRequest = nil; chatTask = nil
                reconcile()
            }
            do {
                let result = try await reader.chat(question: question, request: request, listener: sink)
                if !closing { chatMessages = result.messages; chatChanges = result.changes }
            } catch {
                if !closing, !message(error).contains("已停止") {
                    chatDraft = question
                    chatError = message(error)
                }
            }
        }
    }

    func stopChat() { chatRequest?.cancel() }

    func deleteChat(_ id: String?) {
        guard let reader else { return }
        run(refreshAI: false) { [self] in
            if let id { chatMessages = try await reader.deleteChatTurn(id: id) }
            else { try await reader.clearChat(); chatMessages = []; chatChanges = [] }
        }
    }

    func sources(_ locators: [String]) async -> [ChatSource] {
        guard let reader, !closing, !locators.isEmpty else { return [] }
        return (try? await reader.chatSources(locators: locators)) ?? []
    }

    func followCitation(_ locator: String) {
        guard let reader else { return }
        run { [self] in try show(try await reader.followChatCitation(locator: locator)) }
    }

    func resolveChatChanges(_ accept: Bool) {
        guard let reader else { return }
        run { [self] in
            try show(try await reader.resolveChatChanges(accept: accept))
            chatChanges = []
        }
    }

    func undoChatRewrites() {
        guard let reader, chatChanges.isEmpty else { return }
        run { [self] in try show(try await reader.undoRewrites()) }
    }

    func setSentenceSplit(_ split: Bool) {
        guard let control = frame?.info.controls.first(where: { $0.kind == "sentences" }), control.active != split else { return }
        toggleControl(control)
    }

    func toggleControl(_ control: ReadingControl) {
        guard let reader else { return }
        run { [self] in try show(try await reader.toggleReadingControl(control: control)) }
    }

    func toggleReferenceControl(_ control: ReadingControl) {
        guard let reader, let id = knownDocumentID, let action = lastPreview, let frame else { return }
        run { [self] in
            try show(try await reader.toggleDocumentControl(id: id, control: control))
            let page = try await renderDocument(action, index: 0, reader: reader, frame: frame, token: documentToken)
            guard knownDocumentID == id, var preview else { return }
            // A visible slice refreshes without dismissing and reopening its sheet.
            preview.controls = page.controls
            preview.contentHeight = CGFloat(page.contentHeight)
            self.preview = preview
            documentRevision &+= 1
        }
    }

    func loadToc() {
        guard let reader else { return }
        run { [self] in toc = try await reader.toc() }
    }

    func search(_ query: String) {
        let value = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard reader != nil, !value.isEmpty else { return }
        searchRevision &+= 1
        pendingSearch = (value, searchRevision)
        searchState = .searching
        reconcile()
    }

    func clearSearch() {
        searchRevision &+= 1
        pendingSearch = nil
        searchState = .idle
    }

    func loadNotes() {
        guard let reader else { return }
        run { [self] in notes = try await reader.annotations() }
    }

    func goToToc(_ id: String) {
        guard let reader else { return }
        run { [self] in try show(try await reader.navigateToc(id: id)) }
    }

    func goToSource(_ source: String) {
        guard let reader else { return }
        run { [self] in try show(try await reader.goToSource(source: source)) }
    }

    func select(from start: CGPoint, to end: CGPoint) {
        guard previewedNote == nil else { return }
        enqueueSelection(.range(start, end))
    }

    func moveHandle(start: Bool, to point: CGPoint, scroll: Float) {
        enqueueSelection(.handle(start, point, scroll))
    }

    func beginHandle() { handleStart = nil }

    private func enqueueSelection(_ input: SelectionInput) {
        guard let reader, !closing, operation == nil else { return }
        // A finger can deliver events faster than native hit testing. Keep the
        // newest endpoint while the current request is finishing.
        if case let .handle(start, point, delta) = input,
           case let .handle(_, _, previous) = pendingSelection {
            pendingSelection = .handle(start, point, min(48, max(-48, delta + previous)))
        } else { pendingSelection = input }
        guard selectionTask == nil else { return }
        let epoch = selectionEpoch
        invalidateAiDemand()
        if previewTask != nil { previewedFrame = nil; previewTask?.cancel() }
        selectionTask = Task {
            await previewTask?.value
            await aiDemand?.value
            while epoch == selectionEpoch, !closing, let next = pendingSelection {
                pendingSelection = nil
                do {
                    let result: SelectedText?
                    switch next {
                    case let .range(start, end):
                        result = try await reader.select(x: Float(start.x), y: Float(start.y),
                                                         endX: Float(end.x), endY: Float(end.y))
                    case let .handle(start, point, delta):
                        if handleStart != start {
                            try await reader.beginSelectionHandle(start: start)
                            handleStart = start
                        }
                        if delta != 0, let current = frame, current.info.isScroll {
                            let page = try await reader.scrollBy(delta: delta)
                            if epoch == selectionEpoch, !closing {
                                let next = try ReadingFrame(page, viewport: current.viewport, retaining: current)
                                if next.raster.id != current.raster.id { invalidateNeighbours() }
                                frame = next
                            }
                        }
                        result = try await reader.extendSelection(x: Float(point.x), y: Float(point.y))
                    }
                    if epoch == selectionEpoch, !closing, selection != result { selection = result }
                } catch {
                    if epoch == selectionEpoch, !closing { self.error = message(error) }
                }
            }
            selectionTask = nil
            reconcile()
        }
    }

    func clearSelection() {
        selectionEpoch &+= 1
        pendingSelection = nil
        handleStart = nil
        selection = nil
        guard let reader else { return }
        run { try await reader.clearSelection() }
    }

    func stopEditingNote() { editingNote = nil }

    func writeNote(_ text: String, saved: @escaping () -> Void) {
        guard let reader, editingNote != nil || selection != nil else { return }
        let existing = editingNote
        run { [self] in
            if let existing { notes = try await reader.editAnnotation(id: existing.id, note: text) }
            else { notes = try await reader.saveAnnotation(note: text) }
            try show(try await reader.page())
            editingNote = nil
            saved()
        }
    }

    func toggleHighlight() {
        guard let reader, let selection else { return }
        run { [self] in
            notes = try await reader.toggleSelectionHighlight(remove: selection.highlighted)
            try show(try await reader.page())
        }
    }

    func activate(_ point: CGPoint) {
        if selection != nil { clearSelection(); return }
        guard let reader, let frame else { return }
        run { [self] in
            let action = try await reader.activate(x: Float(point.x), y: Float(point.y))
            switch action.kind {
            case "focus", "page":
                if action.kind == "page" { showingChrome = false }
                if let page = action.media { try show(page) }
            case "image", "formula":
                if let page = action.media {
                    preview = ReaderPreview(text: action.text, raster: try ReaderRaster(page), isImage: true, isFormula: action.kind == "formula")
                }
            case "external":
                if let url = URL(string: action.target), ["https", "http"].contains(url.scheme?.lowercased() ?? "") {
                    externalURL = url
                }
            case "reference", "note":
                lastPreview = action
                noteIndent = action.kind == "note" ? ReferenceSheetStyle.indent(marker: action.marker, fontSize: CGFloat(initialFont)) : 0
                let width = min(640, frame.viewport.width)
                let height = frame.viewport.height
                documentViewport = Viewport(width: max(100, width - min(width, UInt32(2 * ReferenceSheetStyle.inset))),
                                            height: max(100, min(UInt32(ReferenceSheetStyle.maxDocumentHeight), height - min(76, height))),
                                            scale: frame.viewport.scale)
                let page = try await renderDocument(action, index: 0, reader: reader, frame: frame, token: documentToken)
                knownDocumentID = page.id
                preview = ReaderPreview(text: action.text, raster: try ReaderRaster(page.page), document: action,
                                        documentID: page.id, pages: Int(page.total), contentHeight: CGFloat(page.contentHeight),
                                        firstBaseline: page.firstTextBaseline, controls: page.controls, regions: page.page.info().regions)
            case "annotation":
                if let note = try await reader.annotations().first(where: { $0.id == action.target }) {
                    if previewedNote == nil { editingNote = note }
                }
            default:
                guard previewedNote == nil else { return }
                let width = CGFloat(frame.viewport.width)
                if (!frame.info.isScroll || frame.info.focusCount == 0), point.x < width * 0.2 || point.x > width * 0.8 {
                    let forward = point.x > width * 0.8
                    queuedTurn = (forward, false, true)
                } else { toggleChrome() }
            }
        }
    }

    private func renderDocument(_ action: TouchAction, index: UInt32, reader: MobileReader, frame: ReadingFrame, token: UInt64) async throws -> DocumentPage {
        let ink = ReferenceSheetStyle.ink(dark: paperDark)
        let background = ReferenceSheetStyle.background(dark: paperDark)
        let viewport = documentViewport ?? frame.viewport
        if action.kind == "note" {
            return try await reader.notePage(markup: action.markup.isEmpty ? action.text : action.markup,
                token: token, target: action.target, marker: action.marker, firstLineIndent: noteIndent, index: index,
                viewport: viewport, foreground: ink, background: background)
        }
        return try await reader.referencePage(id: action.target, index: index, viewport: viewport,
                                               foreground: ink, background: background)
    }

    private var documentToken: UInt64 = 0

    struct DocumentRaster {
        let raster: ReaderRaster
        let total: Int
        let contentHeight: CGFloat
        let baseline: Float?
        let regions: [TextRegion]
    }

    func documentPage(_ index: Int) async throws -> DocumentRaster? {
        guard let action = lastPreview, let reader, let frame, !closing else { return nil }
        let token = documentToken
        let result = try await renderDocument(action, index: UInt32(index), reader: reader, frame: frame, token: token)
        guard documentToken == token, knownDocumentID != nil else {
            try? await reader.closeDocument(id: result.id)
            return nil
        }
        knownDocumentID = result.id
        guard !Task.isCancelled else { return nil }
        return DocumentRaster(raster: try ReaderRaster(result.page), total: Int(result.total), contentHeight: CGFloat(result.contentHeight),
                              baseline: result.firstTextBaseline, regions: result.page.info().regions)
    }

    func setDocumentInteraction(_ interacting: Bool) {
        guard documentInteraction != interacting else { return }
        documentInteraction = interacting
        invalidateAiDemand()
        reconcile()
    }

    func selectDocument(index: Int, start: CGPoint, end: CGPoint, handle: Bool?) async -> SelectedText? {
        guard let reader, let id = knownDocumentID, !closing else { return nil }
        let token = documentToken
        await aiDemand?.value
        guard token == documentToken, !closing else { return nil }
        let selected = try? await reader.documentSelect(id: id, index: UInt32(index),
            x: Float(start.x), y: Float(start.y), endX: Float(end.x), endY: Float(end.y), handle: handle)
        return token == documentToken && !closing ? selected : nil
    }

    func activateDocument(index: Int, point: CGPoint) {
        guard let reader, let id = knownDocumentID, let frame else { return }
        let token = documentToken
        run { [self] in
            let action = try await reader.documentActivate(id: id, index: UInt32(index), x: Float(point.x), y: Float(point.y))
            guard token == documentToken, !closing else { return }
            switch action.kind {
            case "link":
                closePreview()
                try show(try await reader.followLink(target: action.target))
            case "external":
                if let url = URL(string: action.target), ["https", "http"].contains(url.scheme?.lowercased() ?? "") {
                    externalURL = url
                }
            case "reference", "note":
                let sheet = preview?.id ?? UUID()
                documentToken &+= 1
                lastPreview = action
                noteIndent = action.kind == "note" ? ReferenceSheetStyle.indent(marker: action.marker, fontSize: CGFloat(initialFont)) : 0
                let page = try await renderDocument(action, index: 0, reader: reader, frame: frame, token: documentToken)
                knownDocumentID = page.id
                preview = ReaderPreview(id: sheet, text: action.text, raster: try ReaderRaster(page.page), document: action,
                    documentID: page.id, pages: Int(page.total), contentHeight: CGFloat(page.contentHeight), firstBaseline: page.firstTextBaseline,
                    controls: page.controls, regions: page.page.info().regions)
            case "image", "formula":
                if let page = action.media {
                    documentMedia = ReaderPreview(text: action.text, raster: try ReaderRaster(page),
                                                  isImage: true, isFormula: action.kind == "formula")
                }
            default: break
            }
        }
    }

    func closeDocumentMedia() { documentMedia = nil }

    func closePreview() {
        let open = preview != nil || knownDocumentID != nil
        preview = nil
        documentMedia = nil
        documentInteraction = false
        documentRevision = 0
        lastPreview = nil
        documentViewport = nil
        if let knownDocumentID {
            pendingDocumentClose = knownDocumentID
            self.knownDocumentID = nil
            documentToken &+= 1
        }
        if open { invalidateAiDemand(); reconcile() }
    }

    func checkRemoteProgress() {
        checkResumePending = true
        reconcile()
    }

    func resumeRemoteProgress() {
        guard let reader, let token = resumeToken else { return }
        run { [self] in
            if let page = try await reader.resumeCloudProgress(token: token) {
                try show(page)
                finished = false
            }
            resumeToken = nil
        }
    }

    func trimMemory() {
        invalidateNeighbours()
        if let frame { previewedFrame = frame.raster.id }
        trimPending = true
        reconcile()
    }

    func close() async {
        guard !closing else { return }
        closing = true
        // A cancelled Swift task may still have an in-flight Rust worker.
        // Wait for the command, then release its session and pixel storage.
        stopChat()
        invalidateNeighbours()
        await previewTask?.value
        selectionEpoch &+= 1
        await selectionTask?.value
        referencePreparation?.cancel()
        reader?.stopAi()
        await aiDemand?.value
        reader?.stopAi()
        aiSink = nil
        aiDelivery?.cancel()
        aiDelivery = nil
        await chatTask?.value
        await operation?.value
        if let reader { try? await reader.shutdown() }
        reader = nil
        frame = nil
        preview = nil
    }
}

extension ReaderModel {
    func recordActivity(eligible: Bool, interaction: Bool) async {
        guard !closing, !working, let reader else { return }
        try? await reader.readingActivity(eligible: eligible, activity: interaction)
    }
}


private final class ReaderUpdateSink: ReaderAiListener {
    let events: AsyncStream<ReaderAiUpdate>.Continuation
    init(_ events: AsyncStream<ReaderAiUpdate>.Continuation) { self.events = events }
    func update(update: ReaderAiUpdate) { events.yield(update) }
    deinit { events.finish() }
}

private final class AssistantUpdateSink: ChatListener {
    let deliver: @MainActor @Sendable (String, String, String) -> Void
    init(_ deliver: @escaping @MainActor @Sendable (String, String, String) -> Void) { self.deliver = deliver }
    func update(content: String, reasoning: String, activity: String) {
        Task { @MainActor in deliver(content, reasoning, activity) }
    }
}
