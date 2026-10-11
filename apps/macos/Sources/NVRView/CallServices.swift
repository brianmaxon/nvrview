import AppKit
import AVFoundation
import Foundation
import NVRCore
import Speech
import Synchronization

// What a doorbell call needs from the system (docs/UX.md "Doorbell calls"):
// the microphone, text-to-speech, and speech recognition. Each sits behind a
// small interface: the Apple implementations here, and fakes the self-test
// uses so an automated run never asks for the microphone or speech
// recognition.

/// The computer's microphone as 16 kHz mono float blocks.
@MainActor
protocol MicrophoneInput: AnyObject {
    /// Asks for the microphone (the system prompt, once). `done(false)` when
    /// refused or unavailable, with a reason for the call's status line.
    func requestAccess(_ done: @escaping @MainActor (_ granted: Bool, _ reason: String?) -> Void)
    /// Starts capturing; `onSamples` runs on an audio thread (not the
    /// real-time one) with 16 kHz mono samples. `onEcho` (any thread) says
    /// whether the system cancels the speakers' echo: once it is known
    /// (possibly before this returns), and again if that changes while
    /// capturing. Throws when no microphone could start at once.
    func start(
        _ onSamples: @escaping @Sendable ([Float]) -> Void, onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) throws
    func stop()
    /// A call opened (`true`) or the last one ended (`false`): the slow part
    /// of echo cancellation is prepared ahead of Talk, without opening the
    /// microphone, and only when its use is already allowed.
    func prepare(_ calls: Bool)
}

/// Text-to-speech for a typed reply, as 16 kHz mono float samples.
@MainActor
protocol SpeechRenderer: AnyObject {
    func render(_ text: String) async -> [Float]
}

/// Asks the user to allow talk-back for a camera (the confirmation of
/// `TalkPermission`); `answer(true)` is Allow. Never answers by itself.
@MainActor
protocol TalkPrompter: AnyObject {
    func ask(_ prompt: TalkPermission.Prompt, answer: @escaping @MainActor (Bool) -> Void)
}

/// Everything a call needs from the system.
@MainActor
struct CallServices {
    var makeTranscriber: @MainActor (TranscriptSource) -> any TranscriptionEngine
    var microphone: any MicrophoneInput
    var speech: any SpeechRenderer
    var prompter: any TalkPrompter = AlertTalkPrompter()

    /// The real ones. `player` is the app's audio output, whose engine the
    /// microphone joins for echo cancellation.
    static func system(player: @escaping @MainActor () -> AudioPlayer?) -> CallServices {
        CallServices(
            makeTranscriber: { _ in AppleTranscriber() }, microphone: AppleMicrophone(player: player),
            speech: AppleSpeechRenderer())
    }

    /// Fakes for the self-test and development: no permission prompt, no
    /// device; the transcriber turns fed audio into placeholder text.
    static func fakes() -> CallServices {
        CallServices(makeTranscriber: { _ in FakeTranscriber() }, microphone: FakeMicrophone(), speech: FakeSpeechRenderer(),
            prompter: FakeTalkPrompter())
    }
}

/// Whether the running app has a usage string for a protected resource: an
/// unbundled build (`swift run`) has none, and asking without one ends the
/// process, so the request is not made.
func hasUsageString(_ key: String) -> Bool {
    (Bundle.main.object(forInfoDictionaryKey: key) as? String)?.isEmpty == false
}

// MARK: Microphone

/// The input device through `AVAudioEngine`, converted to 16 kHz mono.
///
/// With echo cancellation (docs/AUDIO.md "Echo cancellation"): the
/// microphone runs on the app's audio output engine with the system's voice
/// processing (`AudioPlayer.startVoiceProcessing`), so the doorbell playing
/// through the speakers is removed from what is sent back. When that cannot
/// start, or stops working during the call, the microphone runs on an
/// engine of its own as before, and the call falls back to the caption gate.
@MainActor
final class AppleMicrophone: MicrophoneInput {
    private let player: @MainActor () -> AudioPlayer?
    private var engine: AVAudioEngine?
    private var voice: AudioPlayer?
    /// Bumped by every start and stop, so a late "voice processing lost"
    /// from an earlier capture changes nothing.
    private var generation = 0

    init(player: @escaping @MainActor () -> AudioPlayer?) { self.player = player }

    func requestAccess(_ done: @escaping @MainActor (Bool, String?) -> Void) {
        guard hasUsageString("NSMicrophoneUsageDescription") else {
            done(false, "the microphone needs the packaged app")
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: done(true, nil)
        case .denied, .restricted: done(false, "microphone access is off (System Settings > Privacy)")
        case .notDetermined:
            // Called on a system queue: not main-actor code (Swift 6 would
            // stop the app), it only hops to the main queue.
            AVCaptureDevice.requestAccess(for: .audio) { @Sendable granted in
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { done(granted, granted ? nil : "microphone access was refused") }
                }
            }
        @unknown default: done(false, "microphone unavailable")
        }
    }

    func start(
        _ onSamples: @escaping @Sendable ([Float]) -> Void, onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) throws {
        stop()
        generation += 1
        guard let player = player() else {
            try startOwnEngine(onSamples)
            onEcho(.decide(.failed("no audio output")))
            return
        }
        voice = player
        player.startVoiceProcessing(
            onSamples: onSamples, onLost: Self.lostHandler(self, generation: generation, onSamples, onEcho),
            done: Self.startedHandler(self, generation: generation, onSamples, onEcho))
    }

    func prepare(_ calls: Bool) {
        guard let player = player() else { return }
        // Building the engine for voice processing reads the input device's
        // format; not before the user allowed the microphone (no prompt).
        if calls, hasUsageString("NSMicrophoneUsageDescription"),
            AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
        {
            player.prepareVoiceProcessing()
        } else if !calls {
            player.discardVoiceProcessing()
        }
    }

    /// Voice processing started or not (built outside main-actor code: the
    /// player calls it on its own queue).
    private nonisolated static func startedHandler(
        _ mic: AppleMicrophone, generation: Int, _ onSamples: @escaping @Sendable ([Float]) -> Void,
        _ onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) -> @Sendable (VoiceProcessingError?) -> Void {
        { [weak mic] error in
            guard let error else {
                // Voice processing is echo cancellation by definition, with
                // the doorbell's output as its reference (the same engine).
                onEcho(.decide(EchoProbe(processing: true, effect: .on, referenceIsPlayback: true)))
                return
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let mic, mic.generation == generation else { return }
                    mic.fallBack(error.description, onSamples, onEcho)
                }
            }
        }
    }

    /// Voice processing stopped during the capture (built outside main-actor
    /// code: it is called on whatever thread saw the device change).
    private nonisolated static func lostHandler(
        _ mic: AppleMicrophone, generation: Int, _ onSamples: @escaping @Sendable ([Float]) -> Void,
        _ onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) -> @Sendable (String) -> Void {
        { [weak mic] why in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let mic, mic.generation == generation else { return }
                    mic.fallBack(why, onSamples, onEcho)
                }
            }
        }
    }

    /// Without voice processing: the microphone on an engine of its own, and
    /// the caption gate (logged by the call).
    private func fallBack(
        _ why: String, _ onSamples: @escaping @Sendable ([Float]) -> Void,
        _ onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) {
        voice = nil
        do {
            try startOwnEngine(onSamples)
            onEcho(.decide(.failed(why)))
        } catch {
            onEcho(.decide(.failed(why + "; the microphone could not start either")))
        }
    }

    /// The microphone on an engine of its own (no echo cancellation).
    private func startOwnEngine(_ onSamples: @escaping @Sendable ([Float]) -> Void) throws {
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw CocoaError(.featureUnsupported)
        }
        // The tap's block runs on an audio thread that is not the real-time
        // one, so converting and pushing to the core there is allowed. It is
        // built outside this main-actor type (TalkAudioConverter.tapBlock).
        input.installTap(onBus: 0, bufferSize: 1024, format: format, block: TalkAudioConverter.tapBlock(onSamples))
        engine.prepare()
        try engine.start()
        self.engine = engine
    }

    func stop() {
        generation += 1
        voice?.stopVoiceProcessing()
        voice = nil
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
        engine = nil
    }
}

// MARK: Text-to-speech

/// The system voice rendered to buffers (`write(_:toBufferCallback:)`), never
/// to the speakers, then converted to 16 kHz mono for talk-back.
@MainActor
final class AppleSpeechRenderer: SpeechRenderer {
    private let synthesizer = AVSpeechSynthesizer()

    func render(_ text: String) async -> [Float] {
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = AVSpeechSynthesisVoice(language: Locale.current.language.languageCode?.identifier ?? "en")
        let collected = SpeechBufferCollector()
        return await withCheckedContinuation { (done: CheckedContinuation<[Float], Never>) in
            // Resumes once, at the first end (a zero-length buffer); the
            // callback is built outside this main-actor type, since it may
            // run on any thread.
            synthesizer.write(utterance, toBufferCallback: collected.callback { done.resume(returning: $0) })
        }
    }
}

// MARK: Speech recognition

/// Apple's on-device recognizer (SpeechAnalyzer with SpeechTranscriber,
/// macOS 26): volatile and fast results, one session per source. Installs
/// the language's assets if needed (status `preparing`), and reports
/// `unavailable` instead of failing the call when it cannot.
final class AppleTranscriber: TranscriptionEngine, @unchecked Sendable {
    var onPartial: (@Sendable (String) -> Void)?
    var onFinal: (@Sendable (String) -> Void)?
    var onError: (@Sendable (String) -> Void)?
    var onStatus: (@Sendable (TranscriptionStatus) -> Void)?

    private let lock = NSLock()
    private var continuation: AsyncStream<AnalyzerInput>.Continuation?
    private var format: AVAudioFormat?
    private var converter: AVAudioConverter?
    private var task: Task<Void, Never>?
    private var finished = false

    func start(source: TranscriptSource) {
        let onStatus = onStatus, onError = onError, onPartial = onPartial, onFinal = onFinal
        task = Task { [weak self] in
            onStatus?(.preparing)
            guard await Self.authorized() else {
                onStatus?(.unavailable("speech recognition is off"))
                onError?("speech recognition is not authorized")
                return
            }
            guard SpeechTranscriber.isAvailable,
                let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: "en-US"))
            else {
                onStatus?(.unavailable("no English speech recognition on this Mac"))
                return
            }
            let transcriber = SpeechTranscriber(
                locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults, .fastResults],
                attributeOptions: [])
            do {
                if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                    try await request.downloadAndInstall()
                }
            } catch {
                onStatus?(.unavailable("speech recognition could not be prepared"))
                return
            }
            let analyzer = SpeechAnalyzer(modules: [transcriber])
            guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
                onStatus?(.unavailable("no audio format for speech recognition"))
                return
            }
            let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream(bufferingPolicy: .bufferingNewest(256))
            guard let self, self.install(continuation, format: format) else {
                continuation.finish()
                return
            }
            let results = Task {
                do {
                    for try await result in transcriber.results {
                        let text = String(result.text.characters)
                        if result.isFinal { onFinal?(text) } else { onPartial?(text) }
                    }
                } catch {
                    onError?("speech recognition stopped")
                }
            }
            do {
                try await analyzer.start(inputSequence: stream)
                onStatus?(.running)
                // Runs until `finish` ends the input.
                await self.waitForFinish()
                try await analyzer.finalizeAndFinishThroughEndOfInput()
            } catch {
                onStatus?(.unavailable("speech recognition stopped"))
            }
            _ = await results.result
            onStatus?(.finished)
        }
    }

    private static func authorized() async -> Bool {
        guard hasUsageString("NSSpeechRecognitionUsageDescription") else { return false }
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: return true
        case .denied, .restricted: return false
        case .notDetermined:
            return await withCheckedContinuation { done in
                SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0 == .authorized) }
            }
        @unknown default: return false
        }
    }

    private func install(_ c: AsyncStream<AnalyzerInput>.Continuation, format: AVAudioFormat) -> Bool {
        lock.withLock {
            guard !finished else { return false }
            continuation = c
            self.format = format
            let source = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
            converter = source == format ? nil : AVAudioConverter(from: source, to: format)
            return true
        }
    }

    private let finishSignal = Mutex<CheckedContinuation<Void, Never>?>(nil)
    private let finishedFlag = Mutex(false)

    private func waitForFinish() async {
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            let resumeNow = finishedFlag.withLock { done -> Bool in
                if done { return true }
                finishSignal.withLock { $0 = c }
                return false
            }
            if resumeNow { c.resume() }
        }
    }

    func feed(pcm16k: [Float]) {
        lock.withLock {
            guard !finished, let continuation, let format, !pcm16k.isEmpty else { return }
            let source = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
            guard let input = AVAudioPCMBuffer(pcmFormat: source, frameCapacity: AVAudioFrameCount(pcm16k.count)) else {
                return
            }
            input.frameLength = AVAudioFrameCount(pcm16k.count)
            pcm16k.withUnsafeBufferPointer { input.floatChannelData![0].update(from: $0.baseAddress!, count: $0.count) }
            guard let converter else {
                continuation.yield(AnalyzerInput(buffer: input))
                return
            }
            let capacity = AVAudioFrameCount(Double(pcm16k.count) * format.sampleRate / 16_000) + 64
            guard let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return }
            var given = false
            var error: NSError?
            converter.convert(to: out, error: &error) { _, status in
                if given {
                    status.pointee = .noDataNow
                    return nil
                }
                given = true
                status.pointee = .haveData
                return input
            }
            if out.frameLength > 0 { continuation.yield(AnalyzerInput(buffer: out)) }
        }
    }

    func finish() {
        let c = lock.withLock { () -> AsyncStream<AnalyzerInput>.Continuation? in
            finished = true
            defer { continuation = nil }
            return continuation
        }
        c?.finish()
        let waiter = finishedFlag.withLock { done -> CheckedContinuation<Void, Never>? in
            done = true
            return finishSignal.withLock { s in defer { s = nil }; return s }
        }
        waiter?.resume()
        if c == nil { task?.cancel() }
    }
}

// MARK: Fakes (self-test, development)

/// Turns fed audio into placeholder text: a partial result on the first
/// block, a final one for every second of audio. Never touches Speech.
final class FakeTranscriber: TranscriptionEngine, @unchecked Sendable {
    var onPartial: (@Sendable (String) -> Void)?
    var onFinal: (@Sendable (String) -> Void)?
    var onError: (@Sendable (String) -> Void)?
    var onStatus: (@Sendable (TranscriptionStatus) -> Void)?
    private let fed = Mutex(0)
    private var source = TranscriptSource.visitor

    func start(source: TranscriptSource) {
        self.source = source
        onStatus?(.running)
    }

    func feed(pcm16k: [Float]) {
        let (before, after) = fed.withLock { n -> (Int, Int) in
            defer { n += pcm16k.count }
            return (n, n + pcm16k.count)
        }
        if before == 0 { onPartial?("(test speech") }
        if after / 16_000 > before / 16_000 { onFinal?("(test speech \(after / 16_000))") }
    }

    func finish() { onStatus?(.finished) }
}

/// No microphone: grants at once, sends nothing unless told to.
@MainActor
final class FakeMicrophone: MicrophoneInput {
    private(set) var running = false
    private var onSamples: (@Sendable ([Float]) -> Void)?

    func requestAccess(_ done: @escaping @MainActor (Bool, String?) -> Void) { done(true, nil) }

    /// What the next start reports (the self-test sets the fallback too).
    var echo = EchoCancellation.system

    func start(
        _ onSamples: @escaping @Sendable ([Float]) -> Void, onEcho: @escaping @Sendable (EchoCancellation) -> Void
    ) throws {
        running = true
        self.onSamples = onSamples
        onEcho(echo)
    }

    /// Whether a call asked for echo cancellation to be prepared.
    private(set) var prepared = false
    func prepare(_ calls: Bool) { prepared = calls }

    func stop() {
        running = false
        onSamples = nil
    }

    /// Sends `count` samples of a quiet tone as if spoken.
    func speak(count: Int) {
        let samples = (0..<count).map { Float(0.1 * sin(Double($0) * 2 * Double.pi * 300 / 16_000)) }
        onSamples?(samples)
    }
}

/// The real prompt: an alert sheet on the window that holds the call (a
/// modal alert when no window is showing).
@MainActor
final class AlertTalkPrompter: TalkPrompter {
    func ask(_ prompt: TalkPermission.Prompt, answer: @escaping @MainActor (Bool) -> Void) {
        let alert = NSAlert()
        alert.messageText = prompt.title
        alert.informativeText = prompt.message
        alert.addButton(withTitle: prompt.allow)
        alert.addButton(withTitle: prompt.cancel)
        if let window = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first(where: { $0.isVisible }) {
            alert.beginSheetModal(for: window) { response in
                MainActor.assumeIsolated { answer(response == .alertFirstButtonReturn) }
            }
        } else {
            answer(alert.runModal() == .alertFirstButtonReturn)
        }
    }
}

/// The self-test's prompt: records what was asked and answers with the next
/// queued answer (Cancel when none), so no human is needed.
@MainActor
final class FakeTalkPrompter: TalkPrompter {
    private(set) var asked: [TalkPermission.Prompt] = []
    var answers: [Bool] = []
    func ask(_ prompt: TalkPermission.Prompt, answer: @escaping @MainActor (Bool) -> Void) {
        asked.append(prompt)
        answer(answers.isEmpty ? false : answers.removeFirst())
    }
}

/// Half a second of tone per reply, without a voice.
@MainActor
final class FakeSpeechRenderer: SpeechRenderer {
    private(set) var rendered: [String] = []
    func render(_ text: String) async -> [Float] {
        rendered.append(text)
        return (0..<8_000).map { Float(0.2 * sin(Double($0) * 2 * Double.pi * 220 / 16_000)) }
    }
}
