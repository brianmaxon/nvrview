import Darwin
import Foundation

/// For the self-test: passes everything the process writes to standard
/// error through unchanged and counts lines that contain one of `patterns`
/// (AppKit's warnings, such as a table view's reentrancy warning, arrive
/// there through NSLog and are otherwise invisible to the app).
final class LogWatch: @unchecked Sendable {
    let patterns: [String]
    private let lock = NSLock()
    private var counts: [String: Int] = [:]
    private var tail = Data()
    private let original: Int32
    private let readEnd: Int32

    /// Starts watching; nil if the pipe could not be set up.
    init?(patterns: [String]) {
        var fds: [Int32] = [0, 0]
        guard pipe(&fds) == 0 else { return nil }
        let saved = dup(STDERR_FILENO)
        guard saved >= 0, dup2(fds[1], STDERR_FILENO) >= 0 else {
            close(fds[0]); close(fds[1])
            return nil
        }
        close(fds[1])
        self.patterns = patterns
        original = saved
        readEnd = fds[0]
        let thread = Thread { [self] in pump() }
        thread.name = "LogWatch"
        thread.start()
    }

    /// How many lines so far contained `pattern`.
    func count(_ pattern: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return counts[pattern, default: 0]
    }

    private func pump() {
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let n = read(readEnd, &buffer, buffer.count)
            if n <= 0 { return }
            buffer.withUnsafeBytes { raw in
                var offset = 0
                while offset < n {
                    let w = write(original, raw.baseAddress! + offset, n - offset)
                    if w <= 0 { break }
                    offset += w
                }
            }
            scan(Data(buffer[0..<n]))
        }
    }

    private func scan(_ chunk: Data) {
        lock.lock()
        defer { lock.unlock() }
        tail.append(chunk)
        while let newline = tail.firstIndex(of: UInt8(ascii: "\n")) {
            let line = String(decoding: tail[tail.startIndex..<newline], as: UTF8.self)
            tail = Data(tail[(newline + 1)...])
            for p in patterns where line.contains(p) { counts[p, default: 0] += 1 }
        }
        // A line without an end is kept only up to a bound.
        if tail.count > 16_384 { tail = Data(tail.suffix(4096)) }
    }
}
