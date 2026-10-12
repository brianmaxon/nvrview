import Foundation

/// Makes a finished MP4 share-safe in place (FFI-RECORDINGS.md section 7a
/// (d)): the creation and modification times of the movie, tracks, and
/// media become zero (the 1904 "unknown" epoch the core's own files carry),
/// the handler names become the generic `VideoHandler` and `SoundHandler`
/// (what AVFoundation writes instead names its framework), and user data
/// and metadata boxes become free space. Nothing moves: every box keeps its
/// size, so offsets in the file stay right.
public enum MP4ShareSafe {
    public enum Failure: Error, Equatable {
        case unreadable
        case noMovieBox
    }

    /// Boxes that only hold other boxes we look into.
    private static let containers: Set<String> = ["moov", "trak", "mdia"]
    /// Boxes that may carry the app's or the machine's own metadata.
    private static let metadata: Set<String> = ["udta", "meta"]

    /// Patches the file at `url`.
    public static func apply(to url: URL) throws(Failure) {
        guard let handle = try? FileHandle(forUpdating: url) else { throw .unreadable }
        defer { try? handle.close() }
        do {
            var offset: UInt64 = 0
            let size = try handle.seekToEnd()
            while offset + 8 <= size {
                try handle.seek(toOffset: offset)
                guard let head = try handle.read(upToCount: 16), head.count >= 8 else { break }
                var boxSize = UInt64(head.u32(0))
                let type = head.fourCC(4)
                var header: UInt64 = 8
                if boxSize == 1 {
                    guard head.count >= 16 else { break }
                    boxSize = head.u64(8)
                    header = 16
                } else if boxSize == 0 {
                    boxSize = size - offset
                }
                guard boxSize >= header, offset + boxSize <= size else { break }
                if type == "moov" {
                    try handle.seek(toOffset: offset)
                    guard var moov = try handle.read(upToCount: Int(boxSize)), moov.count == Int(boxSize) else { throw Failure.unreadable }
                    patch(&moov, from: Int(header), to: moov.count)
                    try handle.seek(toOffset: offset)
                    try handle.write(contentsOf: moov)
                    return
                }
                offset += boxSize
            }
        } catch {
            throw .unreadable
        }
        throw .noMovieBox
    }

    /// Patches the boxes in `data[from..<to]`; offsets are within `data`.
    static func patch(_ data: inout Data, from: Int, to: Int) {
        var at = from
        while at + 8 <= to {
            let size = Int(data.u32(at))
            let type = data.fourCC(at + 4)
            guard size >= 8, at + size <= to else { return }
            switch type {
            case "mvhd", "tkhd", "mdhd":
                zeroTimes(&data, at: at + 8)
            case "hdlr":
                renameHandler(&data, box: at, size: size)
            default:
                if containers.contains(type) {
                    patch(&data, from: at + 8, to: at + size)
                } else if metadata.contains(type) {
                    data.replaceSubrange((at + 4)..<(at + 8), with: Array("free".utf8))
                }
            }
            at += size
        }
    }

    /// `body` is the box's payload: version and flags, then the times.
    private static func zeroTimes(_ data: inout Data, at body: Int) {
        guard body + 4 <= data.count else { return }
        let version = data[data.startIndex + body]
        let length = version == 1 ? 16 : 8
        guard body + 4 + length <= data.count else { return }
        for i in 0..<length { data[data.startIndex + body + 4 + i] = 0 }
    }

    /// A `hdlr` box: version and flags (4), predefined (4), the handler type
    /// (4), reserved (12), then the name as a C string.
    private static func renameHandler(_ data: inout Data, box: Int, size: Int) {
        let kind = box + 8 + 8
        let nameStart = box + 8 + 4 + 4 + 4 + 12
        guard nameStart <= box + size, kind + 4 <= data.count else { return }
        let generic: String
        switch data.fourCC(kind) {
        case "vide": generic = "VideoHandler"
        case "soun": generic = "SoundHandler"
        default: return
        }
        let room = box + size - nameStart
        let bytes = Array(generic.utf8)
        guard room >= bytes.count + 1 else { return }
        for i in 0..<room { data[data.startIndex + nameStart + i] = i < bytes.count ? bytes[i] : 0 }
    }
}

extension Data {
    fileprivate func u32(_ i: Int) -> UInt32 {
        (0..<4).reduce(0) { $0 << 8 | UInt32(self[startIndex + i + $1]) }
    }

    fileprivate func u64(_ i: Int) -> UInt64 {
        (0..<8).reduce(0) { $0 << 8 | UInt64(self[startIndex + i + $1]) }
    }

    fileprivate func fourCC(_ i: Int) -> String {
        String(decoding: self[(startIndex + i)..<(startIndex + i + 4)], as: UTF8.self)
    }
}
