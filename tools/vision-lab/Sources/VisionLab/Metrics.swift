import Darwin
import Foundation
import IOKit

/// Monotonic wall clock in seconds.
public func now() -> Double {
    Double(clock_gettime_nsec_np(CLOCK_UPTIME_RAW)) / 1e9
}

/// This process's user + system CPU time in seconds.
public func processCPUSeconds() -> Double {
    var usage = rusage()
    getrusage(RUSAGE_SELF, &usage)
    func s(_ t: timeval) -> Double { Double(t.tv_sec) + Double(t.tv_usec) / 1e6 }
    return s(usage.ru_utime) + s(usage.ru_stime)
}

/// This process's physical footprint in bytes (what Activity Monitor calls
/// Memory), and its lifetime peak.
public func memoryFootprint() -> (current: UInt64, peak: UInt64) {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
    let kr = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    guard kr == KERN_SUCCESS else { return (0, 0) }
    return (info.phys_footprint, info.ledger_phys_footprint_peak > 0 ? UInt64(info.ledger_phys_footprint_peak) : info.phys_footprint)
}

/// The GPU's "Device Utilization %" from the IOAccelerator registry entry.
/// System-wide (the window server and other apps count too), readable
/// without privileges. Nil when the property is missing.
public func gpuUtilizationPercent() -> Int? {
    var iterator: io_iterator_t = 0
    guard IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOAccelerator"), &iterator)
        == KERN_SUCCESS
    else { return nil }
    defer { IOObjectRelease(iterator) }
    var result: Int?
    var entry = IOIteratorNext(iterator)
    while entry != 0 {
        if let prop = IORegistryEntryCreateCFProperty(entry, "PerformanceStatistics" as CFString, kCFAllocatorDefault, 0)?
            .takeRetainedValue() as? [String: Any],
            let util = prop["Device Utilization %"] as? Int
        {
            result = max(result ?? 0, util)
        }
        IOObjectRelease(entry)
        entry = IOIteratorNext(iterator)
    }
    return result
}

/// Samples GPU utilization on a background thread.
public final class GPUSampler: @unchecked Sendable {
    private let lock = NSLock()
    private var samples: [Int] = []
    private var running = true
    private var thread: Thread?

    public init(interval: TimeInterval = 0.25) {
        let t = Thread { [weak self] in
            while true {
                guard let self else { return }
                self.lock.lock()
                let go = self.running
                self.lock.unlock()
                if !go { return }
                if let u = gpuUtilizationPercent() {
                    self.lock.lock()
                    self.samples.append(u)
                    self.lock.unlock()
                }
                Thread.sleep(forTimeInterval: interval)
            }
        }
        t.qualityOfService = .utility
        thread = t
        t.start()
    }

    /// Stops sampling and returns (mean, max) utilization in percent.
    public func stop() -> (mean: Double, max: Int, count: Int) {
        lock.lock()
        running = false
        let s = samples
        lock.unlock()
        guard !s.isEmpty else { return (0, 0, 0) }
        return (Double(s.reduce(0, +)) / Double(s.count), s.max() ?? 0, s.count)
    }
}

/// CPU seconds used so far by other processes whose command name matches,
/// as `ps` reports them (no privileges needed). Used for the out-of-process
/// work: the hardware decoder's XPC service and the Neural Engine daemon.
public func otherProcessCPUSeconds(matching names: [String]) -> [String: Double] {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/bin/ps")
    p.arguments = ["-axo", "time=,comm="]
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = FileHandle.nullDevice
    do { try p.run() } catch { return [:] }
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    var out: [String: Double] = [:]
    for line in String(decoding: data, as: UTF8.self).split(separator: "\n") {
        let parts = line.trimmingCharacters(in: .whitespaces).split(separator: " ", maxSplits: 1)
        guard parts.count == 2 else { continue }
        let comm = String(parts[1])
        guard let name = names.first(where: { comm.hasSuffix("/" + $0) || comm == $0 }) else { continue }
        out[name, default: 0] += parsePSTime(String(parts[0]))
    }
    return out
}

/// Parses `ps` time (`[[dd-]hh:]mm:ss.cc`).
func parsePSTime(_ s: String) -> Double {
    var days = 0.0
    var rest = Substring(s)
    if let dash = rest.firstIndex(of: "-") {
        days = Double(rest[..<dash]) ?? 0
        rest = rest[rest.index(after: dash)...]
    }
    var total = 0.0
    for part in rest.split(separator: ":") {
        total = total * 60 + (Double(part) ?? 0)
    }
    return days * 86400 + total
}

/// Summary statistics of a list of samples.
public struct Summary: Codable, Sendable {
    public var count: Int
    public var mean: Double
    public var p50: Double
    public var p90: Double
    public var p99: Double
    public var max: Double

    public init(_ values: [Double]) {
        let v = values.sorted()
        count = v.count
        guard !v.isEmpty else {
            mean = 0; p50 = 0; p90 = 0; p99 = 0; max = 0
            return
        }
        func q(_ f: Double) -> Double { v[min(v.count - 1, Int((Double(v.count - 1) * f).rounded()))] }
        mean = v.reduce(0, +) / Double(v.count)
        p50 = q(0.5)
        p90 = q(0.9)
        p99 = q(0.99)
        max = v.last!
    }
}
