import Charts
import Foundation
import SwiftUI

struct Sample: Equatable {
    /// nil on the first sample of a stream, which has no previous CPU reading.
    var cpuPercent: Double?
    var memoryUsed: UInt64 = 0
    var memoryLimit: UInt64 = 0
    var netRx: UInt64 = 0
    var netTx: UInt64 = 0
    var blockRead: UInt64 = 0
    var blockWrite: UInt64 = 0

    /// Same formulas as `docker stats`: CPU over the system delta times online CPUs,
    /// memory without the inactive page cache.
    init(_ stats: StatsResponse) {
        let cpu = stats.cpu_stats
        let precpu = stats.precpu_stats
        let cpuDelta = Double(cpu?.cpu_usage?.total_usage ?? 0) - Double(precpu?.cpu_usage?.total_usage ?? 0)
        let systemDelta = Double(cpu?.system_cpu_usage ?? 0) - Double(precpu?.system_cpu_usage ?? 0)
        if precpu?.system_cpu_usage != nil, systemDelta > 0 {
            cpuPercent = max(0, cpuDelta) / systemDelta * Double(cpu?.online_cpus ?? 1) * 100
        }

        let memory = stats.memory_stats
        let inactive = memory?.stats?["inactive_file"] ?? memory?.stats?["total_inactive_file"] ?? 0
        let usage = memory?.usage ?? 0
        memoryUsed = usage > inactive ? usage - inactive : 0
        memoryLimit = memory?.limit ?? 0

        for network in (stats.networks ?? [:]).values {
            netRx += network.rx_bytes ?? 0
            netTx += network.tx_bytes ?? 0
        }
        for entry in stats.blkio_stats?.io_service_bytes_recursive ?? [] {
            switch entry.op?.lowercased() {
            case "read": blockRead += entry.value ?? 0
            case "write": blockWrite += entry.value ?? 0
            default: break
            }
        }
    }
}

/// One `/containers/{id}/stats` line, named as Docker names its JSON fields.
struct StatsResponse: Decodable {
    struct CPU: Decodable {
        struct Usage: Decodable { var total_usage: UInt64? }
        var cpu_usage: Usage?
        var system_cpu_usage: UInt64?
        var online_cpus: UInt64?
    }
    struct Memory: Decodable {
        var usage: UInt64?
        var limit: UInt64?
        var stats: [String: UInt64]?
    }
    struct NetworkIO: Decodable {
        var rx_bytes: UInt64?
        var tx_bytes: UInt64?
    }
    struct Blkio: Decodable {
        struct Entry: Decodable {
            var op: String?
            var value: UInt64?
        }
        var io_service_bytes_recursive: [Entry]?
    }
    var cpu_stats: CPU?
    var precpu_stats: CPU?
    var memory_stats: Memory?
    var networks: [String: NetworkIO]?
    var blkio_stats: Blkio?
}

/// Last minute of samples, one per second, for the Stats pane.
struct StatsHistory {
    static let capacity = 60
    private(set) var samples: [Sample] = []

    mutating func push(_ sample: Sample) {
        samples.append(sample)
        if samples.count > Self.capacity {
            samples.removeFirst(samples.count - Self.capacity)
        }
    }

    mutating func clear() {
        samples.removeAll()
    }
}

func formatBytes(_ bytes: UInt64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"]
    var value = Double(bytes)
    var unit = 0
    while value >= 1024, unit < units.count - 1 {
        value /= 1024
        unit += 1
    }
    return unit == 0 ? "\(bytes) B" : String(format: "%.1f %@", value, units[unit])
}

struct StatsView: View {
    let samples: [Sample]

    var body: some View {
        let latest = samples.last
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                readout("CPU", latest?.cpuPercent.map { String(format: "%.1f %%", $0) })
                readout("Memory", latest.map { formatBytes($0.memoryUsed) }, detail: latest.map { "of \(formatBytes($0.memoryLimit))" })
                readout("Network", latest.map { "\(formatBytes($0.netRx)) in" }, detail: latest.map { "\(formatBytes($0.netTx)) out" })
                readout("Disk", latest.map { "\(formatBytes($0.blockRead)) read" }, detail: latest.map { "\(formatBytes($0.blockWrite)) written" })
            }
            chart("CPU", color: .blue, minimumTop: 1, value: { $0.cpuPercent }, label: { String(format: "%.1f %%", $0) })
            chart("Memory", color: .green, minimumTop: 1024 * 1024, value: { Double($0.memoryUsed) }, label: { formatBytes(UInt64(max(0, $0))) })
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func readout(_ title: String, _ value: String?, detail: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption.weight(.medium)).foregroundStyle(.secondary)
            Text(value ?? "-").font(.title3.weight(.semibold).monospacedDigit())
            Text(detail ?? " ").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(.quinary, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    /// X is seconds ago, so the newest sample sits at 0 on the right.
    /// An explicit Y domain keeps the axis visible before the first sample arrives.
    private func chart(_ title: String, color: Color, minimumTop: Double, value: @escaping (Sample) -> Double?, label: @escaping (Double) -> String) -> some View {
        let top = max(minimumTop, samples.compactMap(value).max() ?? 0) * 1.1
        return VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            Chart(Array(samples.enumerated()), id: \.offset) { index, sample in
                if let y = value(sample) {
                    AreaMark(x: .value("Seconds", index - samples.count + 1), y: .value(title, y))
                        .interpolationMethod(.monotone)
                        .foregroundStyle(LinearGradient(colors: [color.opacity(0.35), color.opacity(0.02)], startPoint: .top, endPoint: .bottom))
                    LineMark(x: .value("Seconds", index - samples.count + 1), y: .value(title, y))
                        .interpolationMethod(.monotone)
                        .lineStyle(StrokeStyle(lineWidth: 1.5))
                        .foregroundStyle(color)
                }
            }
            .chartXScale(domain: -(StatsHistory.capacity - 1)...0)
            .chartYScale(domain: 0...top)
            .chartXAxis {
                AxisMarks(values: [-59, -45, -30, -15, 0]) { value in
                    AxisGridLine()
                    AxisValueLabel("\(value.as(Int.self) ?? 0)s")
                }
            }
            // Fixed label width keeps the CPU and Memory plot areas aligned.
            .chartYAxis {
                AxisMarks { value in
                    AxisGridLine()
                    AxisValueLabel {
                        Text(label(value.as(Double.self) ?? 0)).frame(width: 64, alignment: .leading)
                    }
                }
            }
        }
        .padding(12)
        .background(.quinary, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}
