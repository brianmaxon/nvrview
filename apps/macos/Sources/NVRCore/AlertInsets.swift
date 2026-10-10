import Foundation

// Alert insets (docs/UX.md "Alert insets"): when something important happens
// on a camera the window is not showing, its live video appears in an inset
// over the stage. A pure model per window, tested headless with the time
// passed in (the viewer clock, seconds) and dates for the header.

/// Why an inset opened.
public enum InsetKind: Sendable, Hashable {
    /// A doorbell press (the device's `visitor` alarm).
    case doorbell
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

    /// Doorbell and detection insets close by themselves after the event;
    /// sound alarms stay until dismissed.
    public var closesByItself: Bool { !isCritical && self != .view }

    /// The header's reason.
    public var reason: String {
        switch self {
        case .doorbell: "Doorbell"
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

    /// The insets drawn, top of the stack first.
    public var shown: [InsetItem] { Array(items.prefix(Self.maxShown)) }
    /// How many more there are than are drawn.
    public var more: Int { max(items.count - Self.maxShown, 0) }
    public var isEmpty: Bool { items.isEmpty && swap == nil }

    public func item(for camera: CameraRef) -> InsetItem? {
        if swap?.shown == camera { return swap?.item }
        return items.first { $0.camera == camera && !$0.isReturn }
    }

    /// An event started. `visible`: the window shows the camera already (it
    /// gets its tile alert instead). A repeat on a camera that has an inset
    /// updates it and brings it to the top. Returns whether anything changed.
    @discardableResult
    public mutating func started(_ kind: InsetKind, camera: CameraRef, visible: Bool, now: Double, at date: Date)
        -> Bool
    {
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
        guard !visible else { return false }
        items.insert(
            InsetItem(
                camera: camera, kind: kind, firstAt: date, lastAt: date, count: 1, active: true, startedAt: now,
                endedAt: nil, usedAt: now),
            at: 0)
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
        guard item.kind.closesByItself, !item.isReturn, !item.hovered else { return nil }
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
        guard let top = items.first else { return nil }
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
        guard let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn }) else { return }
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
        } else if let top = items.first {
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
        if var s = swap, s.shown == camera {
            s.item.usedAt = now
            swap = s
        }
        for i in items.indices where items[i].camera == camera && !items[i].isReturn { items[i].usedAt = now }
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
/// stage's width, stacked from the corner, newest nearest it.
public enum InsetGeometry {
    public static let widthFraction = 0.3
    public static let minWidth = 220.0
    public static let margin = 12.0
    public static let gap = 8.0
    public static let headerHeight = 22.0

    /// The frames of `heights.count` insets of the given heights (header,
    /// video and any call controls) in a stage of `size`. Insets that do not
    /// fit are left out (the caller counts them in "+N more").
    public static func frames(
        count: Int, width: Double, heights: [Double], corner: ChromeCorner, stage: (width: Double, height: Double)
    ) -> [(x: Double, y: Double, width: Double, height: Double)] {
        var out: [(x: Double, y: Double, width: Double, height: Double)] = []
        let left = corner == .topLeft || corner == .bottomLeft
        let top = corner == .topLeft || corner == .topRight
        let x = left ? margin : stage.width - margin - width
        var used = margin
        for h in heights.prefix(count) {
            guard used + h + margin <= stage.height || out.isEmpty else { break }
            let y = top ? used : stage.height - used - h
            out.append((x, y, width, h))
            used += h + gap
        }
        return out
    }

    /// The inset width for a stage width.
    public static func width(stage: Double) -> Double {
        min(max(stage * widthFraction, minWidth), max(stage - 2 * margin, 0))
    }
}
