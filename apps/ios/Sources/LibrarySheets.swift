import SwiftUI

struct ReadingCompletionSummary: View {
    let statistics: BookStatistics?
    let failed: Bool
    let retry: () -> Void
    @Environment(\.colorScheme) private var scheme
    private var palette: MobilePalette { MobilePalette(scheme) }

    var body: some View {
        MobileGroup {
            VStack(alignment: .leading, spacing: 22) {
                HStack(alignment: .top, spacing: 16) {
                    metric(mobileText("累计时长", "Total reading time"), statistics.map { mobileDuration($0.total) },
                           caption: statistics?.started.map { dateCaption($0, started: true) })
                    metric(mobileText("阅读天数", "Reading days"), statistics.map { $0.days.formatted() },
                           caption: statistics?.last.map { dateCaption($0, started: false) })
                }
                HStack(alignment: .top, spacing: 16) {
                    metric(mobileText("想法笔记", "Notes and highlights"), statistics.map { $0.highlights.formatted() },
                           caption: statistics.map {
                        let highlights = $0.highlights >= $0.notes ? $0.highlights - $0.notes : 0
                        let highlightCount = highlights == 1 ? "1 highlight" : "\(highlights) highlights"
                        let commentCount = $0.notes == 1 ? "1 comment" : "\($0.notes) comments"
                        return mobileText("\(highlights) 处高亮 · \($0.notes) 条批注", "\(highlightCount) · \(commentCount)")
                    })
                    VStack(alignment: .leading, spacing: 8) {
                        label(mobileText("阅读明细", "Reading history"))
                        let recent = Array((statistics?.distribution ?? []).suffix(3).reversed())
                        let longest = recent.map(\.milliseconds).max() ?? 0
                        if longest > 0 {
                            ForEach(recent, id: \.day) { day in
                                GeometryReader { geometry in
                                    let fraction = CGFloat(Double(day.milliseconds) / Double(longest))
                                    RoundedRectangle(cornerRadius: 4).fill(palette.selection)
                                        .overlay(alignment: .leading) {
                                            RoundedRectangle(cornerRadius: 4).fill(palette.active.opacity(0.28))
                                                .frame(width: geometry.size.width * fraction)
                                        }.overlay(alignment: .leading) {
                                            Text(day.day).font(.system(size: 11)).lineLimit(1).padding(.horizontal, 5)
                                        }
                                }.frame(height: 18)
                                    .accessibilityLabel(day.day).accessibilityValue(mobileDuration(day.milliseconds))
                            }
                        } else { value(nil) }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                if failed {
                    label(mobileText("暂时无法加载统计", "Unable to load statistics"))
                    Button(mobileText("重新加载统计", "Reload statistics"), action: retry)
                }
            }.padding(18)
        }
    }

    private func label(_ title: String) -> some View {
        Text(title).font(.system(size: 13)).foregroundStyle(palette.secondary)
    }

    private func value(_ text: String?) -> some View {
        Text(text ?? "—").font(.system(size: 22, weight: .semibold)).minimumScaleFactor(0.75)
    }

    private func metric(_ title: String, _ text: String?, caption: String?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            label(title)
            value(text)
            if let caption { Text(caption).font(.system(size: 12)).foregroundStyle(palette.secondary) }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func dateCaption(_ milliseconds: UInt64, started: Bool) -> String {
        let date = Date(timeIntervalSince1970: Double(milliseconds) / 1_000).formatted(date: .abbreviated, time: .omitted)
        return started ? mobileText("\(date)开始阅读", "Started \(date)") : mobileText("最近阅读：\(date)", "Last read: \(date)")
    }
}
