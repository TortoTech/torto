import CoreFoundation
import CoreGraphics
import Foundation

@main
struct BridgeSmoke {
    static func main() async throws {
        let args = CommandLine.arguments
        precondition(args.count == 4)
        let root = args[1], mode = args[2], fixture = args[3]
        try initializeIosCredentialStorage(path: root + "/config/credentials.json")
        let core = try MobileCore(dataDirectory: root)
        let phone = Viewport(width: 390, height: 720, scale: 2)
        let tablet = Viewport(width: 820, height: 1040, scale: 2)
        if mode == "update" {
            try await verifyUpdateBridge(core, root: root)
        } else if mode == "write" {
            let books = try await core.importBook(path: fixture)
            precondition(books.count == 1)
            let reader = try await core.openBook(id: books[0].id, viewport: phone)
            let initial = try await reader.focusMode(enabled: true).info()
            precondition(initial.isScroll && !initial.regions.isEmpty && !initial.focusBounds.isEmpty)
            let next = try await reader.turnReadingUnit(forward: true)
            let info = next.info()
            precondition(info.section != initial.section || info.activeTocId != initial.activeTocId)
            let region = try focusedRegion(info)
            try await reader.selectionMode(mode: "paragraph")
            let bounds = region.bounds[0]
            let x = bounds.x + bounds.width / 2, y = bounds.y + bounds.height / 2
            if let selection = try await reader.select(x: x, y: y, endX: x, endY: y) {
                precondition(!selection.text.isEmpty)
                let notes = try await reader.saveAnnotation(note: "iOS smoke note")
                precondition(notes.contains { $0.note == "iOS smoke note" })
            } else { throw SmokeError.selection }
            try Data(region.text.utf8).write(to: URL(fileURLWithPath: root + "/expected.txt"))
            // The image must still be readable after the Rust session and its
            // page wrapper release their own references.
            let raster = try await detachedRaster(reader)
            try await reader.shutdown()
            let data = raster.image.dataProvider?.data
            precondition(data.map { CFDataGetLength($0) } == raster.image.width * raster.image.height * 4)
            print("Imported and rendered EPUB, advanced section, saved note, retained pixels after shutdown")
        } else {
            let books = try await core.books()
            precondition(books.count == 1)
            let reader = try await core.openBook(id: books[0].id, viewport: tablet)
            let page = try await reader.focusMode(enabled: true)
            let expected = try String(contentsOfFile: root + "/expected.txt", encoding: .utf8)
            let restored = try focusedRegion(page.info())
            precondition(restored.text == expected, "The source position changed across restart and width")
            let notes = try await reader.annotations()
            precondition(notes.contains { $0.note == "iOS smoke note" })
            let resized = try await reader.resize(viewport: phone)
            let resizedRegion = try focusedRegion(resized.info())
            precondition(resizedRegion.text == expected, "The source position changed during resize")
            try await reader.shutdown()
            print("Restored the same focused text and note in a new process at iPad width; resize preserved it")
        }
    }

    private static func verifyUpdateBridge(_ core: MobileCore, root: String) async throws {
        let version = "0.10.0"
        let name = "Torto-\(version)-ios-arm64.ipa"
        let bytes = Data("fixture IPA bytes".utf8)
        let folder = URL(fileURLWithPath: root).appendingPathComponent("cache/updates/\(version)")
        let package = folder.appendingPathComponent(name)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try bytes.write(to: package)
        let token = String(decoding: try JSONSerialization.data(withJSONObject: [
            "version": version, "notes": "", "asset": [
                "name": name,
                "url": "https://github.com/TortoTech/torto/releases/download/v\(version)/\(name)",
                "github_asset_id": 72, "size": bytes.count,
                "sha256": "111c755e4ad459fbe0fb98275430f9ef957a4c3d8102e16a9569c4bc9b7c792a",
            ],
        ]), as: UTF8.self)
        let stopped = ChatRequest()
        stopped.cancel()
        do {
            _ = try await core.downloadUpdate(token: token, abi: "ios-arm64", request: stopped)
            throw SmokeError.expectedCancellation
        } catch is CoreError {
            // A stopped request must not deliver a verified package to Swift.
        }
        let path = try await core.downloadUpdate(token: token, abi: "ios-arm64", request: ChatRequest())
        let verified = try Data(contentsOf: URL(fileURLWithPath: path))
        precondition(path == package.path && verified == bytes)
        print("Cancelled an iOS update and returned the verified IPA through the Swift bridge")
    }

    private static func detachedRaster(_ reader: MobileReader) async throws -> ReaderRaster {
        try ReaderRaster(await reader.page())
    }

    private static func focusedRegion(_ info: PageInfo) throws -> TextRegion {
        guard let first = info.focusBounds.first,
              let region = info.regions.first(where: { r in r.bounds.contains { b in
                  min(b.y + b.height, first.y + first.height) > max(b.y, first.y)
              } }), !region.bounds.isEmpty else { throw SmokeError.focus }
        return region
    }

    enum SmokeError: Error { case focus, selection, expectedCancellation }
}
