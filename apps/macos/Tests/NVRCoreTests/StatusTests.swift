import NVRCore
import Testing

@Suite struct StatusTests {
    @Test func linesFollowTheEvents() {
        var s = ConnectionStatus()
        #expect(s.text == "Starting")
        s.apply(.deviceState(device: 1, state: .connecting))
        #expect(s.text == "Connecting...")
        s.apply(.deviceInfo(device: 1, name: "NVR", model: nil))
        s.apply(.deviceState(device: 1, state: .connected))
        #expect(s.text == "Connected to NVR")
        s.tile = .playing
        #expect(s.text == "Connected to NVR, playing")
        s.apply(.deviceState(device: 1, state: .backoff(attempt: 3, retryMilliseconds: 8000)))
        #expect(s.text == "Connection lost; attempt 3 failed, retrying in 8 s")
        #expect(!s.isTerminal)
        s.apply(.deviceState(device: 1, state: .loginFailed(status: 401, remainTimes: 2)))
        #expect(s.text == "Login failed (status 401); 2 attempts remaining before lockout. Not retrying; check the credentials.")
        #expect(s.isTerminal)
        s.apply(.deviceState(device: 1, state: .loginFailed(status: nil, remainTimes: 1)))
        #expect(s.text.contains("1 attempt remaining"))
        s.apply(.deviceState(device: 1, state: .closed))
        #expect(s.text == "Disconnected" && s.isTerminal)
    }
}
