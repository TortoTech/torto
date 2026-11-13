import SwiftUI

struct CloudSheet: View {
    let core: MobileCore
    @ObservedObject var cloud: CloudSyncCoordinator
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @State private var options: CloudOptions?
    @State private var saved: CloudOptions?
    @State private var presets: [CloudPreset] = []
    @State private var password = ""
    @State private var saving = false
    @State private var error: String?
    @State private var choice: ChoiceRequest?
    private var changed: Bool { options != saved || !password.isEmpty }
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            MobileHeader(title: mobileText("云同步", "Cloud sync")) {
                if changed { save(exit: true) } else { dismiss() }
            }.disabled(saving)
            SettingsScroll {
                if let options {
                    MobileGroup {
                        MobileToggle(title: mobileText("启用云同步", "Enable cloud sync"), value: field(\.enabled, default: options.enabled))
                    }.disabled(saving)
                    MobileGroup {
                        MobileRow(title: mobileText("云盘提供商", "Cloud provider"),
                                  value: presets.first { $0.id == options.provider }?.name ?? "WebDAV") {
                            choice = ChoiceRequest(title: mobileText("云盘提供商", "Cloud provider"),
                                                   choices: presets.map { MobileChoice($0.id, $0.name) }, value: options.provider) { id in
                                self.options?.provider = id
                                if let preset = presets.first(where: { $0.id == id }) { self.options?.url = preset.url }
                            }
                        }.disabled(presets.isEmpty)
                        VStack(spacing: 12) {
                            MobileFormField(title: "WebDAV URL", text: field(\.url, default: options.url))
                                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                                .disabled(options.provider != "custom")
                            MobileFormField(title: mobileText("用户名", "Username"), text: field(\.username, default: options.username))
                                .textInputAutocapitalization(.never).autocorrectionDisabled()
                            MobileFormField(title: options.passwordSaved ? mobileText("密码（已保存，留空保留）", "Password (saved; leave empty to keep)") :
                                            mobileText("密码", "Password"), text: $password, secure: true)
                            MobileFormField(title: mobileText("设备名称", "Device name"), text: field(\.deviceName, default: options.deviceName))
                        }.padding(.horizontal, 16).padding(.vertical, 12)
                    }.disabled(saving)
                    MobileGroup { actions }
                }
                if let error { Text(error).font(.footnote).foregroundStyle(.red) }
            }
        }
        .foregroundStyle(palette.ink).background(palette.background.ignoresSafeArea())
        .fullScreenCover(item: $choice) { request in request.page { choice = nil } }
        .task {
            do {
                let loaded = try await core.cloudOptions()
                var draft = loaded
                if draft.deviceName == "Torto Android" { draft.deviceName = "Torto iOS" }
                options = draft; saved = draft
                presets = try await core.cloudPresets()
            } catch { self.error = message(error) }
        }
    }

    private var actions: some View {
        let status = cloud.status
        return VStack(alignment: .leading, spacing: 12) {
            if cloud.running || status.error != nil || status.retry != nil {
                VStack(alignment: .leading, spacing: 6) {
                    if status.booksTotal > 0 {
                        Text(mobileText("已就绪 \(status.booksCompleted)/\(status.booksTotal) 本",
                                        "\(status.booksCompleted)/\(status.booksTotal) books ready"))
                            .font(.subheadline.weight(.medium))
                    }
                    if cloud.running {
                        Text(stageTitle(status.stage))
                        if status.booksTotal > 0 {
                            ProgressView(value: Double(status.booksCompleted), total: Double(status.booksTotal))
                        } else if status.total > 0 {
                            ProgressView(value: Double(status.completed), total: Double(status.total))
                        } else { ProgressView().progressViewStyle(.linear) }
                    } else if let retry = status.retry {
                        Text(mobileText("同步已暂停，最早重试时间：", "Sync paused; retry no earlier than: ") + timestamp(retry))
                    }
                    if let error = status.error { Text(error).foregroundStyle(.red) }
                }
                .font(.footnote).frame(maxWidth: .infinity, alignment: .leading).padding(12)
                .background(palette.subtle, in: .rect(cornerRadius: 8))
            }
            if cloud.running {
                Button(mobileText("停止同步", "Stop sync"), action: cloud.stop)
            } else {
                Text(mobileText("上次同步：", "Last sync: ") + (status.lastSuccess.map(timestamp) ?? "—"))
                    .font(.footnote).foregroundStyle(palette.secondary)
            }
            HStack(spacing: 12) {
                Button(mobileText("保存", "Save")) { save() }
                    .buttonStyle(.borderedProminent).disabled(saving || !changed)
                Button(mobileText("同步", "Sync")) {
                    guard let saved else { return }
                    Task { await cloud.synchronize(saved) }
                }.buttonStyle(.bordered).disabled(saved?.enabled != true || saving || changed || cloud.running)
            }
        }.padding(16)
    }

    private func timestamp(_ date: Date) -> String { date.formatted(date: .numeric, time: .shortened) }

    private func stageTitle(_ stage: String?) -> String {
        switch stage {
        case "preparing": mobileText("准备同步", "Preparing sync")
        case "checking": mobileText("检查云端变更", "Checking cloud changes")
        case "uploading": mobileText("上传书籍", "Uploading books")
        case "downloading": mobileText("下载书籍", "Downloading books")
        case "derived": mobileText("同步 PDF 处理数据", "Syncing PDF processing data")
        case "statistics": mobileText("同步阅读统计", "Syncing reading statistics")
        default: mobileText("同步进度与批注", "Syncing progress and annotations")
        }
    }

    private func field<T>(_ path: WritableKeyPath<CloudOptions, T>, default initial: T) -> Binding<T> {
        Binding(get: { options?[keyPath: path] ?? initial }, set: { options?[keyPath: path] = $0 })
    }

    private func save(exit: Bool = false) {
        guard let options, !saving else { return }
        saving = true
        error = nil
        Task {
            defer { saving = false }
            do {
                let updated = try await cloud.save(options, password: password.isEmpty ? nil : password)
                self.options = updated; saved = updated
                password = ""
                if exit { dismiss() }
            } catch { self.error = message(error) }
        }
    }
}
