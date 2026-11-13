import SwiftUI

@MainActor
final class AppUpdates: ObservableObject {
    enum Phase {
        case idle, checking, current, checkFailed
        case available(AppUpdate, failed: Bool)
        case downloading(AppUpdate, progress: UInt32, stopping: Bool)
        case downloaded(AppUpdate, URL)
    }

    @Published private(set) var phase: Phase = .idle
    let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    private let platform = "ios-arm64"
    private var downloadRequest: ChatRequest?
    private var downloadID: UUID?

    func checkIfNeeded(_ core: MobileCore) {
        guard case .idle = phase else { return }
        check(core)
    }

    func check(_ core: MobileCore) {
        guard !version.isEmpty else { return }
        switch phase {
        case .idle, .checkFailed, .current: break
        default: return
        }
        phase = .checking
        Task {
            do {
                if let update = try await core.checkUpdate(current: version, abi: platform, source: "github") {
                    phase = .available(update, failed: false)
                } else {
                    phase = .current
                }
            } catch {
                phase = .checkFailed
            }
        }
    }

    func download(_ core: MobileCore) {
        guard case .available(let update, _) = phase else { return }
        let request = ChatRequest()
        let id = UUID()
        downloadRequest = request
        downloadID = id
        phase = .downloading(update, progress: 0, stopping: false)
        request.observeAi(listener: UpdateProgressSink { [weak self] completed in
            DispatchQueue.main.async { [weak self] in
                guard let self, downloadID == id,
                      case .downloading(let update, let previous, false) = phase else { return }
                let value = min(99, completed)
                if value != previous { phase = .downloading(update, progress: value, stopping: false) }
            }
        })
        Task {
            defer { downloadRequest = nil; downloadID = nil }
            do {
                let path = try await core.downloadUpdate(token: update.token, abi: platform, request: request)
                phase = .downloaded(update, URL(fileURLWithPath: path))
            } catch {
                let stopping: Bool
                if case .downloading(_, _, let stopped) = phase { stopping = stopped } else { stopping = false }
                phase = .available(update, failed: !stopping)
            }
        }
    }

    func stopDownload() {
        guard case .downloading(let update, let progress, false) = phase else { return }
        phase = .downloading(update, progress: progress, stopping: true)
        downloadRequest?.cancel()
    }
}

private final class UpdateProgressSink: AiProgressListener {
    let receive: @Sendable (UInt32) -> Void
    init(_ receive: @escaping @Sendable (UInt32) -> Void) { self.receive = receive }
    func update(stage: String, completed: UInt32, total: UInt32) {
        if stage == "update" { receive(completed) }
    }
}

struct AboutUpdates: View {
    @ObservedObject var updates: AppUpdates
    let core: MobileCore
    @Environment(\.colorScheme) private var scheme
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        switch updates.phase {
        case .idle, .checking:
            status(mobileText("正在检查更新", "Checking for updates"))
        case .current:
            status(mobileText("暂无可用的 iOS 更新", "No iOS update available"))
        case .checkFailed:
            status(mobileText("暂时无法检查更新", "Unable to check for updates"))
            Button(mobileText("重试", "Retry")) { updates.check(core) }
        case .available(let update, let failed):
            release(update)
            Button(failed ? mobileText("重试下载", "Retry download") : mobileText("下载更新", "Download update")) {
                updates.download(core)
            }.buttonStyle(.borderedProminent)
            if failed { status(mobileText("更新下载失败", "Could not download the update")) }
        case .downloading(let update, let progress, let stopping):
            release(update)
            ProgressView(value: Double(progress), total: 100).tint(palette.accent)
                .accessibilityLabel(mobileText("下载进度", "Download progress"))
            status(stopping ? mobileText("正在停止下载", "Stopping download") : "\(progress)%")
            Button(mobileText("停止下载", "Stop download"), action: updates.stopDownload).disabled(stopping)
        case .downloaded(let update, let url):
            release(update)
            status(mobileText("下载完成", "Download complete"))
            ShareLink(item: url) { Text(mobileText("导出更新包", "Export update package")) }
                .buttonStyle(.borderedProminent)
        }
    }

    private func status(_ title: String) -> some View {
        Text(title).font(.system(size: 13)).foregroundStyle(palette.secondary).multilineTextAlignment(.center)
    }

    @ViewBuilder private func release(_ update: AppUpdate) -> some View {
        status(mobileText("发现新版本 ", "New version available: ") + update.version)
        let notes = updateReleaseNotes(markdown: update.notes, language: mobileText("zh-CN", "en"))
        if !notes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            ReleaseNotesText(markdown: notes)
        }
    }
}

private struct ReleaseNotesText: View {
    let markdown: String
    @State private var text: AttributedString?

    var body: some View {
        Text(text ?? AttributedString(markdown))
            .font(.system(size: 14)).frame(maxWidth: .infinity, alignment: .leading)
            .textSelection(.enabled)
            .environment(\.openURL, OpenURLAction { url in
                url.scheme?.lowercased() == "https" ? .systemAction : .discarded
            })
            .task(id: markdown) {
                let input = markdown
                let result = await Task.detached(priority: .utility) {
                    try? AttributedString(markdown: input, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))
                }.value
                if !Task.isCancelled { text = result }
            }
    }
}
