import CryptoKit
import Foundation
import NVRCore
import Synchronization

// Updates (minor 2) in the fake core: no network and no files. A check finds
// `Options.updateOffer` (or nothing newer); a download "stages" the offer by
// reporting the offer's `stagedPath`, which a test prepares itself. Keys and
// sources are parsed the way the real core does, closely enough for the UI.

/// What the fake update source offers.
public struct FakeUpdateOffer: Sendable {
    public var version: String
    public var notes: String
    /// Reported as the `ready` path of a download; nil: the download fails
    /// (`staging`).
    public var stagedPath: String?

    public init(version: String, notes: String = "", stagedPath: String? = nil) {
        self.version = version
        self.notes = notes
        self.stagedPath = stagedPath
    }
}

final class FakeUpdateState: Sendable {
    let nextOp = Atomic<UInt64>(1)
}

extension FakeCore {
    public func updateCheck(json: String) throws(CoreStatus) {
        let request = try Self.updateRequest(json, download: false)
        let op = updates.nextOp.wrappingAdd(1, ordering: .relaxed).oldValue
        emit(event: #"{"type":"update","op":\#(op),"action":"check","state":"started"}"#)
        let current = request["current"] as? String ?? ""
        if (request["keys"] as? [String])?.isEmpty ?? true {
            emit(
                event:
                    #"{"type":"update","op":\#(op),"action":"check","state":"failed","reason":"no_trusted_keys","message":"no trusted key is installed"}"#
            )
        } else if let offer = options.updateOffer {
            emit(
                event:
                    #"{"type":"update","op":\#(op),"action":"check","state":"available","current":\#(Self.quote(current)),"version":\#(Self.quote(offer.version)),"notes":\#(Self.quote(offer.notes)),"size":1000,"key_id":"0011223344556677"}"#
            )
        } else {
            emit(
                event:
                    #"{"type":"update","op":\#(op),"action":"check","state":"up_to_date","current":\#(Self.quote(current)),"latest":\#(Self.quote(current))}"#
            )
        }
    }

    public func updateDownload(json: String) throws(CoreStatus) {
        let request = try Self.updateRequest(json, download: true)
        let op = updates.nextOp.wrappingAdd(1, ordering: .relaxed).oldValue
        emit(event: #"{"type":"update","op":\#(op),"action":"download","state":"started"}"#)
        guard let offer = options.updateOffer, offer.version == request["version"] as? String,
            let path = offer.stagedPath
        else {
            emit(
                event:
                    #"{"type":"update","op":\#(op),"action":"download","state":"failed","reason":"staging","message":"the fake core stages nothing"}"#
            )
            return
        }
        emit(event: #"{"type":"update","op":\#(op),"action":"download","state":"downloading","received":0,"total":1000}"#)
        emit(event: #"{"type":"update","op":\#(op),"action":"download","state":"downloading","received":1000,"total":1000}"#)
        emit(
            event:
                #"{"type":"update","op":\#(op),"action":"download","state":"ready","version":\#(Self.quote(offer.version)),"path":\#(Self.quote(path))}"#
        )
    }

    public func updateCancel() throws(CoreStatus) {}

    public func updateParseKey(_ text: String) -> String? {
        let lines = text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty && !$0.hasPrefix("#") }
        guard lines.count == 1 else { return nil }
        var hex = lines[0]
        if hex.hasPrefix("nvrview-ed25519-pub ") { hex = String(hex.dropFirst(20)).trimmingCharacters(in: .whitespaces) }
        hex = hex.lowercased()
        guard hex.count == 64, hex.allSatisfy(\.isHexDigit) else { return nil }
        var bytes = [UInt8]()
        var i = hex.startIndex
        while i < hex.endIndex {
            let j = hex.index(i, offsetBy: 2)
            bytes.append(UInt8(hex[i..<j], radix: 16) ?? 0)
            i = j
        }
        let id = SHA256.hash(data: bytes).prefix(8).map { String(format: "%02x", $0) }.joined()
        return #"{"key":"nvrview-ed25519-pub \#(hex)","id":"\#(id)"}"#
    }

    public func updateParseSource(_ text: String) -> String {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.lowercased().hasPrefix("https://") {
            return #"{"ok":true,"kind":"https","display":\#(Self.quote(t))}"#
        }
        if t.hasPrefix("/") {
            return #"{"ok":true,"kind":"folder","display":\#(Self.quote(t))}"#
        }
        if t.lowercased().hasPrefix("http://") {
            return
                #"{"ok":false,"reason":"insecure_source","message":"http:// is allowed only for hosts on the local network; use https://"}"#
        }
        return #"{"ok":false,"reason":"source_invalid","message":"not a folder or URL"}"#
    }

    /// The request as the real core checks it: an object with a source,
    /// keys, and a current version (and for a download, the version and
    /// staging folder).
    private static func updateRequest(_ json: String, download: Bool) throws(CoreStatus) -> [String: Any] {
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            o["source"] is String, o["keys"] is [String], o["current"] is String,
            !download || (o["version"] is String && o["staging_dir"] is String)
        else { throw .invalidArgument }
        return o
    }

    private static func quote(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s], options: [.withoutEscapingSlashes])) ?? Data("[\"\"]".utf8)
        let text = String(decoding: data, as: UTF8.self)
        return String(text.dropFirst().dropLast())
    }
}
