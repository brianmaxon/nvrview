import Foundation
import NVRCore

// What the alert filters learned (minor 17; docs/FFI.md "Smart alerts") in the
// fake core: layers scripted per camera by a test or the self-test, answered
// in the real core's shape, and a reset that forgets them. The real core's
// learning is tested in Rust.

/// One camera's scripted layers.
struct FakeInsights {
    var classes: [String]
    /// The `layers` array as JSON.
    var layers: String
    /// The kept evidence frame to draw on.
    var picture: (trigger: UInt64, frame: Int, atMs: Int64)?
}

extension FakeCore {
    /// Scripts a camera's insight layers (`layersJSON` is the JSON array the
    /// real core answers in `layers`).
    public func scriptInsights(
        camera: AlertCamera, classes: [String], layersJSON: String, picture: (trigger: UInt64, frame: Int, atMs: Int64)?
    ) {
        alertsWorld.locked { $0.insights[camera] = FakeInsights(classes: classes, layers: layersJSON, picture: picture) }
    }

    public func alertInsightsGet(query: String?) throws(CoreStatus) -> String {
        checkOutsideCallback("nv_alert_insights_get")
        if isFreed { throw .closed }
        var q: [String: Any] = [:]
        if let query, !query.isEmpty {
            guard let o = (try? JSONSerialization.jsonObject(with: Data(query.utf8))) as? [String: Any] else {
                throw .invalidArgument
            }
            q = o
        }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        return try alertsWorld.locked { w throws(CoreStatus) -> String in
            guard let c = q["camera"] else {
                let cams = w.insights.sorted { ($0.key.device, $0.key.channel, $0.key.lens) < ($1.key.device, $1.key.channel, $1.key.lens) }
                    .map { #"{"camera":\#($0.key.json),"classes":\#(Self.jsonArray($0.value.classes))}"# }
                return #"{"cameras":[\#(cams.joined(separator: ","))],"at_ms":\#(nowMs)}"#
            }
            guard let camera = Self.camera(c) else { throw .invalidArgument }
            let i = w.insights[camera]
            let picture = i?.picture.map { #"{"trigger_id":\#($0.trigger),"frame":\#($0.frame),"at_ms":\#($0.atMs)}"# } ?? "null"
            let layers = i?.layers ?? Self.thresholdsOnly
            return #"{"camera":\#(camera.json),"at_ms":\#(nowMs),"night":false,"layers":\#(layers),"picture":\#(picture)}"#
        }
    }

    public func alertInsightsReset(request: String) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_insights_reset")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(request.utf8))) as? [String: Any],
            let c = o["camera"], let camera = Self.camera(c)
        else { throw .invalidArgument }
        alertsWorld.locked { _ = $0.insights.removeValue(forKey: camera) }
    }

    private static func camera(_ any: Any) -> AlertCamera? {
        guard let o = any as? [String: Any], let d = (o["device"] as? NSNumber)?.uint64Value,
            let ch = (o["channel"] as? NSNumber)?.uint16Value
        else { return nil }
        return AlertCamera(device: d, channel: ch, lens: (o["lens"] as? NSNumber)?.uint8Value ?? 0)
    }

    private static func jsonArray(_ list: [String]) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: list)) ?? Data("[]".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    /// What a camera with no memory answers: the thresholds alone.
    private static let thresholdsOnly = """
        [{"id":"thresholds","title":"Thresholds","description":"","provenance":"","units":"","range":[0,1],"kind":"scalar",\
        "items":[{"id":"floor.day","label":"Score floor by day","value":0.5,"units":"score"}]}]
        """
}
