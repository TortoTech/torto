import SwiftUI
import UIKit
import UniformTypeIdentifiers

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

    enum Route { case theme, language, providers, provider, translation, assistant, layout, about }

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
                        MobileRow(title: title(.layout), icon: "type") { routes.append(.layout) }
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
                                 description: mobileText("开启后，将跟随系统切换界面主题", "When enabled, interface theme follows your system settings"),
                                 choices: [MobileChoice("light", mobileText("浅色主题", "Light theme")), MobileChoice("dark", mobileText("深色主题", "Dark theme"))])
            case .language:
                InterfaceOptions(value: $language, fallback: (Locale.preferredLanguages.first ?? "en").hasPrefix("zh") ? "zh-CN" : "en",
                                 description: mobileText("开启后，将跟随系统设置界面语言", "When enabled, the interface language follows your system settings"),
                                 choices: [MobileChoice("zh-CN", "中文"), MobileChoice("en", "English")])
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
        case "dark": mobileText("深色主题", "Dark theme")
        case "light": mobileText("浅色主题", "Light theme")
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
        case .theme: mobileText("界面主题", "Interface theme")
        case .language: mobileText("界面语言", "Interface language")
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
    private func displayName(_ family: String) -> String {
        family == "LXGW WenKai GB Screen" ? "霞鹜文楷" : family
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
        return (automatic.map { [MobileChoice("", $0)] } ?? []) + values.map { MobileChoice($0, displayName($0)) }
    }
}

struct ReaderFontStylePanel: View {
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    let isCjkBook: Bool
    var apply: ((ReadingOptions) -> Void)? = nil
    @Environment(\.colorScheme) private var scheme
    @State private var options: ReadingOptions?
    @State private var catalog: ReaderFontCatalog?
    @State private var activeTab: MobileFontField = .cjk
    @State private var activeGroupId: String = ""
    @State private var downloadingFamily: String?
    @State private var importing = false
    @State private var saving = false
    @State private var error: String?
    private var palette: MobilePalette { MobilePalette(scheme) }

    private var tabs: [MobileFontField] {
        isCjkBook ? [.cjk, .cjkLatin, .code] : [.latin, .latinCjk, .code]
    }

    private func tabTitle(_ tab: MobileFontField) -> String {
        switch tab {
        case .cjk: mobileText("中文字体", "CJK")
        case .latinCjk: mobileText("中日韩字体", "CJK")
        case .latin, .cjkLatin: mobileText("拉丁字体", "Latin")
        case .code: mobileText("代码字体", "Code")
        }
    }

    private func currentFamily(_ opts: ReadingOptions) -> String {
        switch activeTab {
        case .cjk: opts.cjkFont
        case .cjkLatin: opts.cjkLatinFont.isEmpty ? opts.latinFont : opts.cjkLatinFont
        case .latin: opts.latinFont
        case .latinCjk: opts.latinCjkFont.isEmpty ? opts.cjkFont : opts.latinCjkFont
        case .code: opts.monospaceFont
        }
    }

    private func groups(for cat: ReaderFontCatalog) -> [FontGroup] {
        switch activeTab {
        case .cjk, .latinCjk: cat.cjkGroups
        case .latin, .cjkLatin: cat.latinGroups
        case .code: []
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 0) {
                ForEach(tabs, id: \.self) { tab in
                    let selected = activeTab == tab
                    Button {
                        activeTab = tab
                        syncGroup()
                    } label: {
                        Text(tabTitle(tab))
                            .font(.system(size: 13))
                            .lineLimit(1)
                            .foregroundStyle(selected ? palette.ink : palette.muted)
                            .frame(maxWidth: .infinity, minHeight: 30)
                            .background(selected ? palette.surface : .clear, in: RoundedRectangle(cornerRadius: 13))
                    }.buttonStyle(.plain)
                }
            }
            .padding(3)
            .background(palette.surfaceMuted, in: RoundedRectangle(cornerRadius: 16))

            if let cat = catalog, let opts = options {
                let current = currentFamily(opts)
                let gs = groups(for: cat)
                if !gs.isEmpty {
                    HStack(spacing: 0) {
                        ForEach(gs, id: \.id) { group in
                            let selected = activeGroupId == group.id
                            let extra = (group.id == "system" || group.id == "builtin") ? 1 : 0
                            let text = "\(mobileText(group.titleZh, group.titleEn))(\(group.items.count + extra))"
                            Button { activeGroupId = group.id } label: {
                                Text(text)
                                    .font(.system(size: 12))
                                    .lineLimit(1)
                                    .foregroundStyle(selected ? palette.ink : palette.muted)
                                    .frame(maxWidth: .infinity, minHeight: 26)
                                    .background(selected ? palette.surface : .clear, in: RoundedRectangle(cornerRadius: 11))
                            }.buttonStyle(.plain)
                        }
                    }
                    .padding(3)
                    .background(palette.surfaceMuted, in: RoundedRectangle(cornerRadius: 14))
                }

                let showImport = activeTab == .code || activeGroupId == "system" || activeGroupId == "builtin"
                let items = activeTab == .code ? cat.monospace : (gs.first { $0.id == activeGroupId }?.items ?? [])
                let busy = saving || downloadingFamily != nil

                ScrollView {
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 8), count: 3), spacing: 8) {
                        ForEach(items, id: \.family) { item in
                            let selected = item.family.caseInsensitiveCompare(current) == .orderedSame
                            let downloading = downloadingFamily == item.family
                            let label = downloading ? mobileText("下载中", "Downloading") : (item.label == "系统字体" ? mobileText("系统字体", "System font") : item.label)
                            Button { select(item) } label: {
                                Text(label)
                                    .font(.system(size: 12))
                                    .lineLimit(1)
                                    .foregroundStyle((selected || downloading) ? palette.accent : palette.ink)
                                    .padding(.horizontal, 6)
                                    .frame(maxWidth: .infinity, minHeight: 40)
                                    .background(downloading ? palette.accent.opacity(0.12) : palette.surface, in: RoundedRectangle(cornerRadius: 8))
                                    .overlay(RoundedRectangle(cornerRadius: 8).stroke((selected || downloading) ? palette.accent : palette.border, lineWidth: selected ? 1.5 : 1))
                            }.buttonStyle(.plain).disabled(busy)
                        }
                        if showImport {
                            Button { importing = true } label: {
                                Text(mobileText("导入字体", "Import font"))
                                    .font(.system(size: 12))
                                    .lineLimit(1)
                                    .foregroundStyle(palette.ink)
                                    .padding(.horizontal, 6)
                                    .frame(maxWidth: .infinity, minHeight: 40)
                                    .background(palette.surface, in: RoundedRectangle(cornerRadius: 8))
                                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(palette.border, lineWidth: 1))
                            }.buttonStyle(.plain).disabled(busy)
                        }
                    }
                }.frame(maxHeight: 240)
            } else {
                ProgressView().frame(maxWidth: .infinity, minHeight: 160)
            }

            if let error { Text(error).font(.footnote).foregroundStyle(.red) }
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 16)
        .task(id: library.fontsReady) {
            activeTab = tabs.first ?? .cjk
            do {
                options = try await core.readingOptions()
                catalog = try await core.readerFontCatalog()
                syncGroup()
            } catch { if !Task.isCancelled { self.error = message(error) } }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.font]) { result in
            guard case .success(let url) = result else { return }
            Task {
                let accessed = url.startAccessingSecurityScopedResource()
                defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                do {
                    _ = try await core.importCustomFont(path: url.path)
                    catalog = try await core.readerFontCatalog()
                } catch { self.error = message(error) }
            }
        }
    }

    private func syncGroup() {
        guard let cat = catalog, let opts = options else { return }
        let gs = groups(for: cat)
        let cur = currentFamily(opts)
        activeGroupId = gs.first { g in g.items.contains { $0.family.caseInsensitiveCompare(cur) == .orderedSame } }?.id ?? gs.first?.id ?? ""
    }

    private func select(_ item: FontOption) {
        guard var pending = options, !saving, downloadingFamily == nil else { return }
        let field = activeTab
        Task {
            error = nil
            if !item.downloaded {
                downloadingFamily = item.family
                do {
                    try await core.downloadFontFamily(family: item.family)
                    catalog = try await core.readerFontCatalog()
                } catch {
                    downloadingFamily = nil
                    self.error = message(error)
                    return
                }
                downloadingFamily = nil
            }
            pending[keyPath: field.path] = item.family
            saving = true
            defer { saving = false }
            do {
                try await core.saveReadingOptions(options: pending)
                options = pending
                apply?(pending)
            } catch { self.error = message(error) }
        }
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
