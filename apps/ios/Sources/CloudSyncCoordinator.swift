import BackgroundTasks
import Network
import SwiftUI
import UIKit

@MainActor
final class CloudSyncCoordinator: ObservableObject {
    static let identifier = "org.tortotech.torto.cloud-sync"
    private static let interval: TimeInterval = 20 * 60
    private static let pendingKey = "cloud.backgroundReadingPending"

    struct Status {
        var stage: String?
        var completed: UInt32 = 0
        var total: UInt32 = 0
        var booksCompleted: UInt32 = 0
        var booksTotal: UInt32 = 0
        var lastSuccess: Date?
        var retry: Date?
        var error: String?
    }

    @Published private(set) var status = Status()
    @Published private(set) var running = false
    private unowned let library: LibraryModel
    private var core: MobileCore? { library.core }
    private var foreground = false
    private var enabled: Bool?
    private var account: String?
    private var configuring = false
    private var userStopped = false
    private var loop: Task<Void, Never>?
    private var runTask: Task<CloudSyncRun?, Never>?
    private var control: ChatRequest?
    private var runID: UUID?
    private var network: NWPathMonitor?
    private var networkAvailable: Bool?
    private var pendingDeadline: Date?
    private var exitTask = UIBackgroundTaskIdentifier.invalid
    private var exitToken: UUID?
    private var backgroundTask: BGTask?
    private var systemWork: Task<Void, Never>?
    private var configLoad: Task<CloudOptions?, Never>?

    init(library: LibraryModel) { self.library = library }

    func coreReady(_ core: MobileCore) {
        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            let available = path.status == .satisfied
            Task { @MainActor in self?.networkChanged(available) }
        }
        monitor.start(queue: DispatchQueue(label: "org.tortotech.torto.sync-network"))
        network = monitor
        if foreground { core.syncForeground(); startLoop() }
    }

    private func networkChanged(_ available: Bool) {
        let wasUnavailable = networkAvailable == false
        networkAvailable = available
        if available, wasUnavailable, enabled == true { core?.syncNetworkAvailable() }
    }

    func setScene(_ phase: ScenePhase) {
        switch phase {
        case .active:
            guard !foreground else { return }
            foreground = true
            userStopped = false
            endExitTask()
            if backgroundTask != nil || systemWork != nil { cancelSystemWork() }
            core?.syncForeground()
            startLoop()
        case .background:
            guard foreground else { return }
            foreground = false
            guard enabled == true, !userStopped, let core else {
                stopLoop(cancelRun: true)
                return
            }
            core.syncBackground()
            UserDefaults.standard.set(account, forKey: Self.pendingKey)
            submitBackground(noEarlierThan: status.retry ?? Date(), replace: true)
            stopLoop(cancelRun: true)
            guard !configuring else { return }
            let token = UUID()
            exitToken = token
            exitTask = UIApplication.shared.beginBackgroundTask(withName: "Torto reading progress") { [weak self] in
                if let self, self.exitToken == token { self.cancelSystemWork() }
            }
            if exitTask != .invalid { startSystemWork() }
        default:
            // System dialogs and Control Center briefly mark the scene inactive.
            break
        }
    }

    private func loadOptions(_ core: MobileCore) async -> CloudOptions? {
        if let configLoad { return await configLoad.value }
        let task = Task { try? await core.cloudOptions() }
        configLoad = task
        let options = await task.value
        configLoad = nil
        if let options { apply(options) }
        return options
    }

    private func apply(_ options: CloudOptions) {
        if options.account != account {
            account = options.account
            status = Status(lastSuccess: UserDefaults.standard.object(forKey: "cloud.lastSuccess.\(options.account)") as? Date)
        }
        enabled = options.enabled
        if !options.enabled {
            UserDefaults.standard.removeObject(forKey: Self.pendingKey)
            cancelBackgroundRequest()
        }
    }

    private func startLoop() {
        guard foreground, !configuring, !userStopped, loop == nil, let core else { return }
        loop = Task { [weak self] in
            guard let self else { return }
            defer {
                loop = nil
                // Resume after a temporary interruption once the cancelled
                // native operation has finished journaling.
                if Task.isCancelled { startLoop() }
            }
            guard let options = await loadOptions(core), options.enabled else { return }
            submitBackground(noEarlierThan: status.retry ?? Date().addingTimeInterval(Self.interval))
            if UserDefaults.standard.string(forKey: Self.pendingKey) == options.account,
               !Task.isCancelled, !configuring, !userStopped {
                if let runTask { _ = await runTask.value }
                if !Task.isCancelled { await flush(options) }
            }
            while !Task.isCancelled && foreground && !configuring && !userStopped {
                if let runTask {
                    _ = await runTask.value
                    continue
                }
                guard let result = await perform(.automatic, options: options),
                      !Task.isCancelled, result.nextPollMs > 0 else { return }
                do { _ = try await core.waitForSyncChange(revision: result.wakeRevision, timeoutMs: result.nextPollMs) }
                catch { return }
            }
        }
    }

    private func stopLoop(cancelRun: Bool) {
        loop?.cancel()
        if loop != nil { core?.wakeSyncWait() }
        if cancelRun { control?.cancel() }
    }

    func stop() {
        userStopped = true
        stopLoop(cancelRun: true)
        cancelSystemWork()
        cancelBackgroundRequest()
    }

    func save(_ options: CloudOptions, password: String?) async throws -> CloudOptions {
        guard let core else { return options }
        configuring = true
        defer { configuring = false; startLoop() }
        stopLoop(cancelRun: true)
        cancelSystemWork()
        await loop?.value
        _ = await runTask?.value
        _ = await configLoad?.value
        let updated = try await core.saveCloudOptions(o: options, password: password)
        userStopped = false
        apply(updated)
        return updated
    }

    func synchronize(_ options: CloudOptions) async {
        guard foreground, options.enabled, !configuring else { return }
        configuring = true
        defer { configuring = false; startLoop() }
        userStopped = false
        stopLoop(cancelRun: true)
        await loop?.value
        _ = await runTask?.value
        guard foreground, !userStopped else { return }
        _ = await perform(.manual, options: options)
    }

    private func perform(_ trigger: CloudSyncTrigger, options: CloudOptions) async -> CloudSyncRun? {
        guard runTask == nil, let core else { return nil }
        let id = UUID()
        let request = ChatRequest()
        runID = id
        control = request
        if trigger == .manual { running = true; status.stage = "preparing" }
        request.observeAi(listener: CloudProgressSink { [weak self] stage, completed, total in
            DispatchQueue.main.async { [weak self] in
                guard let self, runID == id else { return }
                progress(stage, completed, total)
            }
        })
        let task = Task<CloudSyncRun?, Never> {
            defer { runTask = nil; control = nil; runID = nil; running = false; status.stage = nil }
            do {
                let result = try await core.syncScheduled(account: options.account, trigger: trigger, request: request)
                if result.summary != nil {
                    let now = Date()
                    UserDefaults.standard.set(now, forKey: "cloud.lastSuccess.\(options.account)")
                    status.lastSuccess = now
                    library.syncChanged(covers: false, reloadBooks: true)
                }
                status.error = result.error ?? (result.summary != nil ? nil : status.error)
                status.retry = result.retryAfterMs.map { Date().addingTimeInterval(Double($0) / 1_000) }
                if let retry = status.retry { submitBackground(noEarlierThan: retry, replace: true) }
                return result
            } catch {
                let detail = message(error)
                if detail != "已停止处理" && !(error is CancellationError) { status.error = String(detail.prefix(500)) }
                return nil
            }
        }
        runTask = task
        return await task.value
    }

    @discardableResult
    private func flush(_ options: CloudOptions) async -> Bool {
        guard let result = await perform(.flushReading, options: options), result.summary != nil else { return false }
        if !Task.isCancelled {
            UserDefaults.standard.removeObject(forKey: Self.pendingKey)
            submitBackground(noEarlierThan: status.retry ?? Date().addingTimeInterval(Self.interval), replace: true)
        }
        return true
    }

    private func progress(_ stage: String, _ completed: UInt32, _ total: UInt32) {
        switch stage {
        case "library_changed":
            status.booksCompleted = completed
            status.booksTotal = total
            library.syncChanged(covers: false, reloadBooks: true)
        case "cover_changed": library.syncChanged(covers: true, reloadBooks: false)
        case "reading_changed": library.syncChanged(covers: false, reloadBooks: true)
        default:
            running = true
            status.stage = stage == "books" ? "downloading" : stage
            status.error = nil
            status.retry = nil
            if stage == "preparing" {
                status.booksCompleted = 0
                status.booksTotal = 0
            } else if stage == "books" {
                status.booksCompleted = completed
                status.booksTotal = total
            }
            status.completed = completed
            status.total = total
        }
    }

    private func submitBackground(noEarlierThan date: Date, replace: Bool = false) {
        guard enabled == true, !userStopped else { return }
        if let pendingDeadline, !replace || abs(pendingDeadline.timeIntervalSince(date)) < 5 { return }
        let request = BGProcessingTaskRequest(identifier: Self.identifier)
        request.requiresNetworkConnectivity = true
        request.earliestBeginDate = date
        // Re-submitting the identifier replaces its pending request.
        do {
            try BGTaskScheduler.shared.submit(request)
            pendingDeadline = date
        } catch { pendingDeadline = nil }
    }

    private func cancelBackgroundRequest() {
        pendingDeadline = nil
        BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.identifier)
    }

    func handle(_ task: BGTask) {
        pendingDeadline = nil
        if foreground {
            submitBackground(noEarlierThan: status.retry ?? Date().addingTimeInterval(Self.interval))
            task.setTaskCompleted(success: true)
            return
        }
        guard !configuring, !userStopped, backgroundTask == nil else {
            task.setTaskCompleted(success: true)
            return
        }
        backgroundTask = task
        task.expirationHandler = { [weak self, weak task] in
            Task { @MainActor in
                if let self, let task, self.backgroundTask === task { self.cancelSystemWork() }
            }
        }
        startSystemWork()
    }

    private func startSystemWork() {
        guard systemWork == nil else { return }
        systemWork = Task { [weak self] in
            guard let self else { return }
            var success = false
            defer {
                systemWork = nil
                backgroundTask?.setTaskCompleted(success: success)
                backgroundTask = nil
                endExitTask()
            }
            await loop?.value
            _ = await runTask?.value
            guard !Task.isCancelled, !foreground, !configuring, let core = try? library.makeCore(),
                  let options = await loadOptions(core), options.enabled, !Task.isCancelled else { return }
            if backgroundTask != nil {
                submitBackground(noEarlierThan: Date().addingTimeInterval(Self.interval))
            }
            if UserDefaults.standard.string(forKey: Self.pendingKey) == options.account {
                success = await flush(options)
            } else if backgroundTask != nil {
                if let result = await perform(.automatic, options: options) {
                    success = result.error == nil
                }
            }
        }
    }

    private func cancelSystemWork() {
        guard systemWork != nil || backgroundTask != nil || exitTask != .invalid else { return }
        systemWork?.cancel()
        control?.cancel()
        backgroundTask?.setTaskCompleted(success: false)
        backgroundTask = nil
        endExitTask()
    }

    private func endExitTask() {
        exitToken = nil
        guard exitTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(exitTask)
        exitTask = .invalid
    }
}

private final class CloudProgressSink: AiProgressListener {
    let receive: @Sendable (String, UInt32, UInt32) -> Void
    init(receive: @escaping @Sendable (String, UInt32, UInt32) -> Void) { self.receive = receive }
    func update(stage: String, completed: UInt32, total: UInt32) { receive(stage, completed, total) }
}

@MainActor
final class TortoAppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: CloudSyncCoordinator.identifier, using: .main) { task in
            MainActor.assumeIsolated { LibraryModel.shared.cloud.handle(task) }
        }
        return true
    }
}
