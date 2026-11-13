import SwiftUI
import UIKit

struct ReaderTypographyPanel: View {
    let size: Int
    let canFocus: Bool
    let focused: Bool
    let change: (Int) -> Void
    let fonts: () -> Void
    let layout: () -> Void
    let mode: () -> Void
    @Environment(\.colorScheme) private var scheme
    @State private var pending = 20.0

    var body: some View {
        VStack(spacing: 16) {
            ReadingCapsuleSlider(value: $pending, range: 16...40, step: 2, label: mobileText("字号", "Font size"),
                                 fontSize: true) { if Int($0) != size { change(Int($0)) } }
            HStack(spacing: 10) {
                pill(mobileText("字体样式", "Font styles"), fonts)
                pill(mobileText("AI 排版", "AI layout"), layout)
                if canFocus { pill(focused ? mobileText("专注模式", "Focus mode") : mobileText("经典模式", "Classic mode"), mode) }
            }
        }.padding(.horizontal, 20).padding(.vertical, 16)
            .onChange(of: size, initial: true) { _, value in pending = Double(min(40, max(16, value / 2 * 2))) }
    }

    private func pill(_ title: String, _ action: @escaping () -> Void) -> some View {
        let palette = MobilePalette(scheme)
        return Button(action: action) {
            HStack(spacing: 2) {
                Text(title).font(.system(size: 14)).lineLimit(1).minimumScaleFactor(0.8)
                MobileIcon("next", size: 12)
            }
            .padding(.horizontal, 8).frame(maxWidth: .infinity, minHeight: 36)
            .background(scheme == .dark ? palette.subtle : MobilePalette.hex(0xf7f8f8), in: .capsule)
            .frame(height: 48).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle())
    }
}

struct NoteComposer: View {
    let quote: String
    let submit: (String) -> Void
    let saving: Bool
    @State private var draft: String
    @FocusState private var editing: Bool
    @Environment(\.colorScheme) private var scheme

    init(quote: String, initial: String = "", saving: Bool = false, submit: @escaping (String) -> Void) {
        self.quote = quote
        self.saving = saving
        self.submit = submit
        _draft = State(initialValue: initial)
    }

    var body: some View {
        let palette = MobilePalette(scheme)
        let value = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .bottom, spacing: 10) {
                TextField(mobileText("写下这一刻的想法…", "Write a note…"), text: $draft, axis: .vertical)
                    .focused($editing).lineLimit(1...4).disabled(saving)
                    .accessibilityIdentifier("note-content")
                    .padding(.horizontal, 14).padding(.vertical, 11)
                    .background(palette.surface, in: .rect(cornerRadius: 22))
                Button { submit(value) } label: {
                    MobileIcon("up", size: 24, tint: .white).frame(width: 42, height: 42)
                        .background(palette.active, in: .circle)
                }.buttonStyle(MobileInkButtonStyle()).disabled(value.isEmpty || saving)
                    .accessibilityLabel(mobileText("保存笔记", "Save note"))
            }
            if !quote.isEmpty {
                Text(mobileText("引用：", "Quote: ") + quote)
                    .lineLimit(1).font(.system(size: 13)).foregroundStyle(palette.secondary)
            }
        }.padding(16).task { editing = true }
    }
}

struct ReaderNoteCard: View {
    let note: Annotation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = MobilePalette(scheme)
        let thought = note.note.flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 }
        let text = thought ?? note.quote
        let icon = Image(thought == nil ? "symbol-highlight" : "symbol-note").renderingMode(.template)
        VStack(alignment: .leading, spacing: 12) {
            Text("\(Text(icon).foregroundStyle(palette.icon).baselineOffset(-3))\u{202F}\(Text(verbatim: text))")
                .font(.system(size: 16)).lineLimit(thought == nil ? 4 : nil)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityLabel(text)
            if thought != nil {
                Text(note.quote).font(.system(size: 14)).foregroundStyle(palette.secondary).lineLimit(4)
                    .padding(.leading, 10)
                    .overlay(alignment: .leading) { Rectangle().fill(palette.outline).frame(width: 2) }
            }
        }.padding(16).contentShape(Rectangle())
    }
}

struct ReaderReferenceSheet: View {
    let initialPreview: ReaderPreview
    @ObservedObject var model: ReaderModel
    let fontSize: Int
    let availableHeight: CGFloat
    @Environment(\.colorScheme) private var scheme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var metrics: DocumentMetrics?
    @State private var noticeVisible = false
    @State private var selectedPage: Int?
    @State private var atTop = true
    @State private var startedAtTop: Bool?
    @State private var dragOffset: CGFloat = 0
    private var preview: ReaderPreview { model.preview ?? initialPreview }
    private struct DocumentMetrics {
        let id: String?
        let pages: Int
        let height: CGFloat
    }
    // Keep the measured height while a translation revision is rendering.
    // Falling back to the original height during that brief gap makes the sheet jump.
    private var currentMetrics: DocumentMetrics? { metrics.flatMap { $0.id == preview.documentID ? $0 : nil } }
    private var currentContentHeight: CGFloat { currentMetrics?.height ?? preview.contentHeight }
    private var controlHeight: CGFloat { preview.controls.isEmpty ? 0 : ReferenceSheetStyle.controlHeight }
    private var scrollHeight: CGFloat {
        min(ReferenceSheetStyle.maxDocumentHeight, max(56, availableHeight - 48), max(56, currentContentHeight + controlHeight))
    }

    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(spacing: 0) {
            Button(action: model.closePreview) {
                Capsule().fill(palette.secondary.opacity(0.4)).frame(width: 32, height: 4)
                    .frame(maxWidth: .infinity).frame(height: 36).contentShape(Rectangle())
            }.buttonStyle(.plain)
                .simultaneousGesture(dismissDrag(fromHandle: true))
                .accessibilityLabel(mobileText("关闭脚注预览", "Close reference preview"))
                .accessibilityIdentifier("reference-drag-handle")
            ScrollView {
                LazyVStack(spacing: 0) {
                    if !preview.controls.isEmpty {
                        ReaderControls(controls: preview.controls, action: model.toggleReferenceControl).disabled(model.working)
                    }
                    if let document = preview.document {
                        ForEach(0..<(currentMetrics?.pages ?? preview.pages), id: \.self) { index in
                            DocumentSlice(index: index, initial: index == 0 ? preview.raster : nil,
                                          baseline: preview.firstBaseline, initialRegions: index == 0 ? preview.regions : [],
                                          marker: index == 0 && document.kind == "note" ? document.marker : "", fontSize: fontSize, model: model,
                                          selectedPage: $selectedPage) { result in
                                if index == 0 { metrics = DocumentMetrics(id: preview.documentID, pages: result.total, height: result.contentHeight) }
                            }
                        }
                    } else {
                        Text(preview.text).font(.system(size: CGFloat(fontSize))).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }.id(preview.documentID)
                    .frame(maxWidth: model.documentWidth)
                    .padding(.horizontal, ReferenceSheetStyle.inset)
                    .frame(maxWidth: .infinity)
            }
            .frame(height: scrollHeight)
            .scrollBounceBehavior(.basedOnSize)
            .onScrollGeometryChange(for: Bool.self) { $0.contentOffset.y + $0.contentInsets.top <= 1 } action: { _, top in atTop = top }
            .simultaneousGesture(dismissDrag())
        }
            .padding(.bottom, 12)
            .background(alignment: .top) {
                UnevenRoundedRectangle(topLeadingRadius: 28, topTrailingRadius: 28)
                    .fill(palette.background).ignoresSafeArea(edges: .bottom)
            }
            .offset(y: dragOffset)
            .accessibilityAddTraits(.isModal)
            .accessibilityAction(.escape, model.closePreview)
            .accessibilityIdentifier("reference-preview")
            .onChange(of: preview.documentID) { _, _ in metrics = nil; selectedPage = nil }
            .onChange(of: selectedPage) { _, page in
                if page == nil { model.setDocumentInteraction(false) }
            }
            .onDisappear { model.setDocumentInteraction(false) }
            .fullScreenCover(item: Binding(get: { model.documentMedia }, set: { if $0 == nil { model.closeDocumentMedia() } })) { media in
                if let raster = media.raster {
                    if media.isFormula { FormulaPreview(raster: raster, source: media.text, close: model.closeDocumentMedia) }
                    else { FullImagePreview(raster: raster, close: model.closeDocumentMedia) }
                }
            }
            .overlay(alignment: .top) {
                if noticeVisible, model.aiNotice == "translation" {
                    Text(mobileText("翻译服务错误", "Translation service error"))
                        .font(.system(size: 13)).foregroundStyle(palette.inverseInk).padding(8)
                        .background(palette.inverse, in: .capsule)
                }
            }
            .task(id: model.aiNoticeRevision) {
                guard model.aiNotice == "translation" else { return }
                noticeVisible = true
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
                noticeVisible = false
            }
    }

    private func dismissDrag(fromHandle: Bool = false) -> some Gesture {
        DragGesture(minimumDistance: 12).onChanged { value in
            if startedAtTop == nil { startedAtTop = fromHandle || (atTop && selectedPage == nil) }
            guard startedAtTop == true, value.translation.height > abs(value.translation.width) else { return }
            dragOffset = max(0, value.translation.height)
        }.onEnded { value in
            startedAtTop = nil
            if dragOffset > 80 || (dragOffset > 24 && value.predictedEndTranslation.height > 145) {
                model.closePreview()
            } else {
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) { dragOffset = 0 }
            }
        }
    }
}

private struct DocumentSlice: View {
    let index: Int
    let initial: ReaderRaster?
    let baseline: Float?
    let initialRegions: [TextRegion]
    let marker: String
    let fontSize: Int
    @ObservedObject var model: ReaderModel
    @Binding var selectedPage: Int?
    let didLoad: (ReaderModel.DocumentRaster) -> Void
    @State private var value: ReaderModel.DocumentRaster?
    @State private var selection: SelectedText?
    @State private var dragging = false
    @State private var pending: DragInput?
    @State private var selectionTask: Task<Void, Never>?
    @State private var epoch = 0
    @Environment(\.colorScheme) private var scheme

    private struct DragInput {
        let start: CGPoint
        let end: CGPoint
        let handle: Bool?
    }

    var body: some View {
        Group {
            if let raster = value?.raster ?? initial {
                DocumentTouchSurface(raster: raster, logicalWidth: model.documentWidth, regions: value?.regions ?? initialRegions,
                    selection: selectedPage == index ? selection : nil,
                    tap: { point in
                        if selectedPage != nil { clearSelection(); selectedPage = nil }
                        else { model.activateDocument(index: index, point: point) }
                    }, drag: { start, end, handle in request(DragInput(start: start, end: end, handle: handle)) },
                    moving: { dragging = $0; updateInteraction() })
                    .aspectRatio(CGFloat(raster.image.width) / CGFloat(raster.image.height), contentMode: .fit)
                    .frame(maxWidth: .infinity)
                    .overlay(alignment: .topLeading) {
                        if !marker.isEmpty, let y = value?.baseline ?? baseline {
                            let size = ReferenceSheetStyle.markerSize(CGFloat(fontSize))
                            Text(marker).font(.system(size: size, weight: .medium))
                                .foregroundStyle(MobilePalette.hex(referenceColor(dark: scheme == .dark)))
                                .alignmentGuide(.top) { $0[.firstTextBaseline] - max($0[.firstTextBaseline], CGFloat(y)) }
                                .padding(.leading, 2)
                                .allowsHitTesting(false)
                        }
                    }
                    .overlay(alignment: .topLeading) {
                        if let selection, selectedPage == index, !dragging, selectionTask == nil {
                            GeometryReader { surface in
                                if let first = selection.bounds.first {
                                    let scale = surface.size.width / model.documentWidth
                                    let x = min(surface.size.width - 39, max(39, CGFloat(first.x + first.width / 2) * scale))
                                    Button {
                                        UIPasteboard.general.string = selection.text
                                        clearSelection()
                                    } label: {
                                        VStack(spacing: 5) {
                                            MobileIcon("copy", size: 24, tint: .white)
                                            Text(mobileText("复制", "Copy")).font(.system(size: 12)).foregroundStyle(.white)
                                        }.frame(width: 68, height: 64).padding(.horizontal, 6)
                                            .background(MobilePalette.hex(0x3d3d3d), in: .rect(cornerRadius: 18))
                                    }.buttonStyle(MobileInkButtonStyle())
                                        .position(x: x, y: copyPosition(selection, in: surface.size.height, scale: scale))
                                }
                            }
                        }
                    }
            } else { Color.clear.frame(height: 100) }
        }
        .task(id: model.documentRevision) {
            guard initial == nil || model.documentRevision > 0 else { return }
            let revision = model.documentRevision
            if let updated = try? await model.documentPage(index), !Task.isCancelled, revision == model.documentRevision {
                value = updated
                didLoad(updated)
            }
        }
        .onChange(of: selectedPage) { _, page in
            if page != index, selection != nil { clearSelection() }
        }
        .onDisappear {
            value = nil
            if selectedPage == index || dragging { clearSelection() }
        }
    }

    private func copyPosition(_ selection: SelectedText, in height: CGFloat, scale: CGFloat) -> CGFloat {
        guard let first = selection.bounds.first, let last = selection.bounds.last else { return 32 }
        let above = CGFloat(first.y) * scale - 12 - 32
        let below = CGFloat(last.y + last.height) * scale + 12 + 32
        if above >= 32 { return above }
        if below + 32 <= height { return below }
        return max(32, height - 32)
    }

    private func request(_ input: DragInput) {
        selectedPage = index
        pending = input
        guard selectionTask == nil else { return }
        let generation = epoch
        selectionTask = Task {
            while generation == epoch, !Task.isCancelled, let latest = pending {
                pending = nil
                let result = await model.selectDocument(index: index, start: latest.start, end: latest.end, handle: latest.handle)
                if generation == epoch, !Task.isCancelled, selectedPage == index { selection = result }
            }
            selectionTask = nil
            if generation == epoch { updateInteraction() }
            else if selectedPage == index, let pending { request(pending) }
        }
        updateInteraction()
    }

    private func updateInteraction() {
        if selectedPage == index || dragging {
            model.setDocumentInteraction(dragging || selection != nil || selectionTask != nil)
        }
    }

    private func clearSelection() {
        epoch += 1
        pending = nil
        selection = nil
        dragging = false
        if selectedPage == index {
            selectedPage = nil
            model.setDocumentInteraction(false)
        }
    }
}

private struct DocumentTouchSurface: UIViewRepresentable {
    let raster: ReaderRaster
    let logicalWidth: CGFloat
    let regions: [TextRegion]
    let selection: SelectedText?
    let tap: (CGPoint) -> Void
    let drag: (CGPoint, CGPoint, Bool?) -> Void
    let moving: (Bool) -> Void

    func makeUIView(context: Context) -> Surface { Surface() }
    func updateUIView(_ view: Surface, context: Context) { view.update(self) }

    final class Surface: UIView, UIGestureRecognizerDelegate {
        private var input: DocumentTouchSurface?
        private var origin = CGPoint.zero
        private var handle: Bool?
        private var selecting = false

        init() {
            super.init(frame: .zero)
            let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
            let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:)))
            let pan = UIPanGestureRecognizer(target: self, action: #selector(handleMoved(_:)))
            for gesture in [tap, press, pan] { gesture.delegate = self; addGestureRecognizer(gesture) }
            tap.require(toFail: press)
            isAccessibilityElement = false
        }
        required init?(coder: NSCoder) { fatalError("Use init()") }

        func update(_ value: DocumentTouchSurface) {
            if input?.raster.id != value.raster.id || input?.regions != value.regions { setNeedsLayout() }
            if input?.raster.id != value.raster.id || input?.selection != value.selection { setNeedsDisplay() }
            input = value
        }

        private var logicalScale: CGFloat {
            guard let input else { return 1 }
            return max(0.001, bounds.width / input.logicalWidth)
        }

        private func point(_ gesture: UIGestureRecognizer) -> CGPoint {
            let p = gesture.location(in: self)
            return CGPoint(x: p.x / logicalScale, y: p.y / logicalScale)
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard let input else { return }
            accessibilityElements = input.regions.compactMap { region in
                let box = region.bounds.map(rectangle).reduce(CGRect.null) { $0.union($1) }
                guard !box.isNull, !region.text.isEmpty else { return nil }
                let element = UIAccessibilityElement(accessibilityContainer: self)
                element.accessibilityLabel = region.text
                element.accessibilityTraits = .staticText
                element.accessibilityFrameInContainerSpace = box.applying(CGAffineTransform(scaleX: logicalScale, y: logicalScale))
                return element
            }
        }

        override func draw(_ rect: CGRect) {
            guard let input, let context = UIGraphicsGetCurrentContext() else { return }
            UIImage(cgImage: input.raster.image).draw(in: bounds)
            guard let selection = input.selection else { return }
            context.scaleBy(x: logicalScale, y: logicalScale)
            context.setFillColor(UIColor.systemGreen.withAlphaComponent(0.25).cgColor)
            for bounds in selection.bounds { context.fill(rectangle(bounds)) }
            context.setFillColor(UIColor.systemGreen.cgColor)
            for p in endpoints {
                context.fillEllipse(in: CGRect(x: p.x - 5, y: p.y - 3, width: 10, height: 10))
            }
        }

        private var endpoints: [CGPoint] {
            guard let first = input?.selection?.bounds.first, let last = input?.selection?.bounds.last else { return [] }
            return [CGPoint(x: CGFloat(first.x), y: CGFloat(first.y + first.height)),
                    CGPoint(x: CGFloat(last.x + last.width), y: CGFloat(last.y + last.height))]
        }

        private func handleAt(_ p: CGPoint) -> Bool? {
            for (i, location) in endpoints.enumerated() where hypot(location.x - p.x, location.y - p.y) <= 20 { return i == 0 }
            return nil
        }

        override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard let pan = gestureRecognizer as? UIPanGestureRecognizer else { return true }
            let p = point(pan)
            let translation = pan.translation(in: self)
            handle = handleAt(CGPoint(x: p.x - translation.x / logicalScale, y: p.y - translation.y / logicalScale))
            return handle != nil
        }

        @objc private func tapped(_ recognizer: UITapGestureRecognizer) { input?.tap(point(recognizer)) }

        @objc private func pressed(_ recognizer: UILongPressGestureRecognizer) {
            guard let input else { return }
            switch recognizer.state {
            case .began:
                selecting = true
                origin = point(recognizer)
                input.moving(true)
                input.drag(origin, origin, nil)
            case .changed where selecting: input.drag(origin, point(recognizer), nil)
            case .ended, .cancelled, .failed:
                if selecting && recognizer.state == .ended { input.drag(origin, point(recognizer), nil) }
                selecting = false
                input.moving(false)
            default: break
            }
        }

        @objc private func handleMoved(_ recognizer: UIPanGestureRecognizer) {
            guard let input, let handle else { return }
            if recognizer.state == .began { input.moving(true) }
            if [.began, .changed, .ended].contains(recognizer.state) {
                let p = point(recognizer)
                input.drag(p, p, handle)
            }
            if [.ended, .cancelled, .failed].contains(recognizer.state) {
                self.handle = nil
                input.moving(false)
            }
        }

        private func rectangle(_ rect: Rect) -> CGRect {
            CGRect(x: CGFloat(rect.x), y: CGFloat(rect.y), width: CGFloat(rect.width), height: CGFloat(rect.height))
        }
    }
}

struct ReaderControls: View {
    let controls: [ReadingControl]
    let action: (ReadingControl) -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ScrollView(.horizontal) {
            HStack {
                ForEach(controls, id: \.self) { control in
                    Button(control.kind == "formula" ?
                           (control.active ? mobileText("收起公式备注", "Hide formula notes") : mobileText("公式备注", "Formula notes")) :
                           (control.active ? mobileText("显示表格", "Show table") : mobileText("显示图表", "Show chart"))) { action(control) }
                        .font(.system(size: 14)).padding(.horizontal, 12)
                        .frame(minHeight: ReferenceSheetStyle.controlHeight)
                }
            }.frame(minHeight: ReferenceSheetStyle.controlHeight)
        }.background(MobilePalette(scheme).background)
    }
}

struct FormulaPreview: View {
    let raster: ReaderRaster
    let source: String
    let close: () -> Void
    @State private var showingSource = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(spacing: 8) {
            MobileHeader(title: mobileText("公式预览", "Formula preview"), backIcon: "close", back: close)
            ImageZoomView(image: raster.image, background: UIColor(palette.background), tapOutside: false, close: close)
            if !source.isEmpty {
                Button(mobileText("公式源代码", "Formula source")) { showingSource.toggle() }
                    .padding(8)
                if showingSource {
                    ScrollView {
                        Text(source).font(.system(size: 14, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(16)
                    }.frame(maxHeight: 160)
                }
            }
        }.background(palette.background.ignoresSafeArea())
            .accessibilityAction(.escape, close)
    }
}

struct FullImagePreview: View {
    let image: CGImage
    let close: () -> Void

    init(image: CGImage, close: @escaping () -> Void) {
        self.image = image
        self.close = close
    }

    init(raster: ReaderRaster, close: @escaping () -> Void) {
        self.init(image: raster.image, close: close)
    }

    var body: some View {
        ImageZoomView(image: image, close: close)
            .background(.black).ignoresSafeArea()
            .accessibilityAddTraits(.isModal)
            .accessibilityIdentifier("image-preview")
            .accessibilityAction(.escape, close)
    }
}

private struct ImageZoomView: UIViewRepresentable {
    let image: CGImage
    var background: UIColor = .black
    var tapOutside = true
    let close: () -> Void

    func makeUIView(context: Context) -> ZoomView { ZoomView(image, background: background, tapOutside: tapOutside, close: close) }
    func updateUIView(_ view: ZoomView, context: Context) { view.close = close }

    final class ZoomView: UIScrollView, UIScrollViewDelegate {
        private let picture: UIImageView
        private var lastSize: CGSize = .zero
        var close: () -> Void
        let tapOutside: Bool

        init(_ image: CGImage, background: UIColor, tapOutside: Bool, close: @escaping () -> Void) {
            self.tapOutside = tapOutside
            picture = UIImageView(image: UIImage(cgImage: image))
            self.close = close
            super.init(frame: .zero)
            backgroundColor = background
            contentInsetAdjustmentBehavior = .never
            showsVerticalScrollIndicator = false
            showsHorizontalScrollIndicator = false
            minimumZoomScale = 1
            maximumZoomScale = 8
            delegate = self
            addSubview(picture)
            let tap = UITapGestureRecognizer(target: self, action: #selector(tapped))
            let doubleTap = UITapGestureRecognizer(target: self, action: #selector(zoomTapped(_:)))
            doubleTap.numberOfTapsRequired = 2
            tap.require(toFail: doubleTap)
            addGestureRecognizer(doubleTap)
            if tapOutside { addGestureRecognizer(tap) }
            isAccessibilityElement = true
            accessibilityLabel = mobileText("图片预览", "Image preview")
            accessibilityHint = mobileText("双击缩放，轻点空白关闭", "Double tap to zoom; tap outside the image to dismiss")
            accessibilityTraits = [.image, .button]
        }
        required init?(coder: NSCoder) { fatalError("Use init(_:close:)") }
        override func accessibilityActivate() -> Bool { close(); return true }
        @objc private func tapped(_ gesture: UITapGestureRecognizer) {
            if !picture.bounds.contains(gesture.location(in: picture)) { close() }
        }

        @objc private func zoomTapped(_ gesture: UITapGestureRecognizer) {
            if zoomScale > 1.01 { setZoomScale(1, animated: true); return }
            let point = gesture.location(in: picture)
            let width = bounds.width / 2.5, height = bounds.height / 2.5
            zoom(to: CGRect(x: point.x - width / 2, y: point.y - height / 2, width: width, height: height), animated: true)
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            if bounds.size != lastSize, let image = picture.image, bounds.width > 0, bounds.height > 0 {
                lastSize = bounds.size
                setZoomScale(1, animated: false)
                let scale = min(bounds.width / image.size.width, bounds.height / image.size.height)
                picture.frame = CGRect(origin: .zero, size: CGSize(width: image.size.width * scale, height: image.size.height * scale))
                contentSize = picture.frame.size
            }
            centerImage()
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { picture }
        func scrollViewDidZoom(_ scrollView: UIScrollView) { centerImage() }

        private func centerImage() {
            let x = max(0, (bounds.width - contentSize.width) / 2)
            let y = max(0, (bounds.height - contentSize.height) / 2)
            if contentInset.top != y || contentInset.left != x {
                contentInset = UIEdgeInsets(top: y, left: x, bottom: y, right: x)
            }
        }
    }
}
