import Foundation

// Alert insets (docs/UX.md "Alert insets"): when something important happens
// on a camera the window is not showing, its live video appears in an inset
// over the stage. A pure model per window, tested headless with the time
// passed in (the viewer clock, seconds) and dates for the header.

/// Why an inset opened.
public enum InsetKind: Sendable, Hashable {
    /// A doorbell press (the device's `visitor` alarm).
    case doorbell
    /// A conversation the user started on a camera's tile (no press): the
    /// same call as a doorbell's.
    case conversation
    /// A smoke or CO alarm heard in the camera's audio (`smoke_alarm`,
    /// `co_alarm` from `local_audio`).
    case soundAlarm(String)
    /// An object detection on a camera that asks for it (a person on a
    /// camera marked as an entrance), in the device's kind names.
    case detection(String)
    /// Not an event: the view the stage showed before a swap.
    case view

    /// Red for smoke and CO, a calmer accent for the rest.
    public var isCritical: Bool { if case .soundAlarm = self { true } else { false } }

    /// The inset carries a call (captions, Talk, typed reply).
    public var isCall: Bool { self == .doorbell || self == .conversation }

    /// Doorbell and detection insets close by themselves after the event;
    /// sound alarms stay until dismissed.
    public var closesByItself: Bool { !isCritical && self != .view }

    /// The header's reason.
    public var reason: String {
        switch self {
        case .doorbell: "Doorbell"
        case .conversation: "Conversation"
        case .soundAlarm(let kind): "\(DetectionTracker.label(kind)) (sound)"
        case .detection(let kind): DetectionTracker.label(kind)
        case .view: "Back"
        }
    }
}

/// Which events open an inset (the defaults of docs/UX.md): a doorbell press,
/// smoke and CO sound alarms, and people on cameras marked as entrances;
/// further kinds per camera.
public enum InsetTrigger {
    /// The person kinds a camera marked as an entrance opens an inset for.
    public static let entranceKinds: Set<String> = ["people", "person"]

    /// The inset an event (start or stop) belongs to, or nil if it opens none.
    /// `extraKinds` are the object kinds the user turned on for the camera.
    public static func kind(for event: DetectionEvent, entrance: Bool, extraKinds: Set<String> = []) -> InsetKind? {
        if event.alarm { return event.kind == "visitor" ? .doorbell : nil }
        if event.source == .localAudio { return AlertPolicy.kinds.contains(event.kind) ? .soundAlarm(event.kind) : nil }
        guard event.kind != DetectionEvent.motionKind else { return nil }
        if entrance && entranceKinds.contains(event.kind) { return .detection(event.kind) }
        return extraKinds.contains(event.kind) ? .detection(event.kind) : nil
    }
}

/// One inset: a camera, why, and since when.
public struct InsetItem: Sendable, Equatable, Identifiable {
    public var camera: CameraRef
    public var kind: InsetKind
    public var firstAt: Date
    public var lastAt: Date
    /// Event starts since it opened.
    public var count: Int
    /// The event is still going (no stop yet).
    public var active: Bool
    /// When the last start arrived, on the viewer clock.
    public var startedAt: Double
    /// When the event ended, on the viewer clock.
    public var endedAt: Double?
    /// The last click, swap or hover on it.
    public var usedAt: Double
    public var hovered = false
    /// The view the stage showed before a swap (its camera in the inset),
    /// rather than an event.
    public var isReturn = false
    /// The call is shown in its own window (docs/UX.md "Doorbell calls"):
    /// the item is kept for the call's whole life but not drawn, counted,
    /// leased, swapped or closed by the clock here.
    public var detached = false

    public var id: CameraRef { camera }

    /// The header's lines: reason, first time (and count and last time).
    public func header(now: Date, calendar: Calendar = .current, locale: Locale = .current) -> String {
        func stamp(_ d: Date) -> String {
            let f = DateFormatter()
            f.locale = locale
            f.calendar = calendar
            f.timeZone = calendar.timeZone
            f.timeStyle = .medium
            f.dateStyle = calendar.isDate(d, inSameDayAs: now) ? .none : .medium
            return f.string(from: d)
        }
        var text = "\(kind.reason) · \(stamp(firstAt))"
        if count > 1 { text += " · \(count) times, last \(stamp(lastAt))" }
        return text
    }
}

/// A window's insets, newest first, and the swap: clicking an inset puts its
/// camera on the whole stage and the view that was there into an inset
/// (`InsetItem.isReturn`), so one more click swaps back.
public struct InsetBoard: Sendable, Equatable {
    /// How many insets are drawn; the rest show as "+N more".
    public static let maxShown = 3
    /// Doorbell and detection insets close this long after the event ended
    /// and they were last used, unless hovered.
    public static let autoCloseDelay = 60.0
    /// An event whose stop never came counts as ended this long after its
    /// last start (a lost stop must not keep an inset open for good).
    public static let lostStopAfter = 120.0

    /// The camera on the whole stage after a swap, and its inset's state.
    public struct Swap: Sendable, Equatable {
        public var item: InsetItem
        /// The camera of the view that was on the stage (shown in the return
        /// inset), if it had one.
        public var displaced: CameraRef?
        public var shown: CameraRef { item.camera }
    }

    /// Newest first; a return inset included.
    public private(set) var items: [InsetItem] = []
    public private(set) var swap: Swap?

    public init() {}

    /// The items drawn in this window (all but the detached ones).
    public var attached: [InsetItem] { items.filter { !$0.detached } }
    /// The insets drawn, top of the stack first.
    public var shown: [InsetItem] { Array(attached.prefix(Self.maxShown)) }
    /// How many more there are than are drawn.
    public var more: Int { max(attached.count - Self.maxShown, 0) }
    /// Nothing on screen: no inset drawn and no swap (detached items do not
    /// count).
    public var isEmpty: Bool { attached.isEmpty && swap == nil }
    /// The calls shown in their own windows.
    public var detachedCameras: [CameraRef] { items.filter(\.detached).map(\.camera) }

    public func item(for camera: CameraRef) -> InsetItem? {
        if swap?.shown == camera { return swap?.item }
        return items.first { $0.camera == camera && !$0.isReturn }
    }

    /// An event started. `visible`: the window shows the camera already (it
    /// gets its tile alert instead). A repeat on a camera that has an inset
    /// updates it and brings it to the top. `detached`: a call that opens in
    /// its own window; its item is recorded whether or not the camera is
    /// visible, and a repeat keeps an item's presentation. Returns whether
    /// anything changed.
    @discardableResult
    public mutating func started(
        _ kind: InsetKind, camera: CameraRef, visible: Bool, now: Double, at date: Date, detached: Bool = false
    ) -> Bool {
        if var s = swap, s.shown == camera {
            s.item.count += 1
            s.item.lastAt = date
            s.item.active = true
            s.item.startedAt = now
            s.item.endedAt = nil
            if kind.isCritical || !s.item.kind.isCritical { s.item.kind = kind }
            swap = s
            return true
        }
        if let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn }) {
            var item = items.remove(at: i)
            item.count += 1
            item.lastAt = date
            item.active = true
            item.startedAt = now
            item.endedAt = nil
            // A sound alarm outranks the rest on the same camera.
            if kind.isCritical || !item.kind.isCritical { item.kind = kind }
            items.insert(item, at: 0)
            return true
        }
        guard !visible || detached else { return false }
        items.insert(
            InsetItem(
                camera: camera, kind: kind, firstAt: date, lastAt: date, count: 1, active: true, startedAt: now,
                endedAt: nil, usedAt: now, detached: detached),
            at: 0)
        return true
    }

    /// Pop Out (`true`) or Pop In (`false`) a call's item. Popping out the
    /// swapped-in camera ends the swap first (the stage shows its own view
    /// again). Popping in puts the inset back on top with a fresh minute.
    /// Returns whether anything changed.
    @discardableResult
    public mutating func setDetached(_ camera: CameraRef, _ detached: Bool, now: Double) -> Bool {
        if detached, let s = swap, s.shown == camera {
            swapBack(now: now)
        }
        guard let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn }), items[i].detached != detached
        else { return false }
        var item = items.remove(at: i)
        item.detached = detached
        item.hovered = false
        item.usedAt = max(item.usedAt, now)
        items.insert(item, at: 0)
        return true
    }

    /// An event stopped: the inset's close clock starts (it stays meanwhile).
    @discardableResult
    public mutating func stopped(_ kind: InsetKind, camera: CameraRef, now: Double) -> Bool {
        if var s = swap, s.shown == camera, s.item.kind == kind, s.item.active {
            s.item.active = false
            s.item.endedAt = now
            swap = s
            return true
        }
        guard let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn && $0.kind == kind }),
            items[i].active
        else { return false }
        items[i].active = false
        items[i].endedAt = now
        return true
    }

    /// When an inset that closes by itself closes, on the viewer clock (nil
    /// while hovered or for sound alarms).
    public static func closesAt(_ item: InsetItem) -> Double? {
        guard item.kind.closesByItself, !item.isReturn, !item.hovered, !item.detached else { return nil }
        let ended = item.endedAt ?? (item.startedAt + lostStopAfter)
        return max(ended, item.usedAt) + autoCloseDelay
    }

    /// Closes what is due at `now`. A swapped-in camera that closes ends the
    /// swap (the stage shows its own view again). Returns what closed.
    public mutating func tick(now: Double) -> [InsetItem] {
        var closed: [InsetItem] = []
        if let s = swap, let at = Self.closesAt(s.item), now >= at {
            closed.append(s.item)
            endSwap()
        }
        let due = items.filter { item in Self.closesAt(item).map { now >= $0 } ?? false }
        items.removeAll { item in due.contains(item) }
        return closed + due
    }

    /// The next time `tick` has something to do.
    public var deadline: Double? {
        (items + [swap?.item].compactMap(\.self)).compactMap(Self.closesAt).min()
    }

    /// Esc or the close control: the inset goes. Dismissing the swapped-in
    /// camera ends the swap; dismissing the return inset swaps back. Returns
    /// the dismissed event's inset (nil for a return inset).
    @discardableResult
    public mutating func dismiss(_ camera: CameraRef) -> InsetItem? {
        if let s = swap, s.shown == camera {
            endSwap()
            return s.item
        }
        if let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn }) {
            return items.remove(at: i)
        }
        if items.contains(where: { $0.camera == camera && $0.isReturn }) { swapBack(now: nil) }
        return nil
    }

    /// Esc: the swapped-in camera if there is one, else the top inset.
    @discardableResult
    public mutating func dismissTop() -> InsetItem? {
        if let s = swap { return dismiss(s.shown) }
        guard let top = attached.first else { return nil }
        if top.isReturn {
            swapBack(now: nil)
            return nil
        }
        return dismiss(top.camera)
    }

    /// A click on an inset (or Tab on the top one): its camera takes the
    /// stage and `displaced` (the camera the stage showed, if any) goes into
    /// the return inset. A click on the return inset swaps back. Clicking
    /// another event's inset while swapped puts that one on the stage and
    /// the swapped-in one back into its inset.
    public mutating func swap(_ camera: CameraRef, displaced: CameraRef?, now: Double) {
        if items.contains(where: { $0.camera == camera && $0.isReturn }) {
            swapBack(now: now)
            return
        }
        guard let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn && !$0.detached }) else { return }
        var item = items.remove(at: i)
        item.usedAt = now
        item.hovered = false
        if var s = swap {
            var previous = s.item
            previous.usedAt = now
            items.insert(previous, at: 0)
            s.item = item
            swap = s
            return
        }
        swap = Swap(item: item, displaced: displaced)
        if let displaced {
            items.insert(
                InsetItem(
                    camera: displaced, kind: .view, firstAt: item.firstAt, lastAt: item.lastAt, count: 0,
                    active: false, startedAt: now, endedAt: now, usedAt: now, isReturn: true),
                at: 0)
        }
    }

    /// Tab: swaps the top inset in, or back when swapped.
    public mutating func swapTop(displaced: CameraRef?, now: Double) {
        if swap != nil {
            swapBack(now: now)
        } else if let top = attached.first {
            swap(top.camera, displaced: displaced, now: now)
        }
    }

    /// Back to the stage's own view; the swapped-in camera returns to the
    /// top of the stack (when `now` is given; nil drops it).
    public mutating func swapBack(now: Double?) {
        guard let s = swap else { return }
        endSwap()
        if let now {
            var item = s.item
            item.usedAt = now
            items.insert(item, at: 0)
        }
    }

    private mutating func endSwap() {
        swap = nil
        items.removeAll(where: \.isReturn)
    }

    /// The pointer entered or left an inset: hovering holds it open.
    public mutating func hover(_ camera: CameraRef, _ inside: Bool, now: Double) {
        if var s = swap, s.shown == camera {
            s.item.hovered = inside
            s.item.usedAt = max(s.item.usedAt, now)
            swap = s
        }
        for i in items.indices where items[i].camera == camera {
            items[i].hovered = inside
            items[i].usedAt = max(items[i].usedAt, now)
        }
    }

    /// Something was done with an inset (or its call): its close clock
    /// starts again.
    public mutating func use(_ camera: CameraRef, now: Double) {
        if var s = swap, s.shown == camera, s.item.usedAt < now {
            s.item.usedAt = now
            swap = s
        }
        for i in items.indices where items[i].camera == camera && !items[i].isReturn {
            items[i].usedAt = max(items[i].usedAt, now)
        }
    }

    /// Sound-alarm insets whose alert was dismissed elsewhere (another
    /// window's tile header) go too. Returns what went.
    @discardableResult
    public mutating func dropSoundAlarms(where gone: (CameraRef) -> Bool) -> [InsetItem] {
        var dropped: [InsetItem] = []
        if let s = swap, s.item.kind.isCritical, gone(s.shown) {
            dropped.append(s.item)
            endSwap()
        }
        let out = items.filter { $0.kind.isCritical && gone($0.camera) }
        items.removeAll { $0.kind.isCritical && gone($0.camera) }
        return dropped + out
    }

    /// A camera's inset that is not an event's (the return inset) or a
    /// camera the window no longer may show (its device was removed).
    public mutating func remove(where condition: (InsetItem) -> Bool) {
        if let s = swap, condition(s.item) { endSwap() }
        items.removeAll(where: condition)
    }
}

/// Where the insets sit on the stage: a corner, opposite the corner
/// indicators by default.
extension ChromeCorner {
    /// The diagonally opposite corner.
    public var opposite: ChromeCorner {
        switch self {
        case .topLeft: .bottomRight
        case .topRight: .bottomLeft
        case .bottomLeft: .topRight
        case .bottomRight: .topLeft
        }
    }
}

/// The insets' geometry on a stage (top-left origin): about 30% of the
/// stage's width, in one column placed by an `InsetAnchor` (a corner by
/// default), newest nearest the column's nearer horizontal edge.
public enum InsetGeometry {
    public static let widthFraction = 0.3
    public static let minWidth = 220.0
    public static let margin = 12.0
    public static let gap = 8.0
    public static let headerHeight = 22.0

    /// The frames of `heights.count` insets of the given heights (header,
    /// video and any call controls) in a stage of `size`, stacked from
    /// `corner`. Insets that do not fit are left out (the caller counts them
    /// in "+N more").
    public static func frames(
        count: Int, width: Double, heights: [Double], corner: ChromeCorner, stage: (width: Double, height: Double)
    ) -> [(x: Double, y: Double, width: Double, height: Double)] {
        frames(count: count, width: width, heights: heights, anchor: InsetAnchor(corner: corner), stage: stage)
    }

    /// The same for a column placed anywhere: `anchor` is the column's
    /// position as a fraction of the free space inside the margins. Below
    /// the middle (`y > 0.5`) the newest inset is at the column's bottom and
    /// the others stack upward; otherwise the newest is at its top. Insets
    /// that do not fit the stage's height (inside the margins) are left out;
    /// the newest is always drawn, even when it alone is taller.
    public static func frames(
        count: Int, width: Double, heights: [Double], anchor: InsetAnchor, stage: (width: Double, height: Double)
    ) -> [(x: Double, y: Double, width: Double, height: Double)] {
        var fitting: [Double] = []
        var used = margin
        for h in heights.prefix(count) {
            guard used + h + margin <= stage.height || fitting.isEmpty else { break }
            fitting.append(h)
            used += h + gap
        }
        guard !fitting.isEmpty else { return [] }
        let column = fitting.reduce(0, +) + gap * Double(fitting.count - 1)
        let x = margin + anchor.x * (stage.width - 2 * margin - width)
        let top = margin + anchor.y * (stage.height - 2 * margin - column)
        let upward = anchor.growsUpward
        var out: [(x: Double, y: Double, width: Double, height: Double)] = []
        var offset = 0.0
        for h in fitting {
            let y = upward ? top + column - offset - h : top + offset
            out.append((x, y, width, h))
            offset += h + gap
        }
        return out
    }

    /// The inset width for a stage width.
    public static func width(stage: Double) -> Double {
        min(max(stage * widthFraction, minWidth), max(stage - 2 * margin, 0))
    }
}

/// Where a window's inset column sits (docs/UX.md "Alert insets"): its
/// position as a fraction of the free space inside the stage's margins,
/// `x = (left - margin) / (stageWidth - 2 margin - columnWidth)` and the same
/// for `y` with heights, each in 0...1. (0, 0) is the top left corner and
/// (1, 1) the bottom right. Resizing the window or going full screen keeps
/// the fractions, so a column in a corner stays in it.
public struct InsetAnchor: Sendable, Equatable, Hashable, Codable {
    public let x: Double
    public let y: Double

    /// Clamped to 0...1 (anything not finite counts as 0).
    public init(x: Double, y: Double) {
        self.x = x.isFinite ? min(max(x, 0), 1) : 0
        self.y = y.isFinite ? min(max(y, 0), 1) : 0
    }

    /// A corner's anchor (the default placement).
    public init(corner: ChromeCorner) {
        switch corner {
        case .topLeft: self.init(x: 0, y: 0)
        case .topRight: self.init(x: 1, y: 0)
        case .bottomLeft: self.init(x: 0, y: 1)
        case .bottomRight: self.init(x: 1, y: 1)
        }
    }

    /// The column grows upward from its bottom edge (it sits nearer the
    /// bottom); at the exact middle it grows downward.
    public var growsUpward: Bool { y > 0.5 }

    private enum CodingKeys: String, CodingKey { case x, y }

    /// A saved value goes through the clamping initializer.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(x: try c.decode(Double.self, forKey: .x), y: try c.decode(Double.self, forKey: .y))
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(x, forKey: .x)
        try c.encode(y, forKey: .y)
    }
}

/// Dragging the inset column (docs/UX.md "Alert insets"): where it lands as
/// an anchor, each axis snapping to its edge within `snapDistance`.
public enum InsetPlacement {
    /// Points from an edge (inside the margins) within which an axis snaps
    /// to that edge.
    public static let snapDistance = 24.0

    /// The anchor for a column whose top left corner was dragged to
    /// `origin` in a stage of `stage` (top-left origin, points).
    public static func anchor(
        origin: (x: Double, y: Double), column: (width: Double, height: Double), stage: (width: Double, height: Double)
    ) -> InsetAnchor {
        InsetAnchor(
            x: fraction(origin.x, size: column.width, stage: stage.width),
            y: fraction(origin.y, size: column.height, stage: stage.height))
    }

    /// One axis: the offset from the near margin over the free space; 0 or 1
    /// within the snap distance of either end (the nearer end when both
    /// are, the start on a tie); 0 when there is no free space.
    public static func fraction(_ position: Double, size: Double, stage: Double) -> Double {
        let free = stage - 2 * InsetGeometry.margin - size
        guard free > 0 else { return 0 }
        let fromStart = position - InsetGeometry.margin
        let fromEnd = free - fromStart
        if min(fromStart, fromEnd) <= snapDistance { return fromStart <= fromEnd ? 0 : 1 }
        return fromStart / free
    }

    /// The column's top left corner for an anchor (the inverse of `anchor`
    /// away from the edges).
    public static func origin(
        anchor: InsetAnchor, column: (width: Double, height: Double), stage: (width: Double, height: Double)
    ) -> (x: Double, y: Double) {
        let m = InsetGeometry.margin
        return (m + anchor.x * (stage.width - 2 * m - column.width), m + anchor.y * (stage.height - 2 * m - column.height))
    }
}
