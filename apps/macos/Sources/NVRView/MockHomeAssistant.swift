import Foundation
import Network
import Synchronization

/// A small stand-in for Home Assistant's REST API on a loopback port, for
/// the self-test: helper entities with a state, a bearer-token check, and a
/// log of the requests it served. Never a real server.
final class MockHomeAssistant: Sendable {
    private struct State {
        var entities: [String: String] = [:]
        var log: [String] = []
        var token = ""
        var port: UInt16 = 0
    }

    private let state = Mutex(State())
    private let listener: NWListener
    private let queue = DispatchQueue(label: "nvrview.mock-ha")

    /// Starts listening on 127.0.0.1; nil when the port could not be opened.
    init?(token: String, entities: [String: String]) {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
        state.withLock {
            $0.token = token
            $0.entities = entities
        }
        listener.newConnectionHandler = { [weak self] connection in self?.serve(connection) }
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { [weak self] s in
            if case .ready = s {
                self?.state.withLock { $0.port = listener.port?.rawValue ?? 0 }
                ready.signal()
            } else if case .failed = s {
                ready.signal()
            }
        }
        listener.start(queue: queue)
        _ = ready.wait(timeout: .now() + 5)
        if state.withLock({ $0.port }) == 0 { return nil }
    }

    func stop() { listener.cancel() }

    var url: String { "http://127.0.0.1:\(state.withLock { $0.port })" }
    var log: [String] { state.withLock { $0.log } }
    func entityState(_ id: String) -> String? { state.withLock { $0.entities[id] } }

    /// How many times a helper was turned on.
    func turnOns(_ id: String) -> Int {
        log.filter { $0 == "POST /api/services/input_boolean/turn_on \(id)" }.count
    }

    private func serve(_ connection: NWConnection) {
        connection.start(queue: queue)
        receive(connection, buffer: Data())
    }

    private func receive(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return connection.cancel() }
            var all = buffer
            if let data { all.append(data) }
            if let request = HTTPRequest(all) {
                let (status, body) = respond(to: request)
                let text = String(decoding: body, as: UTF8.self)
                let head =
                    "HTTP/1.1 \(status) X\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
                connection.send(content: Data((head + text).utf8), completion: .contentProcessed { _ in connection.cancel() })
            } else if done || error != nil {
                connection.cancel()
            } else {
                receive(connection, buffer: all)
            }
        }
    }

    private struct HTTPRequest {
        var method = ""
        var path = ""
        var authorization = ""
        var body = Data()

        /// Nil until the whole request has arrived.
        init?(_ data: Data) {
            guard let end = data.range(of: Data("\r\n\r\n".utf8)) else { return nil }
            let head = String(decoding: data[..<end.lowerBound], as: UTF8.self)
            var length = 0
            let lines = head.components(separatedBy: "\r\n")
            let first = lines[0].split(separator: " ")
            guard first.count >= 2 else { return nil }
            method = String(first[0])
            path = String(first[1])
            for line in lines.dropFirst() {
                let lower = line.lowercased()
                if lower.hasPrefix("content-length:") { length = Int(lower.dropFirst(15).trimmingCharacters(in: .whitespaces)) ?? 0 }
                if lower.hasPrefix("authorization:") { authorization = String(line.dropFirst(14)).trimmingCharacters(in: .whitespaces) }
            }
            let bodyStart = end.upperBound
            guard data.count - bodyStart >= length else { return nil }
            body = data.subdata(in: bodyStart..<(bodyStart + length))
        }
    }

    private func respond(to request: HTTPRequest) -> (Int, Data) {
        let entity =
            ((try? JSONSerialization.jsonObject(with: request.body)) as? [String: Any])?["entity_id"] as? String ?? ""
        func json(_ value: Any) -> Data { (try? JSONSerialization.data(withJSONObject: value)) ?? Data() }
        return state.withLock { s in
            let authed = request.authorization == "Bearer \(s.token)"
            let tag = request.method == "POST" && !entity.isEmpty ? "\(request.method) \(request.path) \(entity)" : "\(request.method) \(request.path)"
            s.log.append(authed ? tag : "\(tag) (bad token)")
            guard authed else { return (401, json(["message": "401: Unauthorized"])) }
            if request.path.hasPrefix("/api/states/") {
                let id = String(request.path.dropFirst("/api/states/".count))
                guard let value = s.entities[id] else { return (404, json(["message": "Entity not found."])) }
                return (200, json(["entity_id": id, "state": value]))
            }
            if request.path.hasSuffix("/turn_on") || request.path.hasSuffix("/turn_off"), s.entities[entity] != nil {
                let on = request.path.hasSuffix("/turn_on")
                s.entities[entity] = on ? "on" : "off"
                return (200, json([["entity_id": entity, "state": on ? "on" : "off"]]))
            }
            return (404, json([String: String]()))
        }
    }
}
