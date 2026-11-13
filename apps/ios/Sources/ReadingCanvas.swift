import SwiftUI
import UIKit

struct ReadingCanvas: UIViewRepresentable {
    let frame: ReadingFrame
    let selection: SelectedText?
    let enabled: Bool
    let turn: (Bool, Bool, Bool) -> Void
    let activate: (CGPoint) -> Void
    let select: (CGPoint, CGPoint) -> Void
    let splitSentences: (Bool) -> Void
    let moving: (Bool) -> Void
    let turnMotion: ReaderModel.TurnMotion?
    let turnPending: Bool
    let neighbours: ReaderModel.Neighbours
    let beginHandle: () -> Void
    let moveHandle: (Bool, CGPoint, Float) -> Void

    func makeUIView(context: Context) -> Canvas { Canvas() }
    func updateUIView(_ view: Canvas, context: Context) {
        view.update(self)
    }

    final class Canvas: UIView, UIGestureRecognizerDelegate {
        private var input: ReadingCanvas?
        private var panOffset = CGPoint.zero
        private var selectionStart = CGPoint.zero
        private var axis: Axis?
        private var dragActive = false
        private var handle: Bool?
        private var pointer = CGPoint.zero
        private var edgeTimer: Timer?
        private var pendingOffset: CGPoint?
        private var animation: Transition?
        private var ticker: CADisplayLink?
        private var phase: CGFloat = 0
        private var lastReportedMotion = false

        private enum Axis { case horizontal, vertical }
        private struct Transition {
            let frame: ReadingFrame?
            let offset: CGPoint
            let forward: Bool
            let horizontal: Bool
            let crossfade: Bool
            let start: CFTimeInterval
            let duration: Double
        }
        private var accessibilityRows: [UIAccessibilityElement] = []

        init() {
            super.init(frame: .zero)
            isOpaque = true
            clipsToBounds = true
            contentMode = .redraw
            isAccessibilityElement = false
            let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
            let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:)))
            let pan = UIPanGestureRecognizer(target: self, action: #selector(panned(_:)))
            let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinched(_:)))
            for gesture in [tap, press, pan, pinch] {
                gesture.delegate = self
                addGestureRecognizer(gesture)
            }
            tap.require(toFail: press)
            pan.maximumNumberOfTouches = 1
        }
        required init?(coder: NSCoder) { fatalError("Use init()") }

        func update(_ value: ReadingCanvas) {
            let previous = input
            let changed = previous?.frame.info != value.frame.info ||
                          previous?.frame.raster.id != value.frame.raster.id ||
                          previous?.frame.viewport != value.frame.viewport
            input = value
            if let previous, let motion = value.turnMotion, motion != previous.turnMotion {
                let offset = pendingOffset ?? .zero
                pendingOffset = nil
                if motion.changed, !UIAccessibility.isReduceMotionEnabled {
                    let displacement = CGFloat(value.frame.info.scrollDistance - previous.frame.info.scrollDistance)
                    let crossfade = !motion.horizontal && motion.changedUnit
                    let start = motion.horizontal ? offset :
                        crossfade ? offset : CGPoint(x: 0, y: offset.y + displacement)
                    startAnimation(from: start, old: motion.horizontal || crossfade ? previous.frame : nil,
                                   forward: motion.forward, horizontal: motion.horizontal, crossfade: crossfade)
                } else { resetOffset(animated: !motion.changed) }
            } else if previous?.turnPending == true && !value.turnPending, pendingOffset != nil {
                pendingOffset = nil
                resetOffset(animated: true)
            } else if changed && !dragActive && handle == nil && pendingOffset == nil && animation == nil {
                panOffset = .zero
            }
            if changed { setNeedsLayout() }
            if changed || previous?.selection != value.selection || pendingOffset != nil || dragActive {
                setNeedsDisplay()
            }
        }

        private func startAnimation(from offset: CGPoint, old: ReadingFrame?, forward: Bool,
                                    horizontal: Bool, crossfade: Bool) {
            guard window != nil, !UIAccessibility.isReduceMotionEnabled else {
                stopAnimation()
                return
            }
            ticker?.invalidate()
            let width = CGFloat(input?.frame.viewport.width ?? 1)
            let remaining = horizontal ? 1 - min(1, abs(offset.x) / width) : 1
            let duration = horizontal ? max(0.045, 0.15 * Double(remaining)) :
                crossfade ? 0.15 : min(0.26, max(0.09, Double(abs(offset.y)) / 850))
            panOffset = .zero
            phase = 0
            animation = Transition(frame: old, offset: offset, forward: forward, horizontal: horizontal,
                                   crossfade: crossfade, start: CACurrentMediaTime(), duration: duration)
            let link = CADisplayLink(target: self, selector: #selector(animate(_:)))
            link.add(to: .main, forMode: .common)
            ticker = link
            setNeedsDisplay()
        }

        @objc private func animate(_ link: CADisplayLink) {
            guard let animation else { stopAnimation(); return }
            let t = max(0, min(1, (link.timestamp - animation.start) / animation.duration))
            phase = CGFloat(1 - pow(1 - t, 3))
            if t >= 1 { stopAnimation() }
            else { setNeedsDisplay() }
        }

        private func stopAnimation() {
            ticker?.invalidate()
            ticker = nil
            animation = nil
            phase = 0
            panOffset = .zero
            setNeedsDisplay()
        }

        private func resetOffset(animated: Bool) {
            if animated, panOffset != .zero {
                startAnimation(from: panOffset, old: nil, forward: false, horizontal: false, crossfade: false)
            } else { stopAnimation() }
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { stopAnimation(); stopEdgeScroll() }
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard let input else { return }
            let viewport = input.frame.viewport
            let sx = bounds.width / CGFloat(viewport.width)
            let sy = bounds.height / CGFloat(viewport.height)
            accessibilityRows = input.frame.info.regions.compactMap { region in
                let rect = region.bounds.map(rectangle).reduce(CGRect.null) { $0.union($1) }
                    .applying(CGAffineTransform(scaleX: sx, y: sy)).intersection(bounds)
                guard !rect.isEmpty, !rect.isNull, !region.text.isEmpty else { return nil }
                let element = UIAccessibilityElement(accessibilityContainer: self)
                element.accessibilityLabel = region.text
                element.accessibilityTraits = .staticText
                element.accessibilityFrameInContainerSpace = rect
                return element
            }
            accessibilityElements = accessibilityRows
        }

        override func accessibilityScroll(_ direction: UIAccessibilityScrollDirection) -> Bool {
            guard let input, input.enabled else { return false }
            switch direction {
            case .down: input.turn(true, !input.frame.info.isScroll, false)
            case .up: input.turn(false, !input.frame.info.isScroll, false)
            case .left, .next: input.turn(true, true, true)
            case .right, .previous: input.turn(false, true, true)
            @unknown default: return false
            }
            UIAccessibility.post(notification: .pageScrolled, argument: nil)
            return true
        }

        override func draw(_ rect: CGRect) {
            guard let input, let ctx = UIGraphicsGetCurrentContext() else { return }
            let frame = input.frame
            let width = CGFloat(frame.viewport.width), height = CGFloat(frame.viewport.height)
            ctx.scaleBy(x: bounds.width / width, y: bounds.height / height)
            let viewport = CGRect(x: 0, y: 0, width: width, height: height)
            ctx.setFillColor(Self.color(frame.info.focusFadeColor | 0xff000000).cgColor)
            ctx.fill(viewport)

            if let transition = animation {
                let sign: CGFloat = transition.forward ? -1 : 1
                if let old = transition.frame, transition.horizontal {
                    let x = transition.offset.x + (sign * width - transition.offset.x) * phase
                    paint(old, offset: CGPoint(x: x, y: 0), in: ctx)
                    paint(frame, offset: CGPoint(x: x - sign * width, y: 0), in: ctx)
                } else if let old = transition.frame, transition.crossfade {
                    let offset = transition.offset.y
                    paint(frame, offset: CGPoint(x: 0, y: -sign * 24 * (1 - phase)), in: ctx)
                    ctx.saveGState()
                    ctx.setAlpha(1 - phase)
                    paint(old, offset: CGPoint(x: 0, y: offset + sign * 24 * phase), in: ctx)
                    ctx.restoreGState()
                } else {
                    let offset = CGPoint(x: transition.offset.x * (1 - phase), y: transition.offset.y * (1 - phase))
                    paint(frame, offset: offset, in: ctx)
                }
                return
            }
            let x = panOffset.x
            if abs(x) > 0.01, let neighbour = x < 0 ? input.neighbours.next : input.neighbours.previous {
                paint(neighbour, offset: CGPoint(x: x + (x < 0 ? width : -width), y: 0), in: ctx)
            }
            paint(frame, offset: panOffset, selection: input.selection, in: ctx)
        }

        private func paint(_ frame: ReadingFrame, offset: CGPoint, selection: SelectedText? = nil, in ctx: CGContext) {
            let info = frame.info
            let width = CGFloat(frame.viewport.width), height = CGFloat(frame.viewport.height)
            let viewport = CGRect(x: 0, y: 0, width: width, height: height)
            ctx.saveGState()
            // Each page owns its screen-width column. In focus mode the strip
            // continues above and below the viewport while it moves vertically.
            ctx.clip(to: viewport.offsetBy(dx: offset.x, dy: 0))
            ctx.translateBy(x: offset.x, y: offset.y)
            UIImage(cgImage: frame.raster.image).draw(in: CGRect(x: 0, y: CGFloat(info.rasterTop),
                                                               width: width, height: CGFloat(info.rasterHeight)))
            ctx.setFillColor(Self.color(0x55d8b86a).cgColor)
            for mark in info.marks { ctx.fill(rectangle(mark)) }
            if !info.focusBounds.isEmpty {
                ctx.saveGState()
                for area in info.focusBounds {
                    ctx.addRect(viewport.insetBy(dx: -width, dy: -height))
                    ctx.addRect(rectangle(area).insetBy(dx: -8, dy: -2))
                    ctx.clip(using: .evenOdd)
                }
                ctx.setFillColor(Self.color(info.focusFadeColor).cgColor)
                ctx.fill(viewport.insetBy(dx: -width, dy: -height))
                ctx.restoreGState()
            }
            if let selection {
                ctx.setFillColor(Self.color(0x55448967).cgColor)
                for area in selection.bounds { ctx.fill(rectangle(area)) }
                ctx.setFillColor(Self.color(0xff448967).cgColor)
                for point in handlePoints(selection) {
                    ctx.fillEllipse(in: CGRect(x: point.x - 6, y: point.y - 4, width: 12, height: 12))
                }
            }
            ctx.restoreGState()
        }

        private func handlePoints(_ selection: SelectedText) -> [CGPoint] {
            guard let first = selection.bounds.first, let last = selection.bounds.last else { return [] }
            return [CGPoint(x: CGFloat(first.x), y: CGFloat(first.y + first.height)),
                    CGPoint(x: CGFloat(last.x + last.width), y: CGFloat(last.y + last.height))]
        }

        private func hitHandle(_ point: CGPoint) -> Bool? {
            guard let selection = input?.selection else { return nil }
            let points = handlePoints(selection)
            guard let index = points.indices.min(by: {
                hypot(points[$0].x - point.x, points[$0].y - point.y) <
                hypot(points[$1].x - point.x, points[$1].y - point.y)
            }), hypot(points[index].x - point.x, points[index].y - point.y) <= 22 else { return nil }
            return index == 0
        }

        private func reportMotion() {
            let motion = dragActive || handle != nil || selecting
            if motion != lastReportedMotion { lastReportedMotion = motion; input?.moving(motion) }
        }

        private var selecting = false

        override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard let input else { return false }
            return input.enabled || (input.turnPending && gestureRecognizer is UIPanGestureRecognizer)
        }
        private func location(_ gesture: UIGestureRecognizer) -> CGPoint {
            guard let viewport = input?.frame.viewport, bounds.width > 0, bounds.height > 0 else { return .zero }
            let point = gesture.location(in: self)
            return CGPoint(x: point.x * CGFloat(viewport.width) / bounds.width,
                           y: point.y * CGFloat(viewport.height) / bounds.height)
        }
        @objc private func tapped(_ gesture: UITapGestureRecognizer) {
            if input?.enabled == true { input?.activate(location(gesture)) }
        }
        @objc private func pressed(_ gesture: UILongPressGestureRecognizer) {
            guard let input else { return }
            switch gesture.state {
            case .began where input.enabled:
                stopAnimation()
                selecting = true
                selectionStart = location(gesture)
                reportMotion()
                input.select(selectionStart, selectionStart)
            case .changed where selecting:
                input.select(selectionStart, location(gesture))
            case .ended, .cancelled, .failed:
                if selecting, gesture.state == .ended { input.select(selectionStart, location(gesture)) }
                selecting = false
                reportMotion()
            default: break
            }
        }
        @objc private func pinched(_ gesture: UIPinchGestureRecognizer) {
            if gesture.state == .ended, input?.enabled == true,
               gesture.scale < 0.83 || gesture.scale > 1.2 { input?.splitSentences(gesture.scale > 1) }
        }
        @objc private func panned(_ gesture: UIPanGestureRecognizer) {
            guard let input, bounds.width > 0, bounds.height > 0 else { return }
            let motion = gesture.translation(in: self)
            let speed = gesture.velocity(in: self)
            let f = input.frame
            switch gesture.state {
            case .began:
                let current = location(gesture)
                let origin = CGPoint(x: current.x - motion.x * CGFloat(f.viewport.width) / bounds.width,
                                     y: current.y - motion.y * CGFloat(f.viewport.height) / bounds.height)
                if input.enabled, let hit = hitHandle(origin) {
                    handle = hit
                    input.beginHandle()
                    moveHandle(current)
                } else {
                    if !input.turnPending { stopAnimation() }
                    axis = nil
                    dragActive = true
                }
                reportMotion()
            case .changed:
                if handle != nil { moveHandle(location(gesture)); return }
                guard dragActive else { return }
                if axis == nil, hypot(motion.x, motion.y) > 5 {
                    axis = abs(motion.x) > abs(motion.y) ? .horizontal : .vertical
                }
                guard let axis, !input.turnPending else { return }
                if axis == .horizontal {
                    let width = CGFloat(f.viewport.width)
                    let dx = motion.x / bounds.width * width
                    let preview = dx < 0 ? input.neighbours.next : input.neighbours.previous
                    let offset = preview == nil ? CGFloat(pageTurnResistedOffset(distance: Float(dx), width: Float(width))) : dx
                    panOffset = CGPoint(x: min(width, max(-width, offset)), y: 0)
                } else if f.info.isScroll {
                    let dy = motion.y * CGFloat(f.viewport.height) / bounds.height
                    let low = min(0, CGFloat(f.viewport.height) - CGFloat(f.info.rasterTop + f.info.rasterHeight))
                    let high = max(0, -CGFloat(f.info.rasterTop))
                    let target = dy < 0 ? f.info.nextFocus : f.info.previousFocus
                    let cap = min(64, CGFloat(abs(target?.delta ?? 256)) * 0.25)
                    let stop = CGFloat(abs(target?.delta ?? 0))
                    let length = abs(dy)
                    let resisted = length <= stop ? length : stop + cap * (1 - exp(-(length - stop) / max(1, cap)))
                    panOffset = CGPoint(x: 0, y: min(high, max(low, dy < 0 ? -resisted : resisted)))
                }
                setNeedsDisplay()
            case .ended, .cancelled, .failed:
                if handle != nil {
                    if gesture.state == .ended { moveHandle(location(gesture)) }
                    handle = nil; stopEdgeScroll(); reportMotion()
                    return
                }
                let horizontal = (axis ?? (abs(motion.x) > abs(motion.y) ? .horizontal : .vertical)) == .horizontal
                axis = nil
                dragActive = false
                defer { reportMotion() }
                guard gesture.state == .ended, input.enabled || input.turnPending else {
                    if !input.turnPending { resetOffset(animated: true) }
                    return
                }
                guard horizontal || f.info.isScroll else { return }
                let forward = horizontal ? motion.x < 0 : motion.y < 0
                let target = forward ? f.info.nextFocus : f.info.previousFocus
                let alternate = forward ? f.info.previousFocus : f.info.nextFocus
                let verticalThreshold = (target ?? alternate).map { min(36, max(8, CGFloat(abs($0.delta)) * 0.5)) } ?? 16
                let commit = horizontal ? pageTurnCommit(distance: Float(motion.x), width: Float(bounds.width), velocity: Float(speed.x)) :
                    (abs(motion.y) >= verticalThreshold || abs(speed.y) >= 500) && (abs(speed.y) < 500 || (speed.y < 0) == forward)
                if commit {
                    if !input.turnPending { pendingOffset = panOffset }
                    input.turn(forward, horizontal && f.info.isScroll, horizontal)
                } else if !input.turnPending { resetOffset(animated: true) }
            default: break
            }
        }

        private func moveHandle(_ point: CGPoint) {
            guard let input, let handle else { return }
            pointer = point
            input.moveHandle(handle, point, 0)
            let edge = point.y < 56 || point.y > CGFloat(input.frame.viewport.height) - 56
            if edge && input.frame.info.isScroll, edgeTimer == nil {
                let timer = Timer(timeInterval: 0.04, target: self, selector: #selector(scrollEdge), userInfo: nil, repeats: true)
                RunLoop.main.add(timer, forMode: .common)
                edgeTimer = timer
            } else if !edge { stopEdgeScroll() }
        }

        @objc private func scrollEdge() {
            guard let input, let handle, window != nil else { stopEdgeScroll(); return }
            input.moveHandle(handle, pointer, pointer.y < 56 ? -12 : 12)
        }

        private func stopEdgeScroll() { edgeTimer?.invalidate(); edgeTimer = nil }

        private func rectangle(_ r: Rect) -> CGRect {
            CGRect(x: CGFloat(r.x), y: CGFloat(r.y), width: CGFloat(r.width), height: CGFloat(r.height))
        }
        private static func color(_ rgba: UInt32) -> UIColor {
            UIColor(red: CGFloat((rgba >> 16) & 255) / 255,
                    green: CGFloat((rgba >> 8) & 255) / 255, blue: CGFloat(rgba & 255) / 255,
                    alpha: CGFloat(rgba >> 24) / 255)
        }
    }
}
