import SwiftUI
import Charts

private enum ReadingPeriod: String, CaseIterable, Identifiable {
    case week, month, year, all
    var id: String { rawValue }
    var label: String {
        switch self {
        case .week: mobileText("周", "Week")
        case .month: mobileText("月", "Month")
        case .year: mobileText("年", "Year")
        case .all: mobileText("总", "All")
        }
    }
    var current: String {
        switch self {
        case .week: mobileText("本周", "This week")
        case .month: mobileText("本月", "This month")
        case .year: mobileText("今年", "This year")
        case .all: label
        }
    }

    func range(_ stats: StatisticsPeriod) -> String {
        let start = stats.start, end = stats.end
        let year = String(start.prefix(4))
        let todayYear = Calendar(identifier: .gregorian).component(.year, from: Date())
        switch self {
        case .year: return year
        case .month: return displayDay(String(start.prefix(7)))
        case .week:
            let left = year == String(todayYear) ? String(start.suffix(5)) : start
            let right = end.hasPrefix(year) ? String(end.suffix(5)) : end
            return displayDay(left) + mobileText(" 至 ", " – ") + displayDay(right)
        case .all: return ""
        }
    }

    func chartLabel(_ day: String) -> String {
        switch self {
        case .week:
            guard let date = try? Date(day, strategy: .iso8601.year().month().day().dateSeparator(.dash)) else { return "" }
            let formatter = DateFormatter()
            formatter.locale = Locale(identifier: mobileText("zh_CN", "en_US"))
            formatter.timeZone = .gmt
            formatter.dateFormat = "EEE"
            return formatter.string(from: date)
        case .month:
            let number = Int(day.suffix(2)) ?? 0
            return number == 1 || number > 0 && number.isMultiple(of: 5) ? String(number) : ""
        case .year: return Int(day.dropFirst(5).prefix(2)).map(String.init) ?? ""
        case .all: return String(day.prefix(4))
        }
    }

    func tooltipDate(_ day: String) -> String {
        displayDay(self == .year ? String(day.prefix(7)) : self == .all ? String(day.prefix(4)) : day)
    }
}

struct ReadingStatisticsSheet: View {
    let core: MobileCore
    @ObservedObject var library: LibraryModel
    let openBook: (BookSummary) -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @State private var query = Query()
    @State private var results: [Query: StatisticsPeriod] = [:]
    @State private var error: String?
    @State private var details: DetailsRoute?
    @State private var bookToOpen: BookSummary?
    private struct DetailsRoute: Identifiable { let id: String }

    private struct Query: Hashable {
        var period: ReadingPeriod = .week
        var offset: Int32 = 0
    }

    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(spacing: 0) {
            MobileHeader(title: mobileText("阅读统计", "Reading statistics")) { dismiss() }
            StatisticsScroll {
                periods.padding(.bottom, 8)
                if query.period != .all { periodControls(results[query]) }
                if let stats = results[query] { summary(stats) }
                if let error { Text(error).font(.footnote).foregroundStyle(.red) }
            }
        }
        .foregroundStyle(palette.ink).background(palette.background.ignoresSafeArea())
        .task(id: LoadKey(query: query, revision: library.syncRevision)) {
            let request = query
            error = nil
            do {
                let stats = try await core.statisticsPeriod(kind: request.period.rawValue, offset: request.offset)
                guard !Task.isCancelled, query == request else { return }
                if results.count >= 12, results[request] == nil, let key = results.keys.first { results.removeValue(forKey: key) }
                results[request] = stats
            } catch { if !Task.isCancelled, query == request { self.error = message(error) } }
        }
        .fullScreenCover(item: $details, onDismiss: {
            if let bookToOpen { openBook(bookToOpen); dismiss() }
        }) { row in
            let book = library.books.first { $0.id == row.id }
            BookDetailsScreen(core: core, id: row.id, book: book, coverRevision: library.coverRevision) {
                guard let book else { return }
                bookToOpen = book
                details = nil
            }
        }
    }

    private struct LoadKey: Hashable { let query: Query; let revision: Int }

    private var periods: some View {
        let palette = MobilePalette(scheme)
        return HStack(spacing: 0) {
            ForEach(ReadingPeriod.allCases) { period in
                let selected = period == query.period
                Button { query = Query(period: period) } label: {
                    Text(period.label).font(.system(size: 14, weight: .medium)).lineLimit(1)
                        .frame(maxWidth: .infinity, minHeight: 40)
                        .foregroundStyle(selected ? palette.ink : palette.secondary)
                        .background(selected ? palette.selection : Color.clear)
                        .overlay(alignment: .leading) {
                            if period != .week { Rectangle().fill(palette.controlOutline).frame(width: 1) }
                        }.contentShape(Rectangle())
                }.buttonStyle(MobileInkButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
            }
        }.clipShape(.capsule).overlay { Capsule().strokeBorder(palette.controlOutline, lineWidth: 1) }
            .frame(minHeight: 48)
            .accessibilityElement(children: .contain)
            .accessibilityLabel(mobileText("统计周期", "Statistics period"))
    }

    private func periodControls(_ stats: StatisticsPeriod?) -> some View {
        HStack(spacing: 0) {
            MobileIconButton(icon: "previous", label: mobileText("上一周期", "Previous period"), size: 20) { query.offset -= 1 }
                .disabled(query.offset <= -200)
            Text(stats.map(query.period.range) ?? " ")
                .font(.system(size: 14)).monospacedDigit().lineLimit(1).minimumScaleFactor(0.8)
                .frame(minWidth: 85)
                .accessibilityIdentifier("statistics-period-range")
            MobileIconButton(icon: "next", label: mobileText("下一周期", "Next period"), size: 20) { query.offset += 1 }
                .disabled(query.offset >= 0)
            if query.offset < 0 {
                Button(query.period.current) { query.offset = 0 }.font(.system(size: 14)).padding(.leading, 8)
            }
            Spacer(minLength: 0)
        }.frame(minHeight: 48).padding(.bottom, 8)
    }

    @ViewBuilder private func summary(_ stats: StatisticsPeriod) -> some View {
        let ranked = stats.books.filter { $0.milliseconds > 0 }
        let books = Dictionary(uniqueKeysWithValues: library.books.map { ($0.id, $0) })
        HStack(spacing: 12) {
            StatisticsCard { StatisticsMetric(mobileText("阅读时长", "Reading time"), parts: timeParts(stats.total)) }
            StatisticsCard { StatisticsMetric(mobileText("阅读", "Reading days"), parts: [(stats.days.formatted(), mobileText("天", stats.days == 1 ? "day" : "days"))]) }
        }.padding(.bottom, 12)
        HStack(spacing: 12) {
            StatisticsCard { StatisticsMetric(mobileText("读过", "Books read"), parts: [(ranked.count.formatted(), mobileText("本", ranked.count == 1 ? "book" : "books"))]) }
            StatisticsCard { StatisticsMetric(mobileText("读完", "Finished"), parts: [(stats.finished.formatted(), mobileText("本", stats.finished == 1 ? "book" : "books"))]) }
        }.padding(.bottom, 12)
        StatisticsCard {
            StatisticsHeading(mobileText("阅读时长分布", "Reading time distribution"))
            PeriodDistribution(days: stats.distribution, period: query.period)
                .padding(.top, 14).id(query)
        }.padding(.bottom, 12)
        Section {
            if ranked.isEmpty {
                StatisticsGroupRow(last: true) { Text(mobileText("这段时间还没有阅读记录", "No reading recorded in this period.")).font(.footnote) }
            }
            ForEach(Array(ranked.enumerated()), id: \.element.id) { index, row in
                Button { details = DetailsRoute(id: row.id) } label: {
                    StatisticsGroupRow(last: index == ranked.count - 1, vertical: 10) {
                        HStack(spacing: 14) {
                            StatisticsCover(id: row.id, core: core, revision: library.coverRevision).frame(width: 44)
                            VStack(alignment: .leading, spacing: 5) {
                                StatisticsNumber(parts: timeParts(row.milliseconds), size: 20)
                                Text(row.title.isEmpty ? (books[row.id]?.title ?? mobileText("未知书籍", "Unknown book")) : row.title)
                                    .font(.footnote).lineLimit(1).foregroundStyle(MobilePalette(scheme).secondary)
                            }.frame(maxWidth: .infinity, alignment: .leading)
                            MobileIcon("next", size: 16)
                        }
                    }
                }.buttonStyle(MobileInkButtonStyle()).accessibilityIdentifier("statistics-book-" + row.id)
            }
        } header: {
            StatisticsGroupRow(first: true) {
                StatisticsHeading(mobileText("阅读最久", "Most time spent reading"))
            }
        }
    }
}

struct BookDetailsScreen: View {
    let core: MobileCore
    let id: String
    let book: BookSummary?
    var coverRevision = 0
    let read: () -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @State private var details: BookStatistics?
    @State private var error: String?
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        VStack(spacing: 0) {
            MobileHeader(title: mobileText("阅读详情", "Reading details")) { dismiss() }
            StatisticsScroll {
                if book != nil {
                    Button(action: read) { identity }.buttonStyle(MobileInkButtonStyle()).padding(.bottom, 12)
                        .accessibilityIdentifier("reading-details-open-book")
                } else { identity.padding(.bottom, 12) }
                if let details { history(details) }
                if let error { Text(error).font(.footnote).foregroundStyle(.red) }
            }
        }
        .foregroundStyle(palette.ink).background(palette.background.ignoresSafeArea())
        .task(id: id) {
            do {
                let value = try await core.bookStatistics(id: id)
                if !Task.isCancelled { details = value }
            } catch { if !Task.isCancelled, book == nil { self.error = message(error) } }
        }
    }

    private var identity: some View {
        StatisticsCard {
            HStack(alignment: .top, spacing: 16) {
                StatisticsCover(id: id, core: core, revision: coverRevision).frame(width: 76)
                VStack(alignment: .leading, spacing: 6) {
                    Text(details?.title.nonEmpty ?? book?.title ?? mobileText("未知书籍", "Unknown book"))
                        .font(.system(size: 18, weight: .semibold)).lineLimit(2)
                    Text(details?.authors.nonEmpty ?? book?.authors.joined(separator: ", ") ?? "")
                        .font(.system(size: 14)).foregroundStyle(palette.secondary)
                    HStack(spacing: 10) {
                        Text(statusLabel).foregroundStyle(palette.accent)
                        if details?.status == "reading", let progress = details?.progress, progress.isFinite {
                            Text(min(1, max(0, progress)), format: .percent.precision(.fractionLength(1)))
                                .foregroundStyle(palette.secondary).monospacedDigit()
                        }
                    }.font(.system(size: 14)).padding(.top, 6)
                }.frame(maxWidth: .infinity, alignment: .leading)
                if book != nil { MobileIcon("next", size: 16).padding(.top, 4) }
            }
        }
    }

    @ViewBuilder private func history(_ stats: BookStatistics) -> some View {
        StatisticsCard {
            HStack(spacing: 12) {
                StatisticsMetric(mobileText("累计阅读", "Total reading"), parts: timeParts(stats.total))
                StatisticsMetric(mobileText("阅读天数", "Reading days"), parts: [(stats.days.formatted(), mobileText("天", stats.days == 1 ? "day" : "days"))])
            }
            Text(mobileText("\(stats.highlights) 处高亮 · \(stats.notes) 条批注", "\(stats.highlights) highlights · \(stats.notes) notes"))
                .font(.footnote).foregroundStyle(palette.secondary)
        }.padding(.bottom, 12)
        StatisticsCard(spacing: 0) {
            dateRow(mobileText("加入书架", "Added"), statisticsDate(stats.added))
            dateRow(mobileText("开始阅读", "Started"), statisticsDate(stats.started))
            dateRow(mobileText("最近阅读", "Last read"), statisticsDate(stats.last))
            dateRow(mobileText("读完日期", "Finished"), stats.finished.map(displayDay) ?? "—")
        }.padding(.bottom, 12)
        let days = stats.distribution.filter { $0.milliseconds >= 60_000 }.sorted { $0.day > $1.day }
        Section {
            if days.isEmpty {
                StatisticsGroupRow(last: true) { Text(mobileText("暂无满 1 分钟的阅读记录", "No days with at least one minute of reading.")).font(.footnote) }
            }
            ForEach(Array(days.enumerated()), id: \.element.day) { index, day in
                StatisticsGroupRow(last: index == days.count - 1) {
                    LabeledContent(displayDay(day.day)) {
                        HStack(spacing: 4) {
                            Text((day.milliseconds / 3_600_000).formatted()).frame(minWidth: 12, alignment: .trailing)
                            Text(mobileText("小时", "h"))
                            Text((day.milliseconds / 60_000 % 60).formatted()).frame(minWidth: 21, alignment: .trailing)
                            Text(mobileText("分", "min"))
                        }
                    }.font(.system(size: 14)).monospacedDigit()
                }
            }
        } header: {
            StatisticsGroupRow(first: true) {
                StatisticsHeading(mobileText("阅读历史", "Reading history"))
            }
        }
    }

    private var statusLabel: String {
        switch details?.status {
        case "reading": mobileText("在读", "Reading")
        case "finished": mobileText("已读完", "Finished")
        default: mobileText("未开始", "Not started")
        }
    }

    private func dateRow(_ label: String, _ value: String) -> some View {
        LabeledContent(label) { Text(value).monospacedDigit() }.font(.system(size: 14)).padding(.vertical, 7)
    }
}

private struct StatisticsScroll<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0, content: content)
                .frame(maxWidth: 680).padding(16).frame(maxWidth: .infinity)
        }
    }
}

private struct StatisticsCard<Content: View>: View {
    var spacing: CGFloat = 10
    @ViewBuilder var content: () -> Content
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        let palette = MobilePalette(scheme)
        VStack(alignment: .leading, spacing: spacing, content: content)
            .frame(maxWidth: .infinity, alignment: .leading).padding(16)
            .background(palette.surface, in: .rect(cornerRadius: 12))
            .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(palette.outline.opacity(0.6)) }
            .contentShape(.rect(cornerRadius: 12))
    }
}

private struct StatisticsGroupRow<Content: View>: View {
    var first = false
    var last = false
    var vertical: CGFloat = 12
    @ViewBuilder var content: () -> Content
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        let palette = MobilePalette(scheme)
        let shape = UnevenRoundedRectangle(topLeadingRadius: first ? 12 : 0, bottomLeadingRadius: last ? 12 : 0,
                                          bottomTrailingRadius: last ? 12 : 0, topTrailingRadius: first ? 12 : 0)
        content().frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16).padding(.top, first ? 16 : vertical)
            .padding(.bottom, first ? 0 : vertical + (last ? 16 : 0))
            .background(palette.surface, in: shape)
            .overlay {
                GeometryReader { proxy in
                    let bounds = CGRect(origin: .zero, size: proxy.size)
                    shape.inset(by: 0.5).stroke(palette.outline.opacity(0.6), lineWidth: 1)
                        .mask {
                            Path { path in
                                let cap = min(13, bounds.height)
                                path.addRect(CGRect(x: 0, y: 0, width: 1, height: bounds.height))
                                path.addRect(CGRect(x: bounds.width - 1, y: 0, width: 1, height: bounds.height))
                                if first { path.addRect(CGRect(x: 1, y: 0, width: max(0, bounds.width - 2), height: cap)) }
                                if last { path.addRect(CGRect(x: 1, y: bounds.height - cap, width: max(0, bounds.width - 2), height: cap)) }
                            }.fill()
                        }
                }.allowsHitTesting(false)
            }
            .contentShape(shape)
    }
}

private struct StatisticsHeading: View {
    let title: String
    @Environment(\.colorScheme) private var scheme
    init(_ title: String) { self.title = title }
    var body: some View {
        Text(title).font(.system(size: 14, weight: .medium)).foregroundStyle(MobilePalette(scheme).secondary)
            .accessibilityAddTraits(.isHeader)
    }
}

private struct StatisticsMetric: View {
    let label: String
    let parts: [(String, String)]
    @Environment(\.colorScheme) private var scheme
    init(_ label: String, parts: [(String, String)]) { self.label = label; self.parts = parts }
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).font(.system(size: 12)).foregroundStyle(MobilePalette(scheme).secondary)
            StatisticsNumber(parts: parts)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct StatisticsNumber: View {
    let parts: [(String, String)]
    var size: CGFloat = 28
    @Environment(\.colorScheme) private var scheme
    var body: some View {
        Text(parts.reduce(into: AttributedString()) { value, part in
            var number = AttributedString(part.0)
            number.font = .system(size: size, weight: .medium).monospacedDigit()
            var unit = AttributedString(" " + part.1)
            unit.font = .system(size: 12)
            unit.foregroundColor = MobilePalette(scheme).secondary
            if !value.characters.isEmpty { value += AttributedString(" ") }
            value += number + unit
        }).lineLimit(1).minimumScaleFactor(0.55)
    }
}

private struct PeriodDistribution: View {
    let days: [DayReading]
    let period: ReadingPeriod
    @State private var selection: String?
    @Environment(\.colorScheme) private var scheme
    private var maximum: UInt64 { days.map(\.milliseconds).max() ?? 0 }
    private var slotWidth: CGFloat { period == .all ? 60 : period == .year ? 27 : 10 }
    private var stepMinutes: Double {
        let requested = Double(maximum) / 60_000 * 1.08 / 3
        if let step = [1.0, 2, 5, 10, 15, 30, 60, 120, 180, 300, 600, 1200].first(where: { $0 >= requested }) { return step }
        let power = pow(10, floor(log10(requested)))
        return ([1.0, 2, 5, 10].first(where: { $0 * power >= requested }) ?? 10) * power
    }

    var body: some View {
        if maximum == 0 {
            Text(mobileText("暂无阅读记录", "No reading records")).font(.system(size: 12))
                .foregroundStyle(MobilePalette(scheme).secondary).frame(maxWidth: .infinity, minHeight: 190)
        } else {
            GeometryReader { geometry in
                let width = max(geometry.size.width, slotWidth * CGFloat(days.count) + 50)
                ScrollView(.horizontal) {
                    chart(width: width).padding(.top, 7).frame(width: width)
                }.scrollBounceBehavior(.basedOnSize)
            }.frame(height: 220)
            if let day = days.first(where: { $0.day == selection }) {
                Text(period.tooltipDate(day.day) + " · " + mobileDuration(day.milliseconds))
                    .font(.footnote).monospacedDigit().foregroundStyle(MobilePalette(scheme).secondary)
                    .accessibilityIdentifier("statistics-chart-value")
            }
        }
    }

    private func chart(width: CGFloat) -> some View {
        let palette = MobilePalette(scheme)
        let step = stepMinutes
        let barWidth = min(48, max(1, (width - 50) / CGFloat(max(days.count, 1)) * 0.58))
        return Chart(days, id: \.day) { day in
            BarMark(x: .value("Date", day.day), y: .value("Minutes", Double(day.milliseconds) / 60_000),
                    width: .fixed(barWidth))
                .foregroundStyle(selection == nil || selection == day.day ? palette.accent : palette.accent.opacity(0.4))
                .cornerRadius(3)
                .accessibilityLabel(period.tooltipDate(day.day)).accessibilityValue(mobileDuration(day.milliseconds))
        }
        .chartXScale(domain: days.map(\.day))
        .chartYScale(domain: 0...(step * 3))
        .chartXAxis {
            AxisMarks(values: days.map(\.day)) { value in
                if let date = value.as(String.self) {
                    AxisValueLabel { Text(period.chartLabel(date)).font(.system(size: 11)).foregroundStyle(palette.secondary) }
                }
            }
        }
        .chartYAxis {
            AxisMarks(position: .trailing, values: (0...3).map { Double($0) * step }) { value in
                AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [4, 4]))
                if let minutes = value.as(Double.self) {
                    let hours = step >= 60
                    AxisValueLabel {
                        Text(minutes == 0 ? "0" : (minutes / (hours ? 60 : 1)).formatted(.number.precision(.fractionLength(0...2))) + (hours ? "h" : "min"))
                            .font(.system(size: 11)).foregroundStyle(palette.secondary)
                    }
                }
            }
        }
        .chartGesture { proxy in
            SpatialTapGesture().onEnded { event in
                let point: String? = proxy.value(atX: event.location.x)
                selection = selection == point ? nil : point
            }
        }
    }
}

private struct StatisticsCover: View {
    let id: String
    let core: MobileCore
    let revision: Int
    var body: some View {
        BookCover(id: id, core: core, revision: revision, placeholderSize: 24)
    }
}

private func displayDay(_ iso: String) -> String { iso.replacingOccurrences(of: "-", with: "/") }

private func statisticsDate(_ milliseconds: UInt64?) -> String {
    guard let milliseconds, milliseconds > 0 else { return "—" }
    return Date(timeIntervalSince1970: Double(milliseconds) / 1_000).formatted(date: .numeric, time: .omitted)
}

private func timeParts(_ milliseconds: UInt64) -> [(String, String)] {
    let minutes = milliseconds / 60_000
    var parts: [(String, String)] = []
    if minutes >= 60 { parts.append(((minutes / 60).formatted(), mobileText("小时", "h"))) }
    if minutes < 60 || minutes % 60 != 0 {
        parts.append((milliseconds > 0 && minutes == 0 ? "<1" : (minutes % 60).formatted(), mobileText("分", "min")))
    }
    return parts
}

private extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}
