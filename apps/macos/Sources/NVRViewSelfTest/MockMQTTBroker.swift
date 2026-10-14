@testable import NVRViewApp
import Foundation
import Network
import Synchronization

/// A small stand-in for an MQTT 3.1.1 broker on a loopback port, for the
/// self-test: it accepts any login, answers a subscription, and publishes
/// what the test hands it to every subscribed client. Never a real broker.
final class MockMQTTBroker: Sendable {
    private struct State {
        var port: UInt16 = 0
        var connections: [ObjectIdentifier: NWConnection] = [:]
        var subscribed: Set<ObjectIdentifier> = []
        var connects = 0
    }

    private let state = Mutex(State())
    private let listener: NWListener
    private let queue = DispatchQueue(label: "nvrview.mock-mqtt")

    /// Starts listening on 127.0.0.1; nil when the port could not be opened.
    init?() {
        guard let listener = try? NWListener(using: .tcp, on: .any) else { return nil }
        self.listener = listener
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

    func stop() {
        listener.cancel()
        let all = state.withLock { s -> [NWConnection] in
            defer { s.connections = [:] }
            return Array(s.connections.values)
        }
        for c in all { c.cancel() }
    }

    var port: UInt16 { state.withLock { $0.port } }
    /// Clients that have subscribed.
    var subscribers: Int { state.withLock { $0.subscribed.count } }

    /// Sends a message (QoS 0) to every subscribed client.
    func publish(topic: String, payload: Data) {
        var body = Data()
        let t = Array(topic.utf8)
        body.append(UInt8(t.count >> 8))
        body.append(UInt8(t.count & 0xFF))
        body.append(contentsOf: t)
        body.append(payload)
        var packet = Data([0x30])
        packet.append(Self.length(body.count))
        packet.append(body)
        let targets = state.withLock { s in s.subscribed.compactMap { s.connections[$0] } }
        for c in targets { c.send(content: packet, completion: .contentProcessed { _ in }) }
    }

    private static func length(_ n: Int) -> Data {
        var out = Data()
        var x = n
        repeat {
            var byte = UInt8(x % 128)
            x /= 128
            if x > 0 { byte |= 0x80 }
            out.append(byte)
        } while x > 0
        return out
    }

    private func serve(_ connection: NWConnection) {
        let id = ObjectIdentifier(connection)
        state.withLock { $0.connections[id] = connection }
        connection.start(queue: queue)
        receive(connection, id: id, buffer: Data())
    }

    private func receive(_ connection: NWConnection, id: ObjectIdentifier, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            guard let self else { return }
            var buffer = buffer
            if let data { buffer.append(data) }
            while let (type, body, used) = Self.packet(in: buffer) {
                buffer.removeFirst(used)
                switch type {
                case 0x10:
                    state.withLock { $0.connects += 1 }
                    connection.send(content: Data([0x20, 0x02, 0x00, 0x00]), completion: .contentProcessed { _ in })
                case 0x82:
                    let pid = body.prefix(2)
                    connection.send(content: Data([0x90, 0x03]) + pid + Data([0x01]), completion: .contentProcessed { _ in })
                    _ = state.withLock { $0.subscribed.insert(id) }
                case 0xC0:
                    connection.send(content: Data([0xD0, 0x00]), completion: .contentProcessed { _ in })
                case 0xE0:
                    connection.cancel()
                default:
                    break
                }
            }
            if done || error != nil {
                state.withLock { s in
                    s.connections[id] = nil
                    s.subscribed.remove(id)
                }
                connection.cancel()
            } else {
                self.receive(connection, id: id, buffer: buffer)
            }
        }
    }

    /// The first whole packet in `data`: its first byte, its body, and how
    /// many bytes it took.
    private static func packet(in data: Data) -> (UInt8, Data, Int)? {
        guard data.count >= 2 else { return nil }
        let bytes = Array(data)
        var length = 0
        var multiplier = 1
        var i = 1
        while true {
            guard i < bytes.count else { return nil }
            let b = Int(bytes[i])
            length += (b & 0x7F) * multiplier
            multiplier *= 128
            i += 1
            if b & 0x80 == 0 { break }
            if i > 4 { return nil }
        }
        guard bytes.count >= i + length else { return nil }
        return (bytes[0] & 0xF0 == 0x80 ? 0x82 : bytes[0], Data(bytes[i..<(i + length)]), i + length)
    }
}
