import SwiftUI
import UniformTypeIdentifiers
import UIKit

@main
struct TortoApp: App {
    @UIApplicationDelegateAdaptor(TortoAppDelegate.self) private var appDelegate
    @StateObject private var library = LibraryModel.shared
    @StateObject private var updates = AppUpdates()
    @AppStorage("interface.theme") private var theme = "system"
    @AppStorage("interface.language") private var language = "system"

    var body: some Scene {
        WindowGroup {
            HomeView(library: library, updates: updates, language: language)
                .environmentObject(updates)
                .preferredColorScheme(theme == "dark" ? .dark : theme == "light" ? .light : nil)
                .tint(MobilePalette.hex(0x078a46))
        }
    }
}

private struct HomeView: View {
    @ObservedObject var library: LibraryModel
    let updates: AppUpdates
    let language: String
    @Environment(\.scenePhase) private var phase
    @Environment(\.colorScheme) private var scheme
    @State private var selected: BookSummary?
    @State private var remove: BookSummary?
    @State private var importing = false
    @State private var profile = false
    @State private var settings = false
    @State private var feedback = false
    @State private var statistics = false
    @State private var details: BookSummary?
    @State private var openAfterDetails: BookSummary?
    @State private var totals: ReadingStatistics?
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                if profile {
                    MobileIconButton(icon: "feedback", label: mobileText("反馈问题", "Report an issue")) { feedback = true }
                        .disabled(library.core == nil)
                } else {
                    Text(mobileText("书架", "Library")).font(.system(size: 20, weight: .semibold))
                        .padding(.leading, 14).accessibilityAddTraits(.isHeader)
                }
                Spacer()
                if profile {
                    MobileIconButton(icon: "settings", label: mobileText("设置", "Settings")) { settings = true }
                        .disabled(library.core == nil)
                } else {
                    MobileIconButton(icon: "add", label: mobileText("导入书籍", "Import books")) { importing = true }
                        .disabled(library.working)
                }
            }
            .padding(.horizontal, 4).frame(height: 64)

            Group {
                if profile {
                    ScrollView {
                        MobileGroup {
                            Button { statistics = true } label: {
                                HStack {
                                    VStack(alignment: .leading, spacing: 8) {
                                        Text(mobileText("阅读统计", "Reading statistics")).font(.system(size: 16, weight: .medium))
                                            .padding(.bottom, 4)
                                        VStack(alignment: .leading, spacing: 0) {
                                            Text(totals.map { mobileDuration($0.today) } ?? "—").font(.system(size: 28))
                                            Text(mobileText("今日阅读", "Today")).font(.system(size: 16))
                                        }
                                        if let totals {
                                            Text(mobileText("本周", "This week") + " \(mobileDuration(totals.week)) · " +
                                                 mobileText("\(totals.days) 天", "\(totals.days) days"))
                                                .font(.system(size: 14))
                                        }
                                    }.frame(maxWidth: .infinity, alignment: .leading)
                                    MobileIcon("next", size: 18)
                                }
                                .padding(20).contentShape(Rectangle())
                            }.buttonStyle(MobileInkButtonStyle()).disabled(library.core == nil)
                        }.padding(.horizontal, 16).padding(.top, 16).padding(.bottom, 24)
                    }
                    .task(id: library.syncRevision) {
                        if let core = library.core { totals = try? await core.statistics() }
                    }
                } else { shelf }
            }.frame(maxWidth: .infinity, maxHeight: .infinity)

            HStack(spacing: 0) {
                tab(mobileText("书架", "Library"), icon: "library", selected: !profile) { profile = false }
                tab(mobileText("我", "Me"), icon: "person", selected: profile) { profile = true }
            }
            .frame(height: 56)
            .background(palette.surface.ignoresSafeArea(edges: .bottom))
            .overlay(alignment: .top) { Rectangle().fill(palette.outline).frame(height: 0.5) }
        }
        .foregroundStyle(palette.ink)
        .background(palette.background.ignoresSafeArea())
        .task {
            await library.refresh()
            if let core = library.core { updates.checkIfNeeded(core) }
            await library.downloadFonts()
        }
        .onChange(of: phase, initial: true) { _, phase in library.cloud.setScene(phase) }
        .fileImporter(isPresented: $importing,
                      allowedContentTypes: LibraryModel.extensions.sorted().compactMap { UTType(filenameExtension: $0) },
                      allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls): Task { await library.importBooks(urls) }
            case .failure(let error): library.error = message(error)
            }
        }
        .fullScreenCover(item: $selected, onDismiss: { Task { await library.refresh() } }) { book in
            if let core = library.core { ReaderScreen(core: core, book: book, library: library) }
        }
        .fullScreenCover(isPresented: $settings) {
            if let core = library.core { MobileSettings(core: core, library: library) }
        }
        .fullScreenCover(isPresented: $statistics, onDismiss: openDeferredBook) {
            if let core = library.core {
                ReadingStatisticsSheet(core: core, library: library) { openAfterDetails = $0 }
            }
        }
        .fullScreenCover(isPresented: $feedback) {
            if let core = library.core { FeedbackScreen(core: core) }
        }
        .fullScreenCover(item: $details, onDismiss: openDeferredBook) { book in
            if let core = library.core {
                BookDetailsScreen(core: core, id: book.id, book: book, coverRevision: library.coverRevision) {
                    openAfterDetails = book
                    details = nil
                }
            }
        }
        .alert(mobileText("删除书籍？", "Delete book?"), isPresented: Binding(get: { remove != nil }, set: { if !$0 { remove = nil } })) {
            Button(mobileText("删除", "Delete"), role: .destructive) {
                if let book = remove { Task { await library.removeBook(book) } }
                remove = nil
            }
            Button(mobileText("取消", "Cancel"), role: .cancel) { remove = nil }
        } message: { Text(remove?.title ?? "") }
        .alert("Torto", isPresented: Binding(get: { library.error != nil }, set: { if !$0 { library.error = nil } })) {
            Button("OK", role: .cancel) { library.error = nil }
        } message: { Text(library.error ?? "") }
    }

    private func openDeferredBook() {
        if let openAfterDetails { selected = openAfterDetails; self.openAfterDetails = nil }
    }

    @ViewBuilder private var shelf: some View {
        if library.books.isEmpty {
            if library.working || library.core == nil && library.error == nil {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Text(mobileText("书架还是空的。\n点击 + 导入 EPUB、FB2、CBZ、\nMOBI、CHM 或 PDF。", "No books yet.\nTap + to import an EPUB, FB2, CBZ,\nMOBI, CHM or PDF."))
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(.horizontal, 24)
            }
        } else {
            GeometryReader { geometry in
                let columns = geometry.size.width >= 660 ? max(4, Int(geometry.size.width / 150)) : 3
                ScrollView {
                    LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 14), count: columns), spacing: 22) {
                        ForEach(library.books) { book in
                            Button { selected = book } label: {
                                BookTile(book: book, core: library.core, revision: library.coverRevision)
                            }
                            .buttonStyle(.plain).accessibilityIdentifier("book-\(book.id)")
                            .contextMenu {
                                Button(mobileText("阅读详情", "Reading details")) { details = book }
                                Button(mobileText("删除书籍", "Delete book"), role: .destructive) { remove = book }
                            }
                        }
                    }.padding(.horizontal, 18).padding(.top, 20).padding(.bottom, 32)
                }
            }
        }
    }

    private func tab(_ label: String, icon: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            let ink = selected ? palette.active : palette.icon
            VStack(spacing: 2) {
                MobileIcon(icon, tint: ink).frame(width: 44)
                Text(label).font(.system(size: 11)).foregroundStyle(ink)
            }.frame(maxWidth: .infinity, maxHeight: .infinity).contentShape(Rectangle())
        }.buttonStyle(MobileInkButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

extension BookSummary: Identifiable {}

struct BookCover: View {
    let id: String
    let core: MobileCore?
    var revision = 0
    var placeholderSize: CGFloat = 40
    @State private var cover: UIImage?
    private let covers = CoverCache.shared
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Rectangle().fill(MobilePalette(scheme).cover)
            .aspectRatio(0.7, contentMode: .fit)
            .overlay {
                if let cover {
                    GeometryReader { geometry in
                        Image(uiImage: cover).resizable().scaledToFill()
                            .frame(width: geometry.size.width, height: geometry.size.height).clipped()
                    }
                } else { MobileIcon("book", size: placeholderSize) }
            }
            .clipShape(.rect(cornerRadius: 6))
            .shadow(color: .black.opacity(0.12), radius: 3, y: 1)
            .accessibilityHidden(true)
            .task(id: "\(id):\(revision)") {
                guard let core else { return }
                let key = "\(id):\(revision)" as NSString
                if let cached = covers.images.object(forKey: key) {
                    cover = cached
                    return
                }
                guard let bytes = try? await core.cover(id: id), !Task.isCancelled else { return }
                let image = await Task.detached(priority: .utility) {
                    UIImage(data: bytes)?.preparingForDisplay()
                }.value
                guard !Task.isCancelled, let image else { return }
                covers.images.setObject(image, forKey: key, cost: (image.cgImage?.bytesPerRow ?? 0) * (image.cgImage?.height ?? 0))
                cover = image
            }
    }
}

private struct BookTile: View {
    let book: BookSummary
    let core: MobileCore?
    let revision: Int
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            BookCover(id: book.id, core: core, revision: revision)
            Text(book.title).font(.system(size: 12, weight: .medium)).lineLimit(1)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}


@MainActor
private final class CoverCache {
    static let shared = CoverCache()
    let images = NSCache<NSString, UIImage>()

    private init() {
        images.totalCostLimit = 12 * 1024 * 1024
        images.countLimit = 120
    }
}
