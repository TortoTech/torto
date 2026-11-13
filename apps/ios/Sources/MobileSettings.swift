import SwiftUI
import UIKit

struct MobileSettings: View {
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @AppStorage("interface.theme") private var theme = "system"
    @AppStorage("interface.language") private var language = "system"
    @State private var routes: [Route] = []
    @State private var showCloud = false
    @State private var provider: Provider?
    @State private var aiSaveError: String?
    private var palette: MobilePalette { MobilePalette(scheme) }

    enum Route { case theme, language, typography, providers, provider, translation, assistant, layout, about }

    var body: some View {
        let route = routes.last
        VStack(spacing: 0) {
            MobileHeader(title: title(route), background: route == .about ? palette.surface : nil, back: back) {
                if route == .providers {
                    MobileIconButton(icon: "add", label: mobileText("添加提供商", "Add provider")) {
                        provider = Provider(id: "", kind: "custom", name: "", baseUrl: "", models: [], keySaved: false)
                        routes.append(.provider)
                    }
                }
            }
            switch route {
            case nil:
                SettingsScroll {
                    MobileGroup {
                        MobileRow(title: title(.theme), value: themeLabel, icon: "moon") { routes.append(.theme) }
                        MobileRow(title: title(.language), value: languageLabel(language), icon: "translate") { routes.append(.language) }
                        MobileRow(title: title(.typography), icon: "type") { routes.append(.typography) }
                    }
                    MobileGroup {
                        MobileRow(title: title(.providers), icon: "ai") { routes.append(.providers) }
                        MobileRow(title: title(.translation), icon: "translate") { routes.append(.translation) }
                        MobileRow(title: title(.assistant), icon: "chat") { routes.append(.assistant) }
                    }
                    MobileGroup {
                        MobileRow(title: mobileText("云同步", "Cloud sync"), icon: "cloud") { showCloud = true }
                        MobileRow(title: title(.about), icon: "info") { routes.append(.about) }
                    }
                }
            case .theme:
                InterfaceOptions(value: $theme, fallback: scheme == .dark ? "dark" : "light",
                                 description: mobileText("开启后，将跟随系统打开或关闭深色模式", "When enabled, dark mode follows your system settings"),
                                 choices: [MobileChoice("light", mobileText("浅色", "Light")), MobileChoice("dark", mobileText("深色", "Dark"))])
            case .language:
                InterfaceOptions(value: $language, fallback: (Locale.preferredLanguages.first ?? "en").hasPrefix("zh") ? "zh-CN" : "en",
                                 description: mobileText("开启后，将跟随系统设置界面语言", "When enabled, the interface language follows your system settings"),
                                 choices: [MobileChoice("zh-CN", "中文"), MobileChoice("en", "English")])
            case .typography:
                TypesettingSettings(core: core, library: library) { routes.append(.layout) }
            case .providers:
                ProviderList(core: core) { value in
                    provider = value
                    routes.append(.provider)
                }
            case .provider:
                if let provider { ProviderEditor(core: core, initial: provider, done: back).id(provider.id) }
            case .translation:
                aiSettings(.translation)
            case .assistant:
                aiSettings(.assistant)
            case .layout:
                aiSettings(.layout)
            case .about:
                AboutScreen(core: core)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .foregroundStyle(palette.ink)
        .background((route == .about ? palette.surface : palette.background).ignoresSafeArea())
        .fullScreenCover(isPresented: $showCloud) { CloudSheet(core: core, cloud: library.cloud) }
        .alert(mobileText("无法保存 AI 设置", "Unable to save AI settings"), isPresented: Binding(get: { aiSaveError != nil }, set: { if !$0 { aiSaveError = nil } })) {
            Button("OK", role: .cancel) { aiSaveError = nil }
        } message: { Text(aiSaveError ?? "") }
    }

    private func aiSettings(_ role: AiRole) -> some View {
        AiRoleSettings(core: core, role: role, onSaveError: { aiSaveError = $0 }).id(role)
    }

    private func back() {
        if routes.isEmpty { dismiss() } else { routes.removeLast() }
    }

    private var themeLabel: String {
        switch theme {
        case "dark": mobileText("已开启", "On")
        case "light": mobileText("未开启", "Off")
        default: mobileText("跟随系统", "Follow system")
        }
    }

    private func languageLabel(_ value: String) -> String {
        switch value {
        case "zh-CN": "中文"
        case "en": "English"
        default: mobileText("跟随系统", "Follow system")
        }
    }

    private func title(_ route: Route?) -> String {
        switch route {
        case nil: mobileText("设置", "Settings")
        case .theme: mobileText("深色模式", "Dark mode")
        case .language: mobileText("界面语言", "Interface language")
        case .typography: mobileText("排版", "Typesetting")
        case .providers: mobileText("AI 提供商", "AI providers")
        case .provider: provider?.id.isEmpty == true ? mobileText("添加 AI 提供商", "Add AI provider") : mobileText("编辑 AI 提供商", "Edit AI provider")
        case .translation: mobileText("翻译", "Translation")
        case .assistant: mobileText("阅读助手", "Reading assistant")
        case .layout: mobileText("AI 排版", "AI layout")
        case .about: mobileText("关于", "About")
        }
    }
}

private struct InterfaceOptions: View {
    @Binding var value: String
    let fallback: String
    let description: String
    let choices: [MobileChoice]

    var body: some View {
        SettingsScroll {
            MobileGroup {
                MobileToggle(title: mobileText("跟随系统", "Follow system"), description: description,
                             value: Binding(get: { value == "system" }, set: { value = $0 ? "system" : fallback }))
            }
            if value != "system" {
                SectionCaption(title: mobileText("手动选择", "Manual selection"))
                MobileGroup {
                    ForEach(choices) { option in
                        MobileRow(title: option.title, check: value == option.id) { value = option.id }
                    }
                }
            }
        }
    }
}

struct SettingsScroll<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12, content: content)
                .frame(maxWidth: 680)
                .padding(16).frame(maxWidth: .infinity)
        }
    }
}

struct SectionCaption: View {
    let title: String
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Text(title).font(.system(size: 13)).foregroundStyle(MobilePalette(scheme).secondary)
            .padding(.horizontal, 16).padding(.top, 4)
    }
}

struct MobileChoice: Identifiable {
    let id: String
    let title: String
    init(_ id: String, _ title: String) { self.id = id; self.title = title }
}

struct MobileChoicePage: View {
    let title: String
    let options: [MobileChoice]
    let selection: String
    var searchable = false
    let choose: (String) -> Void
    let back: () -> Void
    @State private var query = ""
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let visible = options.filter { query.isEmpty || $0.title.localizedCaseInsensitiveContains(query) }
        VStack(spacing: 0) {
            MobileHeader(title: title, back: back)
            VStack(spacing: 12) {
                if searchable {
                    MobileSearchField(title: mobileText("搜索", "Search"), query: $query, settings: true)
                } else {
                    Color.clear.frame(height: 0)
                }
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(visible) { option in
                            MobileRow(title: option.title, check: option.id == selection) { choose(option.id); back() }
                        }
                        if visible.isEmpty {
                            Text(query.isEmpty ? mobileText("暂无可选项", "No options available") : mobileText("没有匹配结果", "No matching results"))
                                .font(.system(size: 14)).foregroundStyle(MobilePalette(scheme).secondary)
                                .padding(16).frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .background(MobilePalette(scheme).surface, in: .rect(cornerRadius: 16))
                    .padding(.bottom, 16)
                }.scrollDismissesKeyboard(.interactively)
            }.frame(maxWidth: 680).padding(.horizontal, 16)
        }
        .background(MobilePalette(scheme).background.ignoresSafeArea())
        .accessibilityAddTraits(.isModal)
    }
}

struct ChoiceRequest: Identifiable {
    let id = UUID()
    let title: String
    let choices: [MobileChoice]
    let value: String
    var searchable = false
    let choose: (String) -> Void

    func page(back: @escaping () -> Void) -> some View {
        MobileChoicePage(title: title, options: choices, selection: value, searchable: searchable,
                         choose: choose, back: back)
    }
}

enum MobileFontField: String, CaseIterable, Identifiable {
    case cjk, cjkLatin, latin, latinCjk, code
    var id: Self { self }
    var title: String {
        switch self {
        case .cjk, .latin: mobileText("主要字体", "Primary font")
        case .cjkLatin: mobileText("拉丁字体", "Latin font")
        case .latinCjk: mobileText("中日韩字体", "CJK font")
        case .code: mobileText("代码字体", "Code font")
        }
    }
    var path: WritableKeyPath<ReadingOptions, String> {
        switch self {
        case .cjk: \.cjkFont
        case .cjkLatin: \.cjkLatinFont
        case .latin: \.latinFont
        case .latinCjk: \.latinCjkFont
        case .code: \.monospaceFont
        }
    }
    var pageTitle: String {
        switch self {
        case .cjk: mobileText("中日韩主要字体", "CJK primary font")
        case .latin: mobileText("其他语言主要字体", "Other-language primary font")
        default: title
        }
    }
    func choices(_ fonts: FontFamilies) -> [MobileChoice] {
        let values: [String]
        let automatic: String?
        switch self {
        case .cjk: values = fonts.cjk; automatic = nil
        case .cjkLatin: values = fonts.latin; automatic = mobileText("跟随其他语言字体", "Use other-language font")
        case .latin: values = fonts.latin; automatic = nil
        case .latinCjk: values = fonts.cjk; automatic = mobileText("跟随中日韩主要字体", "Use CJK primary font")
        case .code: values = fonts.monospace; automatic = nil
        }
        return (automatic.map { [MobileChoice("", $0)] } ?? []) + values.map { MobileChoice($0, $0) }
    }
}

struct FontSettings<General: View>: View {
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    var apply: ((ReadingOptions) -> Void)? = nil
    @ViewBuilder var general: () -> General
    @State private var options: ReadingOptions?
    @State private var fonts: FontFamilies?
    @State private var choice: ChoiceRequest?
    @State private var error: String?
    @State private var saving = false

    var body: some View {
        Group {
            SectionCaption(title: mobileText("中日韩书籍", "CJK books"))
            MobileGroup { row(.cjk); row(.cjkLatin) }
            SectionCaption(title: mobileText("其他语言书籍", "Other-language books"))
            MobileGroup { row(.latin); row(.latinCjk) }
            SectionCaption(title: mobileText("通用", "General"))
            MobileGroup { row(.code); general() }
            if let error { Text(error).font(.footnote).foregroundStyle(.red) }
        }
        .task(id: library.fontsReady) {
            do {
                options = try await core.readingOptions()
                fonts = try await core.fontFamilies()
            } catch { if !Task.isCancelled { self.error = message(error) } }
        }
        .fullScreenCover(item: $choice) { request in request.page { choice = nil } }
    }

    private func row(_ field: MobileFontField) -> some View {
        let value = options?[keyPath: field.path] ?? ""
        let choices = fonts.map(field.choices) ?? []
        return MobileRow(title: field.title, value: choices.first { $0.id == value }?.title ?? value) {
            choice = ChoiceRequest(title: field.pageTitle, choices: choices, value: value, searchable: true) { selected in
                guard var pending = options, pending[keyPath: field.path] != selected else { return }
                pending[keyPath: field.path] = selected
                saving = true
                Task {
                    defer { saving = false }
                    do {
                        try await core.saveReadingOptions(options: pending)
                        options = pending
                        apply?(pending)
                    } catch { self.error = message(error) }
                }
            }
        }.disabled(choices.isEmpty || options == nil || saving)
    }
}

struct FontDownloadRow: View {
    @ObservedObject var library: LibraryModel
    var body: some View {
        if !library.fontsReady {
            MobileRow(title: library.fontsDownloading ? mobileText("默认字体下载中", "Downloading reading fonts") :
                        mobileText("下载默认字体", "Download reading fonts")) {
                Task { await library.downloadFonts(retry: true) }
            }.disabled(library.fontsDownloading)
        }
    }
}

private struct TypesettingSettings: View {
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    let openLayout: () -> Void
    @AppStorage("reader.fontSize") private var font = 20
    @AppStorage("reader.focus") private var focus = false
    @AppStorage("reader.unified") private var unified = false
    @State private var size = Double(min(40, max(16, (UserDefaults.standard.object(forKey: "reader.fontSize") as? Int ?? 20) / 2 * 2)))

    var body: some View {
        SettingsScroll {
            FontSettings(core: core, library: library) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(mobileText("字号", "Font size")).font(.system(size: 16))
                    ReadingCapsuleSlider(value: $size, range: 16...40, step: 2,
                                         label: mobileText("字号", "Font size"), fontSize: true) { font = Int($0) }
                }.padding(.horizontal, 16).padding(.vertical, 10)
                MobileToggle(title: mobileText("专注模式", "Focus mode"), value: $focus)
                if !focus { MobileToggle(title: mobileText("统一版式", "Unified layout"), value: $unified) }
            }
            MobileGroup {
                MobileRow(title: mobileText("AI 排版", "AI layout"), icon: "ai", action: openLayout)
                FontDownloadRow(library: library)
            }
        }
    }
}

private struct AboutScreen: View {
    let core: MobileCore
    @Environment(\.colorScheme) private var scheme
    @EnvironmentObject private var updates: AppUpdates

    var body: some View {
        ViewThatFits(in: .vertical) {
            VStack(spacing: 0) { identity; Spacer(minLength: 32); footer }.padding(.vertical, 32)
            ScrollView { VStack(spacing: 32) { identity; footer }.padding(.vertical, 32) }
        }.padding(.horizontal, 24).frame(maxWidth: .infinity, maxHeight: .infinity)
            .task { updates.checkIfNeeded(core) }
    }

    private var identity: some View {
        VStack(spacing: 12) {
            if let appIcon = UIImage(named: "AboutLogo") {
                Image(uiImage: appIcon).resizable().scaledToFit()
                    .frame(width: 104, height: 104).clipShape(.rect(cornerRadius: 22))
                    .accessibilityLabel(mobileText("Torto 徽标", "Torto logo"))
                    .padding(.top, 8).padding(.bottom, 8)
            }
            Text("Torto").font(.system(size: 28, weight: .semibold))
            Text(updates.version).font(.system(size: 14)).foregroundStyle(MobilePalette(scheme).secondary)
            AboutUpdates(updates: updates, core: core)
        }.frame(maxWidth: .infinity)
    }

    private var footer: some View {
        VStack(spacing: 0) {
            Text("© \(Calendar.current.component(.year, from: Date()).formatted(.number.grouping(.never))) TortoTech")
            Link(mobileText("官方网站", "Website"), destination: URL(string: "https://tortotech.github.io/")!)
                .frame(minHeight: 44)
        }.font(.system(size: 12)).foregroundStyle(MobilePalette(scheme).secondary)
    }
}
