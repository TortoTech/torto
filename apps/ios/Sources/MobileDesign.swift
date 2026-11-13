import SwiftUI

func mobileText(_ chinese: String, _ english: String) -> String {
    let choice = UserDefaults.standard.string(forKey: "interface.language") ?? "system"
    let language = choice == "system" ? Locale.preferredLanguages.first ?? "en" : choice
    return language.lowercased().hasPrefix("zh") ? chinese : english
}

struct MobilePalette {
    let dark: Bool
    init(_ scheme: ColorScheme) { dark = scheme == .dark }
    var background: Color { hex(dark ? 0x121416 : 0xf4f5f7) }
    var surface: Color { hex(dark ? 0x1c1f23 : 0xffffff) }
    var subtle: Color { hex(dark ? 0x2a3037 : 0xebedf0) }
    var raised: Color { hex(dark ? 0x2a3037 : 0xe9ebef) }
    var cover: Color { hex(dark ? 0x343b44 : 0xe2e5e9) }
    var controlOutline: Color { hex(dark ? 0x626b77 : 0xa3a9b2) }
    var outline: Color { hex(dark ? 0x353c45 : 0xdee1e6) }
    var ink: Color { hex(dark ? 0xe5e8ec : 0x2b3039) }
    var secondary: Color { hex(dark ? 0x9ba2ac : 0x7b828d) }
    var icon: Color { hex(dark ? 0xa9afb8 : 0x707781) }
    var accent: Color { hex(dark ? 0x36da83 : 0x078a46) }
    var active: Color { hex(dark ? 0x36da83 : 0x07c160) }
    var selection: Color { hex(dark ? 0x263f31 : 0xdeece4) }
    var inverse: Color { hex(dark ? 0xe5e8ec : 0x2b3037) }
    var inverseInk: Color { hex(dark ? 0x252a31 : 0xf4f5f7) }

    static func hex(_ rgb: UInt32) -> Color {
        Color(red: Double((rgb >> 16) & 255) / 255, green: Double((rgb >> 8) & 255) / 255,
              blue: Double(rgb & 255) / 255)
    }

    private func hex(_ rgb: UInt32) -> Color { Self.hex(rgb) }
}

struct MobileIcon: View {
    let name: String
    var size: CGFloat = 28
    var tint: Color? = nil
    @Environment(\.colorScheme) private var scheme

    init(_ name: String, size: CGFloat = 28, tint: Color? = nil) {
        self.name = name
        self.size = size
        self.tint = tint
    }

    var body: some View {
        Image("symbol-\(name)").renderingMode(.template).resizable().scaledToFit()
            .frame(width: size, height: size)
            .foregroundStyle(tint ?? MobilePalette(scheme).icon)
            .accessibilityHidden(true)
    }
}

struct MobileIconButton: View {
    let icon: String
    let label: String
    var size: CGFloat = 28
    var selected = false
    var action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            MobileIcon(icon, size: size, tint: selected ? MobilePalette(scheme).active : nil)
                .frame(width: 48, height: 48).contentShape(Rectangle())
        }
        .buttonStyle(MobileInkButtonStyle())
        .accessibilityLabel(label)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

struct MobileInkButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.opacity(enabled ? (configuration.isPressed ? 0.64 : 1) : 0.38)
    }
}

struct MobileHeader<Trailing: View>: View {
    let title: String
    var subtitle: String? = nil
    var backIcon = "back"
    var background: Color? = nil
    var height: CGFloat = 64
    var inset: CGFloat = 4
    var backSize: CGFloat = 28
    var subtitleSize: CGFloat = 12
    let back: () -> Void
    @ViewBuilder var trailing: () -> Trailing
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = MobilePalette(scheme)
        HStack(spacing: 0) {
            MobileIconButton(icon: backIcon, label: mobileText("返回", "Back"), size: backSize, action: back)
            VStack(spacing: 2) {
                Text(title).font(.system(size: 16)).lineLimit(1)
                if let subtitle, !subtitle.isEmpty { Text(subtitle).font(.system(size: subtitleSize)).foregroundStyle(palette.secondary).lineLimit(1) }
            }
            .frame(maxWidth: .infinity)
            .accessibilityAddTraits(.isHeader)
            Color.clear.frame(width: 48, height: 48).overlay { trailing() }
        }
        .padding(.horizontal, inset)
        .frame(minHeight: height)
        .foregroundStyle(palette.ink)
        .background((background ?? palette.background).ignoresSafeArea(edges: .top))
    }
}

extension MobileHeader where Trailing == Color {
    init(title: String, subtitle: String? = nil, backIcon: String = "back",
         background: Color? = nil, height: CGFloat = 64, inset: CGFloat = 4, back: @escaping () -> Void) {
        self.init(title: title, subtitle: subtitle, backIcon: backIcon, background: background,
                  height: height, inset: inset, back: back, trailing: { Color.clear })
    }
}

struct MobileGroup<Content: View>: View {
    @ViewBuilder var content: () -> Content
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        VStack(spacing: 0, content: content)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(MobilePalette(scheme).surface, in: .rect(cornerRadius: 16))
    }
}

struct MobileRow: View {
    let title: String
    var value: String? = nil
    var icon: String? = nil
    var check: Bool? = nil
    var action: () -> Void
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        let palette = MobilePalette(scheme)
        Button(action: action) {
            HStack(spacing: 0) {
                if let icon { MobileIcon(icon, size: 24).padding(.trailing, 12) }
                SettingsRowLabels {
                    Text(title).font(.system(size: 16, weight: check == true ? .medium : .regular))
                        .foregroundStyle(check == true ? palette.accent : palette.ink)
                    if let value, !value.isEmpty {
                        Text(value).font(.system(size: 14)).foregroundStyle(palette.secondary)
                            .lineLimit(2).multilineTextAlignment(.trailing)
                    }
                }
                if check == true { MobileIcon("check", size: 20, tint: palette.accent).padding(.leading, 12) }
                else if check == nil { MobileIcon("next", size: 18).padding(.leading, 4) }
            }
            .padding(.horizontal, 16).padding(.vertical, 16)
            .frame(minHeight: 56)
            .contentShape(Rectangle())
        }
        .buttonStyle(MobileInkButtonStyle())
        .accessibilityAddTraits(check == true ? .isSelected : [])
    }
}

private struct SettingsRowLabels: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? subviews.reduce(CGFloat.zero) { $0 + $1.sizeThatFits(.unspecified).width + 12 }
        let (title, value) = widths(width, subviews)
        let height = max(subviews.first?.sizeThatFits(.init(width: title, height: nil)).height ?? 0,
                         subviews.dropFirst().first?.sizeThatFits(.init(width: value, height: nil)).height ?? 0)
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let (title, value) = widths(bounds.width, subviews)
        subviews.first?.place(at: CGPoint(x: bounds.minX, y: bounds.midY), anchor: .leading,
                              proposal: .init(width: title, height: nil))
        subviews.dropFirst().first?.place(at: CGPoint(x: bounds.maxX, y: bounds.midY), anchor: .trailing,
                                          proposal: .init(width: value, height: nil))
    }

    private func widths(_ width: CGFloat, _ views: Subviews) -> (CGFloat, CGFloat) {
        guard let value = views.dropFirst().first else { return (width, 0) }
        let right = min(width * 0.52, value.sizeThatFits(.unspecified).width)
        return (max(0, width - right - 12), right)
    }
}

struct MobileToggle: View {
    let title: String
    var description: String? = nil
    @Binding var value: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = MobilePalette(scheme)
        Toggle(isOn: $value) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.system(size: 16)).foregroundStyle(palette.ink)
                if let description { Text(description).font(.system(size: 14)).foregroundStyle(palette.secondary) }
            }
        }
        .toggleStyle(MobileSwitchStyle())
    }
}

private struct MobileSwitchStyle: ToggleStyle {
    @Environment(\.colorScheme) private var scheme
    func makeBody(configuration: Configuration) -> some View {
        let palette = MobilePalette(scheme)
        Button { configuration.isOn.toggle() } label: {
            HStack(spacing: 16) {
                configuration.label.frame(maxWidth: .infinity, alignment: .leading)
                Capsule().fill(configuration.isOn ? palette.active : palette.surface)
                    .overlay { Capsule().strokeBorder(configuration.isOn ? .clear : palette.outline, lineWidth: 2) }
                    .overlay(alignment: configuration.isOn ? .trailing : .leading) {
                        Circle().fill(configuration.isOn ? .white : palette.controlOutline)
                            .frame(width: configuration.isOn ? 24 : 16, height: configuration.isOn ? 24 : 16)
                            .padding(.horizontal, configuration.isOn ? 4 : 8)
                    }.frame(width: 52, height: 32).accessibilityHidden(true)
            }.padding(.horizontal, 16).padding(.vertical, 10)
                .frame(minHeight: 56).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle())
            .accessibilityAddTraits(.isToggle)
            .accessibilityValue(configuration.isOn ? "1" : "0")
    }
}

struct MobileFormField: View {
    let title: String
    @Binding var text: String
    var secure = false
    @FocusState private var focused: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let palette = MobilePalette(scheme)
        let floating = focused || !text.isEmpty
        VStack(alignment: .leading, spacing: 0) {
            Text(title).font(.system(size: 12)).lineLimit(2)
                .foregroundStyle(focused ? palette.accent : palette.secondary)
                .opacity(floating ? 1 : 0).accessibilityHidden(true)
            Group {
                if secure { SecureField(floating ? "" : title, text: $text) }
                else { TextField(floating ? "" : title, text: $text) }
            }.font(.system(size: 16)).focused($focused).accessibilityLabel(title)
                .offset(y: floating ? 0 : -6)
        }.padding(.horizontal, 16).frame(minHeight: 56)
            .background(palette.surface, in: .rect(cornerRadius: 5))
            .overlay { RoundedRectangle(cornerRadius: 5).strokeBorder(focused ? palette.accent : palette.controlOutline) }
            .contentShape(Rectangle()).onTapGesture { focused = true }
    }
}

struct MobileSearchField: View {
    let title: String
    @Binding var query: String
    @FocusState private var focused: Bool
    var autofocus = false
    var settings = false
    var dismissOnSubmit = false
    var submit: () -> Void = {}
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(spacing: 8) {
            MobileIcon("search", size: settings ? 28 : 24)
            TextField(title, text: $query).focused($focused)
                .font(.system(size: 16))
                .submitLabel(.search).onSubmit {
                    if dismissOnSubmit { focused = false }
                    submit()
                }
            if !query.isEmpty {
                MobileIconButton(icon: "close", label: mobileText("清除", "Clear"), size: 18) { query = "" }
                    .frame(width: settings ? 44 : 40, height: 44)
            }
        }
        .padding(.horizontal, 12).frame(minHeight: settings ? 56 : 44)
        .background(settings ? MobilePalette(scheme).surface : MobilePalette(scheme).raised,
                    in: .rect(cornerRadius: settings ? 16 : 20))
        .task { if autofocus { focused = true } }
    }
}

struct ReadingCapsuleSlider: View {
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double? = nil
    let label: String
    var fontSize = false
    var commit: (Double) -> Void
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        let palette = MobilePalette(scheme)
        HStack(spacing: 0) {
            if fontSize { endpoint(decrease: true) }
            else { MobileIcon("sun", size: 20).frame(width: 40).accessibilityHidden(true) }
            GeometryReader { geometry in
                let radius: CGFloat = 16
                let span = max(1, geometry.size.width - 2 * radius)
                let fraction = (value - range.lowerBound) / (range.upperBound - range.lowerBound)
                ZStack(alignment: .leading) {
                    Rectangle().fill(.clear).contentShape(Rectangle())
                    Circle().fill(.white)
                        .strokeBorder(.black.opacity(0.06), lineWidth: 1)
                        .shadow(color: .black.opacity(0.14), radius: 3, y: 2)
                        .frame(width: 32, height: 32)
                        .overlay {
                            if fontSize {
                                Text("\(Int(value))").font(.system(size: 14, weight: .medium))
                                    .foregroundStyle(MobilePalette.hex(0x252a32)).monospacedDigit()
                            }
                        }
                        .offset(x: span * min(1, max(0, fraction)))
                        .allowsHitTesting(false)
                }
                .gesture(DragGesture(minimumDistance: 0)
                    .onChanged { gesture in
                        guard enabled else { return }
                        let fraction = min(1, max(0, (gesture.location.x - radius) / span))
                        value = clamped(range.lowerBound + fraction * (range.upperBound - range.lowerBound))
                    }
                    .onEnded { _ in if enabled { commit(value) } })
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(label)
                .accessibilityIdentifier(fontSize ? "reading-font-size" : "reading-brightness")
                .accessibilityValue(fontSize ? "\(Int(value))" : value.formatted(.percent.precision(.fractionLength(0))))
                .accessibilityAdjustableAction { direction in
                    switch direction {
                    case .increment: adjust(step ?? 0.05)
                    case .decrement: adjust(-(step ?? 0.05))
                    @unknown default: break
                    }
                }
            }
            if fontSize { endpoint(decrease: false) } else { Spacer().frame(width: 4) }
        }
        .frame(height: 48)
        .background { Capsule().fill(scheme == .dark ? palette.raised : MobilePalette.hex(0xeceeee)).frame(height: 36) }
        .opacity(enabled ? 1 : 0.48)
    }

    private func endpoint(decrease: Bool) -> some View {
        Button { adjust(decrease ? -(step ?? 2) : (step ?? 2)) } label: {
            Text("A").font(.system(size: decrease ? 12 : 18))
                .foregroundStyle(MobilePalette(scheme).icon)
                .frame(width: 40, height: 48).contentShape(Rectangle())
        }
        .buttonStyle(MobileInkButtonStyle())
        .accessibilityLabel(decrease ? mobileText("减小字号", "Decrease font size") : mobileText("增大字号", "Increase font size"))
        .disabled(decrease ? value <= range.lowerBound : value >= range.upperBound)
    }

    private func clamped(_ value: Double) -> Double {
        let rounded = step.map { range.lowerBound + ((value - range.lowerBound) / $0).rounded() * $0 } ?? value
        return min(range.upperBound, max(range.lowerBound, rounded))
    }

    private func adjust(_ delta: Double) {
        guard enabled else { return }
        value = clamped(value + delta)
        commit(value)
    }
}

func mobileDuration(_ milliseconds: UInt64) -> String {
    let minutes = milliseconds / 60_000
    if milliseconds > 0 && minutes == 0 { return mobileText("不到 1 分钟", "<1 min") }
    if minutes < 60 { return mobileText("\(minutes) 分钟", "\(minutes) min") }
    return mobileText("\(minutes / 60) 小时 \(minutes % 60) 分钟", "\(minutes / 60) h \(minutes % 60) min")
}
