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
        s.apply(.deviceState(
            device: 1, state: .loginFailed(status: 407, remainTimes: 0, reason: "account locked", unlockSeconds: 301)))
        #expect(s.text == "Login failed: account locked (status 407); the account is locked, try again in 6 min. Not retrying.")
        s.apply(.deviceState(device: 1, state: .closed))
        #expect(s.text == "Disconnected" && s.isTerminal)
    }
}

@Suite struct ChannelFilterTests {
    private let channels = [
        ChannelInfo(channel: 0, connected: true), ChannelInfo(channel: 1, connected: false),
        ChannelInfo(channel: 2, connected: true), ChannelInfo(channel: 3),
    ]

    @Test func hidesOfflineChannelsOnly() {
        #expect(ChannelFilter.visible(channels, hideOffline: true).map(\.channel) == [0, 2])
        #expect(ChannelFilter.visible(channels, hideOffline: false).map(\.channel) == [0, 1, 2, 3])
        #expect(ChannelFilter.visible([], hideOffline: true).isEmpty)
    }

    @Test func counts() {
        #expect(ChannelFilter.countText(shown: 2, total: 4) == "2 of 4")
        #expect(ChannelFilter.countText(shown: 4, total: 4) == "4")
        #expect(ChannelFilter.countText(shown: 0, total: 0) == nil)
        #expect(ChannelFilter.countText(shown: 0, total: 3) == "0 of 3")
    }
}

@Suite struct FrameRateTextTests {
    @Test func words() {
        #expect(FrameRateText.make(device: 15, shown: 14.96, displayHz: 120) == "Device 15.0 fps · Shown 15.0 fps · 120 Hz")
        #expect(FrameRateText.make(device: nil, shown: nil, displayHz: 0) == "Device – fps · Shown – fps")
        #expect(
            FrameRateText.make(device: 15, shown: 15, displayHz: 60, switchNote: "switch +40 ms")
                == "Device 15.0 fps · Shown 15.0 fps · 60 Hz · switch +40 ms")
    }

    @Test func switchNotes() {
        #expect(TierSwitchTiming(deltaMs: 40, heldMs: 0, capped: false).note == "switch +40 ms")
        #expect(TierSwitchTiming(deltaMs: 0, heldMs: 0, capped: false).note == "switch +0 ms")
        #expect(TierSwitchTiming(deltaMs: 5, heldMs: 160, capped: false).note == "switch held 160 ms")
        #expect(TierSwitchTiming(deltaMs: -520, heldMs: 300, capped: true).note == "capped \u{2212}520 ms")
        #expect(TierSwitchTiming(deltaMs: nil, heldMs: 0, capped: false).note == "switch")
        #expect(TierSwitchTiming(deltaMs: nil, heldMs: 300, capped: true).note == "capped")
    }
}

@Suite struct LevelMeterModeTests {
    @Test func colorAndVisibility() {
        #expect(LevelMeterMode(hasCamera: false, audible: false) == .hidden)
        #expect(LevelMeterMode(hasCamera: false, audible: true) == .hidden)
        #expect(LevelMeterMode(hasCamera: true, audible: false) == .muted)
        #expect(LevelMeterMode(hasCamera: true, audible: true) == .audible)
        // Muted: only with the header (hover), and polled only then.
        #expect(LevelMeterMode.muted.visible(headerShown: true))
        #expect(!LevelMeterMode.muted.visible(headerShown: false))
        #expect(!LevelMeterMode.muted.polls(headerShown: false))
        // Audible: always, also once the header has faded.
        #expect(LevelMeterMode.audible.visible(headerShown: false))
        #expect(LevelMeterMode.audible.polls(headerShown: false))
        #expect(!LevelMeterMode.hidden.visible(headerShown: true))
        #expect(!LevelMeterMode.hidden.polls(headerShown: true))
    }
}
