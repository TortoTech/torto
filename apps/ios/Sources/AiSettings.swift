import SwiftUI

struct ProviderList: View {
    let core: MobileCore
    let edit: (Provider) -> Void
    @State private var providers: [Provider] = []
    @State private var deleting: Provider?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        SettingsScroll {
            ForEach(providers, id: \.id) { provider in
                MobileGroup {
                    HStack(spacing: 0) {
                        Button { edit(provider) } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(provider.name).font(.system(size: 16, weight: .medium)).foregroundStyle(palette.ink)
                                Text(mobileText("\(provider.models.count) 个模型", "\(provider.models.count) models"))
                                    .font(.system(size: 12)).foregroundStyle(palette.secondary)
                            }.frame(maxWidth: .infinity, minHeight: 48, alignment: .leading).contentShape(Rectangle())
                        }.buttonStyle(MobileInkButtonStyle())
                        MobileIconButton(icon: "edit", label: mobileText("编辑提供商", "Edit provider")) { edit(provider) }
                        MobileIconButton(icon: "delete", label: mobileText("删除提供商", "Delete provider")) { deleting = provider }
                            .disabled(providers.count <= 1)
                    }.padding(16).disabled(busy)
                }
            }
            if let error { Text(error).font(.footnote).foregroundStyle(.red) }
        }
        .task { await load() }
        .alert(mobileText("删除提供商？", "Delete provider?"), isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } })) {
            Button(mobileText("删除", "Delete"), role: .destructive) {
                guard let deleting else { return }
                self.deleting = nil
                busy = true
                Task {
                    defer { busy = false }
                    do { try await core.deleteProvider(id: deleting.id); await load() }
                    catch { self.error = message(error) }
                }
            }
            Button(mobileText("取消", "Cancel"), role: .cancel) { deleting = nil }
        } message: { Text(deleting?.name ?? "") }
    }

    @Environment(\.colorScheme) private var scheme
    private var palette: MobilePalette { MobilePalette(scheme) }

    private func load() async {
        do { providers = try await core.providers() }
        catch { if !Task.isCancelled { self.error = message(error) } }
    }
}

struct ProviderEditor: View {
    let core: MobileCore
    let initial: Provider
    let done: () -> Void
    @State private var draft: Provider
    @State private var kinds: [ProviderKind] = []
    @State private var key = ""
    @State private var query = ""
    @State private var discovered: [String] = []
    @State private var choice: ChoiceRequest?
    @State private var error: String?
    @State private var busy = false
    @Environment(\.colorScheme) private var scheme

    init(core: MobileCore, initial: Provider, done: @escaping () -> Void) {
        self.core = core
        self.initial = initial
        self.done = done
        _draft = State(initialValue: initial)
    }

    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(spacing: 0) {
            SettingsScroll {
                MobileGroup {
                    MobileRow(title: mobileText("提供商类型", "Provider type"),
                              value: kinds.first { $0.id == draft.kind }?.name ?? draft.kind) {
                        choice = ChoiceRequest(title: mobileText("提供商类型", "Provider type"),
                                               choices: kinds.map { MobileChoice($0.id, $0.name) }, value: draft.kind) { id in
                            guard let kind = kinds.first(where: { $0.id == id }), draft.kind != id else { return }
                            if draft.name.isEmpty || kinds.contains(where: { $0.name == draft.name }) { draft.name = kind.name }
                            draft.kind = id
                            draft.baseUrl = kind.baseUrl
                        }
                    }.disabled(kinds.isEmpty)
                    VStack(spacing: 12) {
                        MobileFormField(title: mobileText("名称", "Name"), text: $draft.name)
                        MobileFormField(title: "Base URL", text: $draft.baseUrl).keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                        MobileFormField(title: initial.keySaved ? mobileText("API Key（已保存，留空保留）", "API key (saved; leave empty to keep)") : "API Key", text: $key, secure: true)
                    }.padding(16)
                }
                MobileGroup {
                    HStack {
                        Text(mobileText("模型", "Models")).font(.system(size: 16, weight: .semibold))
                        Spacer()
                        MobileIconButton(icon: "sync", label: busy ? mobileText("正在刷新模型列表…", "Refreshing model list…") : mobileText("刷新模型列表", "Refresh model list"), action: fetch)
                            .rotationEffect(.degrees(busy ? -360 : 0))
                            .animation(busy ? .linear(duration: 0.9).repeatForever(autoreverses: false) : .default, value: busy)
                            .disabled(draft.baseUrl.isEmpty)
                    }.padding(.leading, 16).padding(.trailing, 4)
                    MobileFormField(title: mobileText("搜索模型", "Search models"), text: $query)
                        .padding(.horizontal, 16).padding(.top, 8)
                    let sortedModels = Set(discovered + draft.models).sorted { a, b in
                        let sa = draft.models.contains(a), sb = draft.models.contains(b)
                        if sa != sb { return sa }
                        return a.localizedCaseInsensitiveCompare(b) == .orderedAscending
                    }
                    LazyVStack(spacing: 0) {
                        ForEach(sortedModels.filter { query.isEmpty || $0.localizedCaseInsensitiveContains(query) }, id: \.self) { id in
                            MobileRow(title: id, check: draft.models.contains(id)) {
                                if draft.models.contains(id) { draft.models.removeAll { $0 == id } }
                                else { draft.models.append(id) }
                            }
                        }
                    }
                }
                if let error { Text(error).font(.footnote).foregroundStyle(.red) }
            }.disabled(busy)
        }
        .task {
            kinds = (try? await core.providerKinds()) ?? []
            if !initial.id.isEmpty {
                discovered = (try? await core.cachedModels(providerId: initial.id)) ?? []
            }
        }
        .onChange(of: draft.baseUrl) { _, _ in discovered = [] }
        .onDisappear { saveIfChanged() }
        .fullScreenCover(item: $choice) { request in request.page { choice = nil } }
    }

    private func fetch() {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do { discovered = try await core.fetchModels(provider: draft, key: key.isEmpty ? nil : key) }
            catch { self.error = message(error) }
        }
    }

    private func saveIfChanged() {
        var value = draft
        value.baseUrl = value.baseUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        value.name = value.name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard (value != initial || !key.isEmpty), !value.name.isEmpty, !value.baseUrl.isEmpty else { return }
        let apiKey = key.isEmpty ? nil : key
        Task {
            try? await core.saveProvider(provider: value, key: apiKey, clearKey: false)
        }
    }
}

enum AiRole: Hashable {
    case layout, translation, assistant
    var provider: WritableKeyPath<AiOptions, String> {
        switch self { case .layout: \.layoutProvider; case .translation: \.translationProvider; case .assistant: \.chatProvider }
    }
    var model: WritableKeyPath<AiOptions, String> {
        switch self { case .layout: \.layoutModel; case .translation: \.translationModel; case .assistant: \.chatModel }
    }
    var effort: WritableKeyPath<AiOptions, String> {
        switch self { case .layout: \.layoutReasoning; case .translation: \.translationReasoning; case .assistant: \.chatReasoning }
    }
}

@MainActor
private enum AiRoleWrites {
    private static var pending: Task<Void, Never>?
    private static var revision = 0

    static func wait() async { await pending?.value }

    static func enqueue(_ write: @escaping @MainActor () async -> Void) {
        let previous = pending
        revision += 1
        let id = revision
        pending = Task {
            await previous?.value
            await write()
            if revision == id { pending = nil }
        }
    }
}

struct AiRoleSettings: View {
    let core: MobileCore
    let role: AiRole
    var onSave: (() -> Void)? = nil
    var onSaveError: ((String) -> Void)? = nil
    @State private var providers: [Provider] = []
    @State private var draft: AiOptions?
    @State private var baseline: AiOptions?
    @State private var choice: ChoiceRequest?
    @State private var error: String?
    @State private var custom = false
    @State private var customLanguage = ""
    private struct ProbeSelection: Equatable {
        let provider: String
        let address: String
        let model: String
    }
    @State private var layoutProtocol: (selection: ProbeSelection, value: String)?
    private var probeSelection: ProbeSelection? {
        guard role == .layout, let draft, !draft.layoutModel.isEmpty,
              let provider = providers.first(where: { $0.id == draft.layoutProvider }) else { return nil }
        return ProbeSelection(provider: provider.id, address: provider.baseUrl, model: draft.layoutModel)
    }

    @FocusState private var editingLanguage: Bool

    var body: some View {
        SettingsScroll {
            if let draft {
                let provider = providers.first { $0.id == draft[keyPath: role.provider] }
                let model = draft[keyPath: role.model]
                let decision = provider.map { isLayoutDecisionModel(baseUrl: $0.baseUrl, model: model) } == true
                    || (layoutProtocol?.selection == probeSelection && layoutProtocol?.value == "decision")
                let effort = (try? aiReasoningLevels(providerKind: provider?.kind ?? "custom", model: model)) ?? ["default", "none", "low", "medium", "high"]
                if role == .layout {
                    MobileGroup {
                        MobileToggle(title: mobileText("AI 排版", "AI layout"), value: bool(\.layoutEnabled))
                            .disabled(model.isEmpty)
                    }
                }
                let multiProvider = providers.count > 1
                let allModelChoices = providers.flatMap { p in
                    p.models.map { m in MobileChoice("\(p.id)\n\(m)", multiProvider ? "\(m) · \(p.name)" : m) }
                }
                let selectedKey = model.isEmpty ? "" : "\(draft[keyPath: role.provider])\n\(model)"
                let modelDesc: String? = switch role {
                case .layout: mobileText("具备视觉能力的决策/文本模型", "Vision-capable decision or text model")
                case .translation: nil
                case .assistant: mobileText("要求视觉能力", "Requires vision")
                }
                let effortDesc: String? = role == .translation ? mobileText("建议设为「低」或「不思考」", "Recommended: Low or None") : nil
                MobileGroup {
                    choiceRow(modelLabel, selectedKey, allModelChoices, description: modelDesc, searchable: true) { key in
                        let parts = key.split(separator: "\n", maxSplits: 1)
                        guard parts.count == 2 else { return }
                        change {
                            $0[keyPath: role.provider] = String(parts[0])
                            $0[keyPath: role.model] = String(parts[1])
                        }
                    }
                    if role != .layout || !decision {
                        choiceRow(mobileText("思考等级", "Reasoning effort"), draft[keyPath: role.effort], effort.map { MobileChoice($0, effortName($0)) }, description: effortDesc) { id in
                            change { $0[keyPath: role.effort] = id }
                        }
                    }
                }
                if role == .layout {
                    imageModelSettings(draft)
                }
                if role == .translation {
                    MobileGroup {
                        choiceRow(mobileText("目标语言", "Target language"), custom ? "custom" : draft.targetLanguage, [
                            MobileChoice("system", mobileText("跟随系统", "System language")), MobileChoice("zh-CN", "简体中文"),
                            MobileChoice("en", "English"), MobileChoice("custom", mobileText("自定义", "Custom")),
                        ]) { value in
                            custom = value == "custom"
                            if !custom { change { $0.targetLanguage = value } }
                        }
                        if custom {
                            MobileFormField(title: mobileText("自定义语言", "Custom language"), text: Binding(get: {
                                customLanguage
                            }, set: { customLanguage = String($0.prefix(128)) }))
                            .padding(.horizontal, 16).padding(.vertical, 8)
                            .focused($editingLanguage).onSubmit { editingLanguage = false }
                        }
                        MobileToggle(title: mobileText("显示原文", "Show original text"), value: bool(\.showOriginal))
                        MobileToggle(title: mobileText("提取术语表", "Extract glossary"), value: bool(\.expertTranslation))
                    }
                }
            }
            if let error { Text(error).font(.footnote).foregroundStyle(.red) }
        }
        .task {
            guard draft == nil else { return }
            await AiRoleWrites.wait()
            do {
                async let entries = core.providers()
                async let options = core.aiOptions()
                let (loaded, value) = try await (entries, options)
                guard !Task.isCancelled else { return }
                providers = loaded; draft = value; baseline = value
                custom = !["system", "zh-CN", "en"].contains(value.targetLanguage)
                if custom { customLanguage = value.targetLanguage }
            } catch { if !Task.isCancelled { self.error = message(error) } }
        }
        .accessibilityHidden(choice != nil)
        .task(id: probeSelection.map { [$0.provider, $0.address, $0.model] }) {
            guard let selection = probeSelection else { return }
            let type = (try? await core.probeLayoutModel(provider: selection.provider, model: selection.model))
                ?? "unconfirmed"
            guard !Task.isCancelled, probeSelection == selection else { return }
            layoutProtocol = (selection, type)
            if type == "chat" && draft?.imageModel.isEmpty == true {
                change {
                    $0.imageProvider = selection.provider
                    $0.imageModel = selection.model
                    $0.imageReasoning = $0.layoutReasoning
                }
            }
        }
        .onDisappear { if choice == nil { commit() } }
        .fullScreenCover(item: $choice) { request in request.page { choice = nil } }
    }

    private func imageModelSettings(_ options: AiOptions) -> some View {
        let provider = providers.first { $0.id == options.imageProvider }
        let multi = providers.count > 1
        let models = providers.flatMap { entry in
            entry.models.filter { !isLayoutDecisionModel(baseUrl: entry.baseUrl, model: $0) }
                .map { m in MobileChoice("\(entry.id)\n\(m)", multi ? "\(m) · \(entry.name)" : m) }
        }
        let selectedKey = options.imageModel.isEmpty ? "" : "\(options.imageProvider)\n\(options.imageModel)"
        let levels = (try? aiReasoningLevels(providerKind: provider?.kind ?? "custom", model: options.imageModel))
            ?? ["default", "none", "low", "medium", "high"]
        return MobileGroup {
            choiceRow(mobileText("排版生成模型", "Layout generation model"), selectedKey,
                      models, description: mobileText("要求视觉能力", "Requires vision"), searchable: true) { key in
                let parts = key.split(separator: "\n", maxSplits: 1)
                guard parts.count == 2 else { return }
                change {
                    $0.imageProvider = String(parts[0])
                    $0.imageModel = String(parts[1])
                }
            }
            choiceRow(mobileText("生成模型思考等级", "Generation reasoning effort"),
                      options.imageReasoning, levels.map { MobileChoice($0, effortName($0)) },
                      description: mobileText("建议设为「不思考」", "Recommended: None")) { id in
                change { $0.imageReasoning = id }
            }
        }
    }

    private func choiceRow(_ title: String, _ value: String, _ choices: [MobileChoice],
                           description: String? = nil, searchable: Bool = false, choose: @escaping (String) -> Void) -> some View {
        MobileRow(title: title, value: choices.first { $0.id == value }?.title ?? (value.isEmpty ? mobileText("未选择", "Not selected") : value), description: description) {
            choice = ChoiceRequest(title: title, choices: choices, value: value, searchable: searchable, choose: choose)
        }.disabled(choices.isEmpty)
    }

    private func bool(_ path: WritableKeyPath<AiOptions, Bool>) -> Binding<Bool> {
        Binding(get: { draft?[keyPath: path] ?? false }, set: { value in change { $0[keyPath: path] = value } })
    }

    private func change(_ mutate: (inout AiOptions) -> Void) {
        guard var value = draft else { return }
        mutate(&value)
        draft = value
    }

    private func commit() {
        guard let original = baseline, var edited = draft else { return }
        if role == .translation && custom {
            let target = customLanguage.trimmingCharacters(in: .whitespacesAndNewlines)
            edited.targetLanguage = target.isEmpty ? original.targetLanguage : target
        }
        guard edited != original else { return }
        baseline = edited
        AiRoleWrites.enqueue {
            do {
                var latest = try await core.aiOptions()
                let current = latest
                func apply<Value: Equatable>(_ key: WritableKeyPath<AiOptions, Value>) {
                    if edited[keyPath: key] != original[keyPath: key] { latest[keyPath: key] = edited[keyPath: key] }
                }
                apply(role.provider); apply(role.model); apply(role.effort)
                if role == .layout {
                    apply(\.layoutEnabled)
                    apply(\.imageProvider)
                    apply(\.imageModel)
                    apply(\.imageReasoning)
                }
                if role == .translation {
                    apply(\.targetLanguage); apply(\.showOriginal); apply(\.translateToc); apply(\.expertTranslation)
                }
                guard latest != current else { return }
                _ = try await core.saveAiOptions(options: latest)
                onSave?()
            } catch { onSaveError?(message(error)) }
        }
    }

    private var modelLabel: String {
        switch role {
        case .layout: mobileText("排版分类模型", "Layout classification model")
        case .translation: mobileText("翻译模型", "Translation model")
        case .assistant: mobileText("对话模型", "Chat model")
        }
    }

    private var hint: String {
        switch role {
        case .layout: mobileText("图片识别使用能识图的模型。", "Use a vision model for image recognition.")
        case .translation: mobileText("使用文字能力强、速度快的模型，思考等级设为“低”或“不思考”。", "Use a fast language model with low or no reasoning.")
        case .assistant: mobileText("使用能识图的模型。", "Use a model with vision.")
        }
    }

    private func effortName(_ value: String) -> String {
        switch value {
        case "default": mobileText("模型默认", "Model default")
        case "none": mobileText("不思考", "None")
        case "minimal": mobileText("最小", "Minimal")
        case "low": mobileText("低", "Low")
        case "medium": mobileText("中", "Medium")
        case "high": mobileText("高", "High")
        case "max": mobileText("最高", "Maximum")
        default: value
        }
    }
}
