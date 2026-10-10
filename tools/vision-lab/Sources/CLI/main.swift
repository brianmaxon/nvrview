import Foundation
import VisionLab

let usage = """
    vision-lab: on-device detection bench for decoded video (see docs/VISION.md)

    vision-lab run <file.h264|file.h265> [options]
      --source-fps F       pictures per second of the input (default 15)
      --duration S         or: stream length in seconds (source fps = pictures / S)
      --fps R              sample R pictures per second of stream time (default: every picture)
      --frames FILE        or: sample exactly the picture indices in FILE (first column of a label file)
      --detectors LIST     comma list of human, human-upper, animal, face, classify, coreml,
                           or none to measure decoding alone (default human,animal)
      --model PATH         Core ML model for 'coreml' (.mlmodel, .mlpackage or .mlmodelc); repeatable
      --units U            Core ML compute units: all, cpu, cpu-gpu, cpu-ane (default all)
      --vision-device D    Vision built-ins' main stage: auto, cpu, gpu, ane (default auto)
      --scale-fit          letterbox pictures into the Core ML input instead of stretching
      --realtime           decode at the source rate, like a live tile, to measure steady CPU use
      --max-seconds S      stop after S seconds of stream time
      --results FILE       write one JSON line per sampled picture
      --export DIR         write sampled pictures as JPEG (local labeling only; never commit)
      --report FILE        write the summary as JSON (also printed)
      --allow-software     do not require the hardware decoder

    vision-lab eval --labels FILE --results FILE [--events FILE ...] [options]
      --labels FILE        frame,person,vehicle,animal rows (0/1)
      --results FILE       results of a run over (at least) the labeled pictures
      --events FILE        results of a sampled run, for event-level recall; repeatable
      --source-fps F       as for run (needed to turn label rows into times)
      --thresholds LIST    confidence thresholds (default 0.3,0.5,0.7)
      --gap S              labels/detections closer than S seconds form one event (default 4)

    vision-lab plan --model PATH [--units U]
      where Core ML plans to run each operation (cpu, gpu, ane)

    vision-lab identifiers
      the image classifier's labels and the ones mapped to each class
    """

struct Args {
    var positional: [String] = []
    var options: [String: [String]] = [:]
    var flags: Set<String> = []

    static let flagNames: Set<String> = ["--realtime", "--scale-fit", "--allow-software", "--help"]

    init(_ argv: [String]) throws {
        var i = 0
        while i < argv.count {
            let a = argv[i]
            if Self.flagNames.contains(a) {
                flags.insert(a)
            } else if a.hasPrefix("--") {
                guard i + 1 < argv.count else { throw LabError.message("\(a) needs a value") }
                options[a, default: []].append(argv[i + 1])
                i += 1
            } else {
                positional.append(a)
            }
            i += 1
        }
    }

    func one(_ name: String) -> String? { options[name]?.last }
    func double(_ name: String) throws -> Double? {
        guard let s = one(name) else { return nil }
        guard let v = Double(s), v > 0 else { throw LabError.message("\(name) needs a positive number") }
        return v
    }
}

func printJSON<T: Encodable>(_ value: T, to url: URL? = nil) throws {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    let data = try encoder.encode(value)
    if let url { try data.write(to: url) }
    print(String(decoding: data, as: UTF8.self))
}

func frameIndices(_ path: String) throws -> Set<Int> {
    let text = try String(contentsOfFile: path, encoding: .utf8)
    var set = Set<Int>()
    for line in text.split(whereSeparator: \.isNewline) {
        if let first = line.split(separator: ",").first, let n = Int(first.trimmingCharacters(in: .whitespaces)) {
            set.insert(n)
        }
    }
    return set
}

func makeDetectors(_ args: Args) throws -> ([Detector], [String: Double]) {
    let names = (args.one("--detectors") ?? "human,animal").split(separator: ",").map(String.init)
    let device = VisionDevice(rawValue: args.one("--vision-device") ?? "auto")
    guard let device else { throw LabError.message("--vision-device must be auto, cpu, gpu or ane") }
    guard let units = ComputeUnits(rawValue: args.one("--units") ?? "all") else {
        throw LabError.message("--units must be all, cpu, cpu-gpu or cpu-ane")
    }
    var detectors: [Detector] = []
    var loadSeconds: [String: Double] = [:]
    for name in names where name != "none" {
        switch name {
        case "human": detectors.append(try HumanDetector(upperBodyOnly: false, device: device))
        case "human-upper": detectors.append(try HumanDetector(upperBodyOnly: true, device: device))
        case "animal": detectors.append(try AnimalDetector(device: device))
        case "face": detectors.append(try FaceDetector(device: device))
        case "classify": detectors.append(try SceneClassifier(device: device))
        case "coreml":
            let models = args.options["--model"] ?? []
            guard !models.isEmpty else { throw LabError.message("coreml needs --model") }
            for m in models {
                let t = now()
                let d = try CoreMLDetector(
                    modelURL: URL(fileURLWithPath: m), units: units, scaleFit: args.flags.contains("--scale-fit"))
                loadSeconds[d.name] = now() - t
                detectors.append(d)
            }
        default: throw LabError.message("unknown detector \(name)")
        }
    }
    return (detectors, loadSeconds)
}

struct RunOutput: Encodable {
    var report: BenchReport
    var modelLoadSeconds: [String: Double]
    var sampling: String
    var sourceFPS: Double
    var visionDevice: String
    var computeUnits: String
}

func run(_ args: Args) throws {
    guard let path = args.positional.first else { throw LabError.message(usage) }
    let input = URL(fileURLWithPath: path)
    guard let codec = Codec.from(path: path) else { throw LabError.message("cannot tell the codec from \(path)") }

    var sourceFPS = try args.double("--source-fps") ?? 15
    if let duration = try args.double("--duration") {
        let count = accessUnits(splitAnnexB(try Data(contentsOf: input)), codec: codec).count
        sourceFPS = Double(count) / duration
    }
    let sampling: Sampling
    var samplingName = "all"
    if let fps = try args.double("--fps") {
        sampling = .rate(fps)
        samplingName = "\(fps) fps"
    } else if let file = args.one("--frames") {
        let set = try frameIndices(file)
        sampling = .frames(set)
        samplingName = "\(set.count) listed pictures"
    } else {
        sampling = .all
    }

    let (detectors, loads) = try makeDetectors(args)
    var config = BenchConfig(
        input: input, codec: codec, sourceFPS: sourceFPS, sampling: sampling, detectors: detectors)
    config.realtime = args.flags.contains("--realtime")
    config.maxSeconds = try args.double("--max-seconds")
    config.exportDir = args.one("--export").map { URL(fileURLWithPath: $0) }
    config.resultsURL = args.one("--results").map { URL(fileURLWithPath: $0) }
    config.requireHardware = !args.flags.contains("--allow-software")

    let report = try runBench(config)
    try printJSON(
        RunOutput(
            report: report, modelLoadSeconds: loads, sampling: samplingName, sourceFPS: sourceFPS,
            visionDevice: args.one("--vision-device") ?? "auto", computeUnits: args.one("--units") ?? "all"),
        to: args.one("--report").map { URL(fileURLWithPath: $0) })
}

func fmt(_ v: Double?) -> String { v.map { String(format: "%.2f", $0) } ?? "-" }

func pad(_ s: String, _ width: Int) -> String {
    s.count >= width ? s + " " : s + String(repeating: " ", count: width - s.count)
}

func eval(_ args: Args) throws {
    guard let labelsPath = args.one("--labels"), let resultsPath = args.one("--results") else {
        throw LabError.message(usage)
    }
    let labels = try parseLabels(try String(contentsOfFile: labelsPath, encoding: .utf8))
    let results = try parseResults(try Data(contentsOf: URL(fileURLWithPath: resultsPath)))
    let thresholds = (args.one("--thresholds") ?? "0.3,0.5,0.7").split(separator: ",").compactMap { Float($0) }
    let sourceFPS = try args.double("--source-fps") ?? 15
    let gap = try args.double("--gap") ?? 4

    let detectorNames = Set(results.flatMap { $0.results.keys }).sorted() + ["any"]
    let labeled = Set(labels.map(\.frame))
    let scored = results.filter { labeled.contains($0.frame) }.count
    print("pictures labeled: \(labels.count), with results: \(scored)")
    for cls in ObjectClass.allCases {
        print("  \(cls.rawValue) present in \(labels.filter { $0.present.contains(cls) }.count)")
    }
    print("\npicture level (presence of the class anywhere in the picture)")
    print(pad("detector", 16) + pad("class", 9) + "thr     tp    fp    fn    tn  precision  recall  fp-rate")
    for d in detectorNames {
        for cls in ObjectClass.allCases {
            for t in thresholds {
                let s = scoreFrames(labels: labels, results: results, detector: d, cls: cls, threshold: t)
                if s.tp + s.fp == 0 && s.fn == 0 { continue }
                let fpRate = s.fp + s.tn == 0 ? nil : Double(s.fp) / Double(s.fp + s.tn)
                print(
                    pad(d, 16) + pad(cls.rawValue, 9)
                        + String(format: "%.2f %5d %5d %5d %5d", t, s.tp, s.fp, s.fn, s.tn)
                        + "  " + pad(fmt(s.precision), 9) + "  " + pad(fmt(s.recall), 6) + "  " + fmt(fpRate))
            }
        }
    }

    // Events from the labels: positive label times merged with `gap`, each
    // widened by half the labeling interval.
    let sortedFrames = labels.map(\.frame).sorted()
    let steps = zip(sortedFrames.dropFirst(), sortedFrames).map { Double($0 - $1) / sourceFPS }
    let labelInterval = steps.isEmpty ? 1 : steps.sorted()[steps.count / 2]
    for eventsPath in args.options["--events"] ?? [] {
        let run = try parseResults(try Data(contentsOf: URL(fileURLWithPath: eventsPath)))
        print("\nevent level: \((eventsPath as NSString).lastPathComponent) (\(run.count) sampled pictures)")
        let hours = Double(run.map(\.frame).max() ?? 0) / sourceFPS / 3600
        print(pad("detector", 16) + pad("class", 9) + "thr   events  detected  recall  false-episodes  per-hour  median-latency-s")
        for cls in ObjectClass.allCases {
            let times = labels.filter { $0.present.contains(cls) }.map { Double($0.frame) / sourceFPS }
            let truth = spans(times, gap: gap, pad: labelInterval / 2)
            for d in Set(run.flatMap { $0.results.keys }).sorted() + ["any"] {
                for t in thresholds {
                    let e = scoreEvents(
                        truth: truth, results: run, detector: d, cls: cls, threshold: t, gap: gap,
                        pad: labelInterval / 2)
                    if e.events == 0 && e.falseEpisodes == 0 { continue }
                    let lat = e.latencySeconds.sorted()
                    let median = lat.isEmpty ? nil : lat[lat.count / 2]
                    print(
                        pad(d, 16) + pad(cls.rawValue, 9)
                            + String(format: "%.2f %7d %9d  ", t, e.events, e.detected)
                            + pad(fmt(e.events == 0 ? nil : Double(e.detected) / Double(e.events)), 6)
                            + String(format: "  %14d  %8.1f  ", e.falseEpisodes, hours > 0 ? Double(e.falseEpisodes) / hours : 0)
                            + fmt(median))
                }
            }
        }
    }
}

func plan(_ args: Args) async throws {
    guard let model = args.one("--model") else { throw LabError.message(usage) }
    guard let units = ComputeUnits(rawValue: args.one("--units") ?? "all") else {
        throw LabError.message("--units must be all, cpu, cpu-gpu or cpu-ane")
    }
    let counts = try await computePlanSummary(modelURL: URL(fileURLWithPath: model), units: units)
    for (device, n) in counts.sorted(by: { $0.key < $1.key }) {
        print("\(device): \(n) operations")
    }
}

func identifiers() throws {
    let c = try SceneClassifier(device: .auto)
    let all = try c.supportedLabels()
    print("\(all.count) labels")
    for cls in ObjectClass.allCases {
        let mapped = SceneClassifier.labels(for: cls)
        print("\(cls.rawValue): \(all.filter { mapped.contains($0) }.sorted().joined(separator: ", "))")
        print("  not in the taxonomy: \(mapped.subtracting(all).sorted().joined(separator: ", "))")
    }
}

do {
    let argv = Array(CommandLine.arguments.dropFirst())
    let args = try Args(Array(argv.dropFirst()))
    switch argv.first {
    case "run": try run(args)
    case "eval": try eval(args)
    case "plan": try await plan(args)
    case "identifiers": try identifiers()
    default:
        print(usage)
        exit(argv.isEmpty || argv.first == "--help" ? 0 : 2)
    }
} catch {
    FileHandle.standardError.write(Data("vision-lab: \(error)\n".utf8))
    exit(1)
}
