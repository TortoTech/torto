import Foundation
import SwiftUI

/// The Rust environment is process-wide. A failed shelf refresh must not
/// reinitialize its paths or credential store.
@MainActor
final class LibraryModel: ObservableObject {
    static let shared = LibraryModel()
    @Published private(set) var books: [BookSummary] = []
    @Published private(set) var core: MobileCore?
    @Published private(set) var working = false
    @Published var error: String?
    @Published private(set) var syncRevision = 0
    @Published private(set) var coverRevision = 0
    @Published private(set) var fontsReady = false
    @Published private(set) var downloadedFonts = 0
    @Published private(set) var fontsDownloading = false
    private var triedFonts = false
    lazy var cloud = CloudSyncCoordinator(library: self)
    private var syncRefresh: Task<Void, Never>?
    private var reloadAfterSync = false
    private var loadingSyncedBooks = false

    func syncChanged(covers: Bool, reloadBooks: Bool) {
        if covers { coverRevision &+= 1 }
        reloadAfterSync = reloadAfterSync || reloadBooks
        guard syncRefresh == nil else { return }
        syncRefresh = Task {
            try? await Task.sleep(for: .milliseconds(250))
            syncRefresh = nil
            syncRevision &+= 1
            await refreshAfterSync()
        }
    }

    private func refreshAfterSync() async {
        guard reloadAfterSync, let core, !working, !loadingSyncedBooks else { return }
        loadingSyncedBooks = true
        defer {
            loadingSyncedBooks = false
            if reloadAfterSync, !working { Task { await refreshAfterSync() } }
        }
        reloadAfterSync = false
        if let updated = try? await core.books() {
            if working { reloadAfterSync = true } else { books = updated }
        }
    }

    private func finishLibraryWork() {
        working = false
        if reloadAfterSync { Task { await refreshAfterSync() } }
    }

    func downloadFonts(retry: Bool = false) async {
        guard (!triedFonts || retry), !fontsDownloading, let core else { return }
        fontsDownloading = true
        defer { fontsDownloading = false }
        triedFonts = true
        if let report = try? await core.downloadReadingFonts() {
            fontsReady = report.ready
            downloadedFonts = Int(report.downloaded)
        }
    }

    func refresh() async {
        guard !working else { return }
        working = true
        defer { finishLibraryWork() }
        do {
            let core = try makeCore()
            books = try await core.books()
        } catch { self.error = message(error) }
    }

    func importBooks(_ urls: [URL]) async {
        guard !working else { return }
        working = true
        defer { finishLibraryWork() }
        var failures: [String] = []
        do {
            let core = try makeCore()
            for url in urls {
                if Task.isCancelled { break }
                do {
                    let staged = try await Task.detached(priority: .userInitiated) {
                        try Self.stage(url)
                    }.value
                    defer { try? FileManager.default.removeItem(at: staged.deletingLastPathComponent()) }
                    books = try await core.importBook(path: staged.path)
                } catch { failures.append("\(url.lastPathComponent): \(message(error))") }
            }
        } catch { failures.append(message(error)) }
        if !failures.isEmpty { error = failures.joined(separator: "\n") }
    }

    func removeBook(_ book: BookSummary) async {
        guard !working, let core else { return }
        working = true
        defer { finishLibraryWork() }
        do {
            try await core.removeBook(id: book.id)
            books = try await core.books()
        } catch { self.error = message(error) }
    }

    func makeCore() throws -> MobileCore {
        if let core { return core }
        let files = FileManager.default
        let base = try files.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                 appropriateFor: nil, create: true)
        let root = base.appendingPathComponent("Torto", isDirectory: true)
        var privateConfig = root.appendingPathComponent("config", isDirectory: true)
        try files.createDirectory(at: privateConfig, withIntermediateDirectories: true)
        // Allow system-scheduled sync after the first device unlock, while
        // keeping the local credentials protected before that unlock.
        for name in ["", "credentials.json", "credentials.lock", "webdav-sync.json", "cloud-schedule-v1.json"] {
            let url = name.isEmpty ? privateConfig : privateConfig.appendingPathComponent(name)
            if files.fileExists(atPath: url.path) {
                try files.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
                                        ofItemAtPath: url.path)
            }
        }
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try privateConfig.setResourceValues(values)
        var cache = root.appendingPathComponent("cache", isDirectory: true)
        try files.createDirectory(at: cache, withIntermediateDirectories: true)
        try cache.setResourceValues(values)
        let newCore = try MobileCore(dataDirectory: root.path)
        try initializeIosCredentialStorage(path: privateConfig.appendingPathComponent("credentials.json").path)
        core = newCore
        cloud.coreReady(newCore)
        return newCore
    }

    nonisolated static let extensions: Set<String> = ["epub", "pdf", "mobi", "azw", "azw3", "fb2", "fbz", "cbz", "chm"]

    nonisolated private static func stage(_ url: URL) throws -> URL {
        guard extensions.contains(url.pathExtension.lowercased()) else {
            throw ImportError.unsupported
        }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let files = FileManager.default
        let folder = files.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let staged = folder.appendingPathComponent(url.lastPathComponent)
        try files.createDirectory(at: folder, withIntermediateDirectories: false)
        do {
            var coordinationError: NSError?
            var copyResult: Result<Void, Error>?
            NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { readable in
                copyResult = Result {
                    let metadata = try readable.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                    guard metadata.isRegularFile == true, let bytes = metadata.fileSize,
                          bytes > 0, bytes <= 256 * 1024 * 1024 else { throw ImportError.size }
                    try files.copyItem(at: readable, to: staged)
                }
            }
            if let coordinationError { throw coordinationError }
            guard let copyResult else { throw ImportError.unavailable }
            try copyResult.get()
            return staged
        } catch {
            try? files.removeItem(at: folder)
            throw error
        }
    }

    enum ImportError: LocalizedError {
        case unsupported, size, unavailable
        var errorDescription: String? {
            switch self {
            case .unsupported: String(localized: "This book format is not supported.")
            case .size: String(localized: "Select a book file no larger than 256 MB.")
            case .unavailable: String(localized: "The selected file could not be read.")
            }
        }
    }
}
