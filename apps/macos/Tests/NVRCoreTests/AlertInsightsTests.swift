import CoreGraphics
import Foundation
import NVRCore
import Testing

// What the alert filters learned, as the app reads it (FFI minor 17): the
// generic insight layers, the color scale and heatmap raster, and the trace
// words for the filters step and their quiet-log reasons.

@Suite struct AlertInsightsTests {
    /// The real core's shape (crates/alerts/src/insight.rs), a small grid.
    static let sample = """
        {"camera":{"device":2,"channel":3,"lens":0},"at_ms":1000,"night":true,
         "layers":[
          {"id":"scene.person","title":"Scene memory: person","description":"Where person boxes have stayed.",
           "provenance":"A running average (1999).","units":"warmth","range":[0.0,1.0],"kind":"grid",
           "rows":2,"cols":3,"values":[0,0.2,0.9,0,0,1],"class":"person","threshold":0.7,"updated_ms":900},
          {"id":"repeat.last","title":"Last alerted boxes","description":"","provenance":"","units":"score",
           "range":[0.0,1.0],"kind":"boxes","boxes":[{"class":"person","score":0.6,"box":{"x":0.1,"y":0.2,"w":0.1,"h":0.3}}],
           "at_ms":800},
          {"id":"thresholds","title":"Thresholds","description":"","provenance":"","units":"","range":[0.0,1.0],
           "kind":"scalar","items":[{"id":"floor.night","label":"Score floor at night","value":0.5,"units":"score"},
                                   {"id":"repeat.window","label":"Repeat window","value":30.0,"units":"min"}]},
          {"id":"future.thing","title":"A later method","description":"Something new.","provenance":"",
           "units":"","range":[0,1],"kind":"contours"}
         ],
         "picture":{"trigger_id":42,"frame":0,"at_ms":990}}
        """

    @Test func parsesEveryKindAndKeepsUnknownOnes() throws {
        let i = try #require(AlertInsights.parse(Self.sample))
        #expect(i.camera == AlertCamera(device: 2, channel: 3))
        #expect(i.night)
        #expect(i.picture?.triggerID == 42 && i.picture?.frame == 0)
        #expect(i.gridLayers.map(\.id) == ["scene.person"])
        let g = try #require(i.gridLayers.first?.grid)
        #expect(g.rows == 2 && g.cols == 3 && g.className == "person" && g.threshold == 0.7)
        #expect(g.value(row: 0, col: 2) == 0.9 && g.value(row: 1, col: 2) == 1 && g.value(row: 2, col: 0) == nil)
        #expect(g.hotCells == 2)
        guard case .boxes(let boxes, let at) = i.boxLayers.first?.data else { Issue.record("no boxes"); return }
        #expect(boxes.first?.className == "person" && boxes.first?.w == 0.1 && at == 800)
        guard case .scalar(let items) = i.scalarLayers.first?.data else { Issue.record("no scalars"); return }
        #expect(items.map(\.text) == ["0.50 score", "30 min"])
        #expect(i.layers.last?.data == .other(kind: "contours"))
        #expect(i.layers.last?.description == "Something new.")
    }

    @Test func aGridOfTheWrongSizeIsRefused() {
        let bad = Self.sample.replacingOccurrences(of: #""rows":2"#, with: #""rows":3"#)
        #expect(AlertInsights.parse(bad) == nil)
    }

    @Test func theCameraList() throws {
        let l = try #require(
            InsightCameraList.parse(#"{"cameras":[{"camera":{"device":1,"channel":0,"lens":1},"classes":["person","vehicle"]}],"at_ms":5}"#))
        #expect(l.cameras.first?.camera == AlertCamera(device: 1, channel: 0, lens: 1))
        #expect(l.cameras.first?.classes == ["person", "vehicle"])
        #expect(InsightCameraList.parse("[") == nil)
    }

    @Test func theColorScaleRunsFromTheFirstStopToTheLast() {
        let lo = InsightColorScale.color(-1, in: 0...1), hi = InsightColorScale.color(2, in: 0...1)
        #expect(lo.r == InsightColorScale.stops[0].r && lo.b == InsightColorScale.stops[0].b)
        #expect(hi.g == InsightColorScale.stops[4].g)
        let mid = InsightColorScale.color(0.5, in: 0...1)
        #expect(abs(mid.g - InsightColorScale.stops[2].g) < 1e-9)
        // Brighter toward the top.
        func light(_ c: (r: Double, g: Double, b: Double)) -> Double { 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b }
        let steps = (0...10).map { light(InsightColorScale.color(Double($0) / 10, in: 0...1)) }
        #expect(zip(steps, steps.dropFirst()).allSatisfy { $0 < $1 })
    }

    @Test func theHeatmapIsOnePixelPerCellAndClearWhereNothingWasLearned() throws {
        let g = InsightGrid(rows: 2, cols: 3, values: [0, 0.2, 0.9, 0, 0, 1])
        let image = try #require(InsightColorScale.image(g, range: 0...1))
        #expect(image.width == 3 && image.height == 2)
        let data = try #require(image.dataProvider?.data as Data?)
        // Top left: nothing learned, transparent; bottom right: the top color.
        #expect(data[3] == 0)
        let top = InsightColorScale.stops[4]
        #expect(data[5 * 4 + 3] == 255)
        #expect(data[5 * 4] == UInt8((top.r * 255).rounded()) && data[5 * 4 + 1] == UInt8((top.g * 255).rounded()))
        #expect(InsightColorScale.image(InsightGrid(rows: 2, cols: 2, values: [1]), range: 0...1) == nil)
    }

    @Test func theFiltersStepAndItsReasonsInWords() {
        func line(_ json: String) -> String {
            TraceText.line(TraceStep(JSONValue.parse(json)!)!, cameraName: { _ in "C" }, time: { "t\($0)" })
        }
        #expect(
            line(#"{"at_ms":1,"step":"filters","class":"person","hits":2,"stationary":2,"moving":false,"acted":"scene_memory"}"#)
                == "Filters: 2 person boxes, 2 in place, not moving → held back: in the same place as before (scene memory)")
        #expect(
            line(#"{"at_ms":1,"step":"filters","class":"person","hits":1,"stationary":0,"moving":true,"repeat_of":7,"acted":"repeat"}"#)
                == "Filters: 1 person box, moving, repeats alert #7 → held back: a repeat")
        #expect(
            line(#"{"at_ms":1,"step":"filters","class":"person","hits":2,"stationary":0,"moving":false,"acted":"parked"}"#)
                == "Filters: 2 person boxes, not moving → not an always-alert case (did not move)")
        #expect(
            line(#"{"at_ms":1,"step":"frame_sampled","camera":{"device":1,"channel":0,"lens":0},"detections":[{"class":"person","score":0.61,"above":true,"in_region":true,"hit":true,"stationary":true}]}"#)
                == "C: picture: person 0.61 hit, in place")
        #expect(line(#"{"at_ms":1,"step":"decision","decision":"quiet","reason":"stationary","for_ms":600000}"#)
            == "Decision: quiet log (in the same place as before (scene memory))")
        #expect(line(#"{"at_ms":1,"step":"decision","decision":"quiet","reason":"repeat","alert_id":3,"since_ms":5}"#)
            == "Decision: quiet log (the same detections as the last alert)")
    }
}
