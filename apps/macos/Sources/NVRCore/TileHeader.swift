import Foundation

/// The tile's hover header and its menus (docs/UX.md "Tile header",
/// docs/CAPTURE.md "The header model"): which buttons fit a tile of a given
/// width, in which order, what moves into the "…" menu, and the right-click
/// menu built from the same list. Pure, with no view code, so both apps lay
/// out headers the same way. The vectors in
/// `fixtures/synthetic/tile-header.json` are shared with NvrView.Tiles
/// (TileHeader.cs) on Windows.

/// Everything a header or its menus can hold. The raw values are stable
/// identifiers (the shared vectors use them).
public enum TileHeaderItem: String, CaseIterable, Sendable {
    /// "● REC 1:23": an indicator, never in the overflow.
    case recordingPill = "recording-pill"
    /// The camera's name: truncated before it is dropped.
    case title
    /// An active alert's Dismiss: an indicator, never in the overflow.
    case dismissAlert = "dismiss-alert"
    case speaker
    case record
    case savePicture = "save-picture"
    case conversation
    case panTilt = "pan-tilt"
    case presets
    case lens
    case resetZoom = "reset-zoom"
    /// Menus only: brings a filled picture's moved crop back to the middle.
    case resetPosition = "reset-position"
    case quality
    case fisheye
    /// The "…" button.
    case more
    // The long tail: menus only.
    case pop
    /// The tile's own Fill choice (a checked item).
    case fillTile = "fill-tile"
    case allowTalkBack = "allow-talk-back"
    case entrance
    case callsOpenIn = "calls-open-in"
    case savePictureAs = "save-picture-as"
    case saveZoomedArea = "save-zoomed-area"
    case saveOriginalFisheye = "save-original-fisheye"
    case recordMain = "record-main"
    case recordSub = "record-sub"
}

/// Where the camera's doorbell call is, if one is open.
public enum TileHeaderCall: String, Sendable {
    /// No call for this camera.
    case noCall = "none"
    /// A call shown in the viewer windows' insets: "Pop Out Call".
    case inset
    /// This tile is the call window's own tile: "Pop In Call", and Record
    /// is the call recording.
    case window
}

/// What a tile's header depends on: the camera's capabilities and the
/// tile's state.
public struct TileHeaderTile: Equatable, Sendable {
    /// The camera has audio (a Speaker button).
    public var audio = false
    /// `ipcAudioTalk` 1: talk-back, so Start Conversation is in the header.
    public var talk = false
    /// A doorbell: Start Conversation in the header, and "Calls open in".
    public var doorbell = false
    /// `ptzType` 3.
    public var panTilt = false
    /// `ptzPreset` bit 0.
    public var presets = false
    /// `ptzType` 1 or 3, and the camera reported lens ranges.
    public var lens = false
    public var fisheye = false
    /// The tile shows a picture; false under a no-picture veil
    /// ("Connecting", "Reconnecting").
    public var picture = true
    /// The camera is being recorded (the pill; Record becomes Stop).
    public var recording = false
    /// Zoomed past 1x, or a moved virtual camera.
    public var zoomed = false
    /// At 1x, a filled picture's crop moved off center.
    public var panned = false
    /// An active alert on the tile.
    public var alert = false
    /// The tile is audible (Speaker says Stop Listening).
    public var listening = false
    /// The camera's call is open (End Conversation).
    public var conversation = false
    public var call = TileHeaderCall.noCall
    /// A recording player's tile (docs/UX.md "Recordings"): only what applies
    /// to a picture or its stream is offered (Save Picture, Reset Zoom,
    /// Quality (Auto, High, Low), Fisheye View, Fill Tile, Reset Position,
    /// the Save items); no Record, Speaker, talk-back, PTZ or call items.
    public var playback = false

    public init(
        audio: Bool = false, talk: Bool = false, doorbell: Bool = false, panTilt: Bool = false,
        presets: Bool = false, lens: Bool = false, fisheye: Bool = false, picture: Bool = true,
        recording: Bool = false, zoomed: Bool = false, panned: Bool = false, alert: Bool = false,
        listening: Bool = false, conversation: Bool = false, call: TileHeaderCall = .noCall,
        playback: Bool = false
    ) {
        self.audio = audio
        self.talk = talk
        self.doorbell = doorbell
        self.panTilt = panTilt
        self.presets = presets
        self.lens = lens
        self.fisheye = fisheye
        self.picture = picture
        self.recording = recording
        self.zoomed = zoomed
        self.panned = panned
        self.alert = alert
        self.listening = listening
        self.conversation = conversation
        self.call = call
        self.playback = playback
    }
}

/// Widths in points (DIPs on Windows).
public struct TileHeaderMetrics: Equatable, Sendable {
    /// An icon button, "…" included.
    public var icon: Double = 26
    /// The title's least width before buttons give way.
    public var titleMin: Double = 48
    /// The recording pill.
    public var pill: Double = 64
    /// The header's inset at each end.
    public var padding: Double = 8
    /// A width change smaller than this from the width that last changed
    /// the result keeps the result.
    public var hysteresis: Double = 8
    /// Below this tile width only the pill, the title and "…" show.
    public var narrow: Double = 120

    public init() {}

    public static let standard = TileHeaderMetrics()
}

/// One menu item: the item, its English label and its key hints.
public struct TileHeaderMenuEntry: Equatable, Sendable {
    public let item: TileHeaderItem
    public let label: String
    /// The key hint on macOS ("" when there is none).
    public let macShortcut: String
    /// The key hint on Windows ("" when there is none).
    public let windowsShortcut: String

    public init(_ item: TileHeaderItem, _ label: String, mac: String = "", windows: String = "") {
        self.item = item
        self.label = label
        macShortcut = mac
        windowsShortcut = windows
    }
}

/// The header of one tile at one width.
public struct TileHeaderLayout: Equatable, Sendable {
    /// The width at which the visible items last changed (hysteresis).
    public let anchor: Double
    public let tile: TileHeaderTile
    public let titleWidth: Double
    public let metrics: TileHeaderMetrics
    /// The items shown, in display order: the pill, the title, Dismiss, the
    /// buttons by priority, then "…" when anything is hidden.
    public let visible: [TileHeaderItem]
    /// The header buttons that did not fit, by priority.
    public let overflow: [TileHeaderItem]
    /// The title has less room than its natural width.
    public let titleTruncated: Bool

    public var showsMore: Bool { visible.contains(.more) }

    /// The right-click menu: every action, in the same order as the header.
    public var contextMenu: [TileHeaderMenuEntry] { Self.menu(for: tile) }

    /// The "…" menu: the right-click menu without what the header shows.
    public var moreMenu: [TileHeaderMenuEntry] {
        let shown = Set(visible)
        return contextMenu.filter { !shown.contains($0.item) }
    }

    /// Lays out the header for a tile `width` wide whose title needs
    /// `titleWidth`. Pass the previous result for the same tile so small
    /// width changes do not reflow the buttons.
    public static func compute(
        width: Double, titleWidth: Double, tile: TileHeaderTile, metrics: TileHeaderMetrics = .standard,
        previous: TileHeaderLayout? = nil
    ) -> TileHeaderLayout {
        let fresh = place(width: width, tile: tile, metrics: metrics)
        guard let previous, previous.tile == tile, previous.titleWidth == titleWidth, previous.metrics == metrics
        else {
            return TileHeaderLayout(width: width, titleWidth: titleWidth, tile: tile, metrics: metrics, placed: fresh)
        }
        if abs(width - previous.anchor) < metrics.hysteresis || fresh.visible == previous.visible {
            // Same items as before: the anchor stays; only the title's room
            // follows the width.
            let kept = Placement(visible: previous.visible, overflow: previous.overflow)
            return TileHeaderLayout(
                width: previous.anchor, titleWidth: titleWidth, tile: tile, metrics: metrics,
                placed: kept, at: width)
        }
        return TileHeaderLayout(width: width, titleWidth: titleWidth, tile: tile, metrics: metrics, placed: fresh)
    }

    /// What a recording player's tile offers: the picture's own actions.
    public static let playbackItems: Set<TileHeaderItem> = [
        .savePicture, .resetZoom, .resetPosition, .quality, .fisheye, .fillTile, .savePictureAs, .saveZoomedArea,
        .saveOriginalFisheye,
    ]

    /// The full menu for a tile, in header order, then the long tail.
    public static func menu(for tile: TileHeaderTile) -> [TileHeaderMenuEntry] {
        let all = fullMenu(for: tile)
        return tile.playback ? all.filter { playbackItems.contains($0.item) } : all
    }

    private static func fullMenu(for tile: TileHeaderTile) -> [TileHeaderMenuEntry] {
        let inWindow = tile.call == .window
        var m: [TileHeaderMenuEntry] = []
        if tile.alert { m.append(.init(.dismissAlert, "Dismiss Alert", mac: "↩", windows: "Enter")) }
        if tile.audio { m.append(.init(.speaker, tile.listening ? "Stop Listening" : "Listen", mac: "M", windows: "M")) }
        if inWindow {
            m.append(
                .init(
                    .record, tile.recording ? "Stop Recording Call" : "Record Call", mac: "⇧⌘R",
                    windows: "Ctrl+Shift+R"))
        } else {
            m.append(.init(.record, tile.recording ? "Stop Recording" : "Start Recording", mac: "R", windows: "R"))
        }
        if tile.picture {
            m.append(
                inWindow
                    ? .init(.savePicture, "Save Picture", mac: "⇧⌘P", windows: "Ctrl+Shift+P")
                    : .init(.savePicture, "Save Picture", mac: "P", windows: "P"))
        }
        m.append(
            .init(.conversation, tile.conversation ? "End Conversation" : "Start Conversation", mac: "T", windows: "T"))
        if tile.panTilt { m.append(.init(.panTilt, "Pan and Tilt…", mac: "arrows", windows: "arrows")) }
        if tile.presets { m.append(.init(.presets, "Presets", mac: "⌥1–9", windows: "Alt+1–9")) }
        if tile.lens { m.append(.init(.lens, "Zoom and Focus…", mac: "⌥= ⌥-", windows: "Alt+= Alt+-")) }
        if tile.zoomed { m.append(.init(.resetZoom, "Reset Zoom", mac: "0", windows: "0")) }
        m.append(.init(.quality, "Quality"))
        if tile.fisheye { m.append(.init(.fisheye, "Fisheye View")) }
        if tile.panned && !tile.zoomed { m.append(.init(.resetPosition, "Reset Position", mac: "0", windows: "0")) }
        if !inWindow { m.append(.init(.fillTile, "Fill Tile")) }
        switch tile.call {
        case .noCall: break
        case .inset: m.append(.init(.pop, "Pop Out Call"))
        case .window: m.append(.init(.pop, "Pop In Call"))
        }
        m.append(.init(.allowTalkBack, "Allow talk-back"))
        m.append(.init(.entrance, "Entrance"))
        if tile.doorbell { m.append(.init(.callsOpenIn, "Calls open in")) }
        if tile.picture {
            m.append(.init(.savePictureAs, "Save Picture As…", mac: "⇧P", windows: "Shift+P"))
            if tile.zoomed { m.append(.init(.saveZoomedArea, "Save Zoomed Area")) }
            if tile.fisheye { m.append(.init(.saveOriginalFisheye, "Save Original Fisheye Picture")) }
        }
        if !tile.recording && !inWindow {
            m.append(.init(.recordMain, "Record Main Stream"))
            m.append(.init(.recordSub, "Record Sub-stream"))
        }
        return m
    }

    /// The header buttons that apply to a tile, by priority (the pill,
    /// the title and Dismiss excluded).
    public static func buttons(for tile: TileHeaderTile) -> [TileHeaderItem] {
        let all = fullButtons(for: tile)
        return tile.playback ? all.filter { playbackItems.contains($0) } : all
    }

    private static func fullButtons(for tile: TileHeaderTile) -> [TileHeaderItem] {
        var b: [TileHeaderItem] = []
        if tile.audio { b.append(.speaker) }
        b.append(.record)
        if tile.picture { b.append(.savePicture) }
        if tile.talk || tile.doorbell { b.append(.conversation) }
        if tile.panTilt { b.append(.panTilt) }
        if tile.presets { b.append(.presets) }
        if tile.lens { b.append(.lens) }
        if tile.zoomed { b.append(.resetZoom) }
        b.append(.quality)
        if tile.fisheye { b.append(.fisheye) }
        return b
    }

    private struct Placement {
        var visible: [TileHeaderItem]
        var overflow: [TileHeaderItem]
    }

    private static func place(width: Double, tile: TileHeaderTile, metrics: TileHeaderMetrics) -> Placement {
        let buttons = buttons(for: tile)
        // Under a no-picture veil: the title and "…" only (Retry is in the veil).
        if !tile.picture {
            return Placement(visible: buttons.isEmpty ? [.title] : [.title, .more], overflow: buttons)
        }
        let pill: [TileHeaderItem] = tile.recording ? [.recordingPill] : []
        if width < metrics.narrow {
            return Placement(visible: pill + (buttons.isEmpty ? [.title] : [.title, .more]), overflow: buttons)
        }
        let dismiss: [TileHeaderItem] = tile.alert ? [.dismissAlert] : []
        let fixed = (tile.recording ? metrics.pill : 0) + (tile.alert ? metrics.icon : 0) + metrics.titleMin
        var room = width - 2 * metrics.padding - fixed
        if Double(buttons.count) * metrics.icon <= room {
            return Placement(visible: pill + [.title] + dismiss + buttons, overflow: [])
        }
        room -= metrics.icon  // the "…" comes first once anything is hidden
        var shown = 0
        while shown < buttons.count && room >= metrics.icon {
            room -= metrics.icon
            shown += 1
        }
        return Placement(
            visible: pill + [.title] + dismiss + buttons.prefix(shown) + [.more],
            overflow: Array(buttons.dropFirst(shown)))
    }

    /// `at` is the actual width when the result is kept from an earlier one.
    private init(
        width: Double, titleWidth: Double, tile: TileHeaderTile, metrics: TileHeaderMetrics, placed: Placement,
        at: Double? = nil
    ) {
        anchor = width
        self.titleWidth = titleWidth
        self.tile = tile
        self.metrics = metrics
        visible = placed.visible
        overflow = placed.overflow
        var others = 0.0
        for item in placed.visible {
            switch item {
            case .title: break
            case .recordingPill: others += metrics.pill
            default: others += metrics.icon
            }
        }
        let room = max(0, (at ?? width) - 2 * metrics.padding - others)
        titleTruncated = room < titleWidth
    }
}
