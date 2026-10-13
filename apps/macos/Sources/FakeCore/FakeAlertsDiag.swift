import Foundation
import NVRCore

// Smart alerts, debugging and review (minor 14; docs/FFI.md "Smart alerts") in
// the fake core: a scripted message log per sensor, scripted connection
// states, a scripted result for "Send test alert", and an evidence store (no
// expiry: the real core's clock rules are tested there).

extension FakeCore {
    static func configuredInputs(_ configuration: String?) -> [String] {
        guard let configuration,
            let o = (try? JSONSerialization.jsonObject(with: Data(configuration.utf8))) as? [String: Any],
            let inputs = o["inputs"] as? [[String: Any]]
        else { return [] }
        return inputs.compactMap { $0["id"] as? String }
    }

    private static func diagJSON(_ object: [String: Any]) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    // MARK: Scripting (tests and the self-test)

    /// Adds a message to a sensor's log with its outcome.
    public func logSensorMessage(
        input: String, topic: String, payload: String, outcomeCode: String, outcomeText: String
    ) {
        alertsWorld.locked { w in
            let seq = w.sensorSeq[input] ?? 0
            w.sensorSeq[input] = seq + 1
            var list = w.sensorMessages[input] ?? []
            list.append([
                "seq": seq, "at_ms": Int(Date().timeIntervalSince1970 * 1000), "topic": topic, "payload": payload,
                "payload_len": payload.utf8.count, "truncated": false, "binary": false, "retained": false,
                "outcome": ["code": outcomeCode, "text": outcomeText],
            ])
            if list.count > 500 { list.removeFirst(list.count - 500) }
            w.sensorMessages[input] = list
        }
    }

    /// Sets what `nv_alerts_get` reports for a sensor's connection.
    public func setSensorConnection(input: String, status: [String: Any]) {
        alertsWorld.locked { $0.sensorStates[input] = status }
    }

    /// The result the next "send test alert" reports.
    public func setOutputTestResult(ok: Bool, httpStatus: Int?, summary: String) {
        alertsWorld.locked { $0.outputTestResult = (ok, httpStatus, summary) }
    }

    // MARK: The calls

    public func alertsMessages(request: String) throws(CoreStatus) -> String {
        checkOutsideCallback("nv_alerts_messages")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(request.utf8))) as? [String: Any],
            let input = o["input"] as? String
        else { throw .invalidArgument }
        let after = (o["after"] as? NSNumber)?.int64Value ?? 0
        let limit = min(max((o["limit"] as? NSNumber)?.intValue ?? 100, 1), 500)
        let clear = o["clear"] as? Bool ?? false
        return try alertsWorld.locked { w throws(CoreStatus) -> String in
            guard Self.configuredInputs(w.lastConfiguration).contains(input) else { throw .unknownID }
            if clear { w.sensorMessages[input] = [] }
            let all = (w.sensorMessages[input] ?? []).filter { (($0["seq"] as? Int64) ?? 0) >= after }
            let page = Array(all.prefix(limit))
            let next = (page.last?["seq"] as? Int64).map { $0 + 1 } ?? max(after, w.sensorSeq[input] ?? 0)
            return Self.diagJSON([
                "entries": page, "next_seq": next, "total": (w.sensorMessages[input] ?? []).count,
                "more": all.count > page.count,
            ])
        }
    }

    public func alertsOutputTest(request: String) throws(CoreStatus) {
        checkOutsideCallback("nv_alerts_output_test")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(request.utf8))) as? [String: Any],
            let index = (o["output"] as? NSNumber)?.intValue
        else { throw .invalidArgument }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard w.configured else { throw .notConfigured }
            guard let config = w.lastConfiguration,
                let doc = (try? JSONSerialization.jsonObject(with: Data(config.utf8))) as? [String: Any],
                let outputs = doc["outputs"] as? [[String: Any]], outputs.indices.contains(index)
            else { throw .unknownID }
            var e: [String: Any] = [
                "type": "alerts_output_test", "output": index, "kind": outputs[index]["kind"] ?? "desktop",
                "ok": w.outputTestResult.ok, "summary": w.outputTestResult.summary,
                "at_ms": Int(Date().timeIntervalSince1970 * 1000),
            ]
            if let s = w.outputTestResult.status { e["http_status"] = s }
            w.events.append(Self.diagJSON(e))
        }
        scheduleAlertWorkNow()
    }

    public func alertEvidencePut(json: String, data: Data?) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_evidence_put")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            let trigger = (o["trigger_id"] as? NSNumber)?.uint64Value
        else { throw .invalidArgument }
        let frame = (o["frame"] as? NSNumber)?.intValue
        let doc = o["doc"] as? [String: Any]
        guard frame != nil || doc != nil else { throw .invalidArgument }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard trigger > 0, trigger < w.nextTrigger else { throw .unknownID }
            var set = w.evidence[trigger] ?? FakeAlertsState.FakeEvidence()
            if let frame {
                guard let data, !data.isEmpty else { throw .invalidArgument }
                guard data.count <= 1 << 20 else { throw .limit }
                if set.frames[frame] == nil, set.frames.count >= 8 { throw .limit }
                set.frames[frame] = ((o["at_ms"] as? NSNumber)?.intValue ?? Int(Date().timeIntervalSince1970 * 1000), data)
            }
            if let doc { Self.deepMerge(&set.doc, doc) }
            w.evidence[trigger] = set
        }
    }

    private static func deepMerge(_ into: inout [String: Any], _ patch: [String: Any]) {
        for (k, v) in patch {
            if var a = into[k] as? [String: Any], let b = v as? [String: Any] {
                deepMerge(&a, b)
                into[k] = a
            } else {
                into[k] = v
            }
        }
    }

    private static func describe(_ id: UInt64, _ e: FakeAlertsState.FakeEvidence) -> [String: Any] {
        var v = e.doc
        v["trigger_id"] = id
        v["created_ms"] = e.created
        v["expires_ms"] = e.pinned ? e.created + 7 * 86_400_000 : e.created + 120 * 60_000
        v["pinned"] = e.pinned
        v["bytes"] = e.frames.values.reduce(0) { $0 + $1.data.count }
        v["frames"] = e.frames.keys.sorted().map { i -> [String: Any] in
            ["index": i, "at_ms": e.frames[i]!.at, "bytes": e.frames[i]!.data.count]
        }
        return v
    }

    public func alertEvidenceGet(query: String?) throws(CoreStatus) -> String {
        checkOutsideCallback("nv_alert_evidence_get")
        if isFreed { throw .closed }
        var q: [String: Any] = [:]
        if let query, !query.isEmpty {
            guard let o = (try? JSONSerialization.jsonObject(with: Data(query.utf8))) as? [String: Any] else {
                throw .invalidArgument
            }
            q = o
        }
        return try alertsWorld.locked { w throws(CoreStatus) -> String in
            if let t = (q["trigger_id"] as? NSNumber)?.uint64Value {
                guard let e = w.evidence[t] else { throw .unknownID }
                return Self.diagJSON(Self.describe(t, e))
            }
            let since = (q["since_ms"] as? NSNumber)?.intValue
            let sets = w.evidence.filter { since == nil || $0.value.created >= since! }
                .sorted { $0.value.created > $1.value.created || ($0.value.created == $1.value.created && $0.key > $1.key) }
                .map { Self.describe($0.key, $0.value) }
            return Self.diagJSON(["sets": sets, "retention_min": 120])
        }
    }

    public func alertEvidenceFrameGet(triggerID: UInt64, index: UInt32) throws(CoreStatus) -> Data {
        checkOutsideCallback("nv_alert_evidence_frame_get")
        if isFreed { throw .closed }
        return try alertsWorld.locked { w throws(CoreStatus) -> Data in
            guard let f = w.evidence[triggerID]?.frames[Int(index)] else { throw .unknownID }
            return f.data
        }
    }

    public func alertEvidencePin(triggerID: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_alert_evidence_pin")
        if isFreed { throw .closed }
        try alertsWorld.locked { w throws(CoreStatus) in
            guard w.evidence[triggerID] != nil else { throw .unknownID }
            w.evidence[triggerID]?.pinned = true
        }
    }

    /// Whether a simulated trigger was marked "also send to Home Assistant"
    /// (nil for a trigger that is not a test).
    public func testSentToHomeAssistant(trigger: UInt64) -> Bool? { alertsWorld.locked { $0.tests[trigger] } }

    func scheduleAlertWorkNow() {
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { [weak self] in
            self?.completeAlertWork()
        }
    }
}
