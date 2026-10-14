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
    /// A smart-alert check of a camera the window does not show (docs/UX.md
    /// "Smart alerts"): the live verification view's inset. Its lifetime is
    /// the check's (`VerificationBoard`), not the inset clock's.
    case verification

    /// Red for smoke and CO, a calmer accent for the rest.
    public var isCritical: Bool { if case .soundAlarm = self { true } else { false } }

    /// The inset carries a call (captions, Talk, typed reply).
    public var isCall: Bool { self == .doorbell || self == .conversation }

    /// Doorbell and detection insets close by themselves after the event;
    /// sound alarms stay until dismissed; a check's inset goes with its
    /// check.
    public var closesByItself: Bool { !isCritical && self != .view && self != .verification }

    /// The header's reason.
    public var reason: String {
        switch self {
        case .doorbell: "Doorbell"
        case .conversation: "Conversation"
        case .soundAlarm(let kind): "\(DetectionTracker.label(kind)) (sound)"
        case .detection(let kind): DetectionTracker.label(kind)
        case .view: "Back"
        case .verification: "Smart alert check"
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
    /// A return inset of a stage that showed several tiles: it draws the
    /// whole layout in miniature (`camera` is only its identity), not one
    /// camera.
    public var showsLayout = false
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

/// How many insets a window draws at once, as a grid (docs/UX.md "Alert
/// insets"): `rows` insets stacked in a column, up to `columns` columns. The
/// rest wait in "+N more". The default, 3 rows in 1 column, is the original
/// stack of three.
public struct InsetGridSetting: Sendable, Equatable, Hashable, Codable {
    public static let rowRange = 1...4
    public static let columnRange = 1...6
    public static let standard = InsetGridSetting(rows: 3, columns: 1)

    public let rows: Int
    public let columns: Int

    /// Clamped to the ranges.
    public init(rows: Int, columns: Int) {
        self.rows = min(max(rows, Self.rowRange.lowerBound), Self.rowRange.upperBound)
        self.columns = min(max(columns, Self.columnRange.lowerBound), Self.columnRange.upperBound)
    }

    /// How many insets are drawn at most.
    public var capacity: Int { rows * columns }

    private enum CodingKeys: String, CodingKey { case rows, columns }

    /// A saved value goes through the clamping initializer.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(rows: try c.decode(Int.self, forKey: .rows), columns: try c.decode(Int.self, forKey: .columns))
    }
}

/// A window's insets, newest first, and the swap: clicking an inset puts its
/// camera on the whole stage and the view that was there into an inset
/// (`InsetItem.isReturn`), so one more click swaps back.
///
/// The order is stable: an event on a camera that has an inset refreshes it
/// where it is; a new camera takes the top. At most `capacity` insets are
/// drawn; a newer one pushes an older one into "+N more" only once the older
/// has been on screen `minimumShownTime`, otherwise it waits its turn.
public struct InsetBoard: Sendable, Equatable {
    /// How many insets are drawn by default; the rest show as "+N more".
    public static let maxShown = InsetGridSetting.standard.capacity
    /// An inset stays on screen at least this long (viewer clock, seconds)
    /// before a newer one can push it out.
    public static let minimumShownTime = 10.0
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
        /// The stage showed a layout (several tiles, nothing focused): the
        /// return inset draws all of it, not just `displaced`.
        public var displacedLayout = false
        public var shown: CameraRef { item.camera }
    }

    /// Newest first; a return inset included.
    public private(set) var items: [InsetItem] = []
    public private(set) var swap: Swap?
    /// How many insets may be drawn at once.
    public private(set) var capacity = InsetBoard.maxShown
    /// The cameras drawn now, and since when (viewer clock).
    private var drawn: [CameraRef: Double] = [:]
    /// The latest time any call gave (for changes that carry none).
    private var lastNow = 0.0

    public init(capacity: Int = InsetBoard.maxShown) { self.capacity = max(capacity, 1) }

    /// The items drawn in this window (all but the detached ones).
    public var attached: [InsetItem] { items.filter { !$0.detached } }
    /// The insets drawn, top of the stack first.
    public var shown: [InsetItem] { attached.filter { drawn[$0.camera] != nil } }
    /// The insets waiting for room ("+N more"), top of the stack first.
    public var waiting: [InsetItem] { attached.filter { drawn[$0.camera] == nil } }
    /// How many more there are than are drawn.
    public var more: Int { waiting.count }
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
            // Refreshed where it is: no churn among the insets.
            items[i].count += 1
            items[i].lastAt = date
            items[i].active = true
            items[i].startedAt = now
            items[i].endedAt = nil
            // A sound alarm outranks the rest on the same camera.
            if kind.isCritical || !items[i].kind.isCritical { items[i].kind = kind }
            settle(now: now)
            return true
        }
        guard !visible || detached else { return false }
        items.insert(
            InsetItem(
                camera: camera, kind: kind, firstAt: date, lastAt: date, count: 1, active: true, startedAt: now,
                endedAt: nil, usedAt: now, detached: detached),
            at: 0)
        settle(now: now)
        return true
    }

    /// A camera waiting in "+N more" was chosen: it takes the top and a
    /// place among the drawn ones; the lowest drawn inset goes to the
    /// waiting list. Returns whether anything changed.
    @discardableResult
    public mutating func bringIn(_ camera: CameraRef, now: Double) -> Bool {
        guard drawn[camera] == nil,
            let i = items.firstIndex(where: { $0.camera == camera && !$0.isReturn && !$0.detached })
        else { return false }
        var item = items.remove(at: i)
        item.usedAt = max(item.usedAt, now)
        items.insert(item, at: 0)
        settle(now: now, force: camera)
        return true
    }

    /// Changes how many insets are drawn; those over the new capacity go to
    /// the waiting list (lowest first), regardless of how long they were up.
    public mutating func setCapacity(_ capacity: Int, now: Double? = nil) {
        self.capacity = max(capacity, 1)
        settle(now: now)
    }

    /// Decides which items are drawn. Kept ones stay; free places go to the
    /// waiting items, newest first; a waiting item newer than a drawn one
    /// takes its place only if that has been up `minimumShownTime`. A return
    /// inset is always drawn, and `force` (a chip click) too.
    private mutating func settle(now: Double?, force: CameraRef? = nil) {
        if let now { lastNow = max(lastNow, now) }
        let t = lastNow
        let order = attached.map(\.camera)
        drawn = drawn.filter { order.contains($0.key) }
        func rank(_ c: CameraRef) -> Int { order.firstIndex(of: c) ?? Int.max }
        func isReturn(_ c: CameraRef) -> Bool { items.contains { $0.camera == c && $0.isReturn } }
        /// The drawn item to give up: the lowest in the stack that may go.
        func victim(ignoringTime: Bool, below: Int) -> CameraRef? {
            drawn.filter { c, since in
                !isReturn(c) && rank(c) > below && (ignoringTime || t - since >= Self.minimumShownTime)
            }.max { rank($0.key) < rank($1.key) }?.key
        }
        // Over capacity (it was reduced): the lowest ones go.
        while drawn.count > capacity, let out = victim(ignoringTime: true, below: -1) { drawn[out] = nil }
        for camera in order {
            guard drawn[camera] == nil else { continue }
            let must = isReturn(camera) || camera == force
            if drawn.count < capacity {
                drawn[camera] = t
            } else if must, let out = victim(ignoringTime: true, below: -1) {
                drawn[out] = nil
                drawn[camera] = t
            } else if let out = victim(ignoringTime: false, below: rank(camera)) {
                drawn[out] = nil
                drawn[camera] = t
            }
        }
    }

    /// The first time something waiting may take a place: when the lowest
    /// drawn inset below a waiting one has been up long enough.
    private var settleDeadline: Double? {
        let order = attached.map(\.camera)
        guard let firstWaiting = order.firstIndex(where: { drawn[$0] == nil }) else { return nil }
        var earliest: Double?
        for (c, since) in drawn {
            guard let r = order.firstIndex(of: c), r > firstWaiting, !items.contains(where: { $0.camera == c && $0.isReturn })
            else { continue }
            earliest = min(earliest ?? .infinity, since + Self.minimumShownTime)
        }
        return earliest
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
        settle(now: now)
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
        settle(now: now)
        return closed + due
    }

    /// The next time `tick` has something to do.
    public var deadline: Double? {
        ((items + [swap?.item].compactMap(\.self)).compactMap(Self.closesAt) + [settleDeadline].compactMap(\.self)).min()
    }

    /// Esc or the close control: the inset goes. Dismissing the swapped-in
    /// camera ends the swap; dismissing the return inset swaps back. Returns
    /// the dismissed event's inset (nil for a return inset).
    @discardableResult
    public mutating func dismiss(_ camera: CameraRef) -> InsetItem? {
        defer { settle(now: nil) }
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
        defer { settle(now: nil) }
        if let s = swap { return dismiss(s.shown) }
        guard let top = shown.first else { return nil }
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
    /// the swapped-in one back into its inset. `displacedLayout`: the stage
    /// showed a layout (several tiles, nothing focused), so the return inset
    /// draws all of it.
    public mutating func swap(
        _ camera: CameraRef, displaced: CameraRef?, displacedLayout: Bool = false, now: Double
    ) {
        defer { settle(now: now) }
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
        swap = Swap(item: item, displaced: displaced, displacedLayout: displacedLayout && displaced != nil)
        if let displaced {
            items.insert(
                InsetItem(
                    camera: displaced, kind: .view, firstAt: item.firstAt, lastAt: item.lastAt, count: 0,
                    active: false, startedAt: now, endedAt: now, usedAt: now, isReturn: true,
                    showsLayout: displacedLayout),
                at: 0)
        }
    }

    /// Tab: swaps the top inset in, or back when swapped.
    public mutating func swapTop(displaced: CameraRef?, displacedLayout: Bool = false, now: Double) {
        if swap != nil {
            swapBack(now: now)
        } else if let top = shown.first {
            swap(top.camera, displaced: displaced, displacedLayout: displacedLayout, now: now)
        }
    }

    /// Back to the stage's own view; the swapped-in camera returns to the
    /// top of the stack (when `now` is given; nil drops it).
    public mutating func swapBack(now: Double?) {
        defer { settle(now: now) }
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
        defer { settle(now: nil) }
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
        defer { settle(now: nil) }
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
        grid(
            heights: Array(heights.prefix(count)), width: width, rows: max(count, 1), columns: 1, anchor: anchor,
            stage: stage)
    }

    /// The inset width for a stage width.
    public static func width(stage: Double) -> Double { width(stage: stage, columns: 1) }

    /// How many columns of insets fit side by side (at least one, at most
    /// `columns`), each at least `minWidth` wide.
    public static func fittingColumns(stage: Double, columns: Int) -> Int {
        let fit = Int(((stage - 2 * margin + gap) / (minWidth + gap)).rounded(.down))
        return max(1, min(columns, fit))
    }

    /// The inset width for a grid of up to `columns` columns: about 30% of
    /// the stage's width, narrower when the columns need the room, never
    /// under `minWidth` (columns that do not fit are dropped instead; see
    /// `fittingColumns`). One column is the original single stack.
    public static func width(stage: Double, columns: Int) -> Double {
        let used = fittingColumns(stage: stage, columns: columns)
        let available = max((stage - 2 * margin - gap * Double(used - 1)) / Double(used), 0)
        return min(max(min(stage * widthFraction, available), minWidth), available)
    }

    /// The frames of insets of the given heights in a grid of up to `rows`
    /// insets per column and `columns` columns: column by column, the
    /// first column at the anchor's horizontal edge (the right edge when the
    /// anchor is right of the middle, so the grid grows leftward). In each
    /// column the order is as in `frames(count:...anchor:...)`. Returns the
    /// frames of the insets that fit, a prefix of `heights`: an inset that
    /// does not fit the stage's height starts the next column, and one that
    /// finds no column left ends the grid (the caller counts the rest in
    /// "+N more"). The first inset is always drawn, even when it alone is
    /// taller than the stage.
    public static func grid(
        heights: [Double], width: Double, rows: Int, columns: Int, anchor: InsetAnchor,
        stage: (width: Double, height: Double)
    ) -> [(x: Double, y: Double, width: Double, height: Double)] {
        let maxColumns = fittingColumns(stage: stage.width, columns: columns)
        var perColumn: [[Double]] = []
        var used = margin
        for h in heights {
            if let last = perColumn.last, last.count < max(rows, 1), used + h + margin <= stage.height {
                perColumn[perColumn.count - 1].append(h)
                used += h + gap
            } else if perColumn.count < maxColumns {
                perColumn.append([h])
                used = margin + h + gap
            } else {
                break
            }
        }
        guard !perColumn.isEmpty else { return [] }
        func height(_ column: [Double]) -> Double { column.reduce(0, +) + gap * Double(column.count - 1) }
        let tallest = perColumn.map(height).max() ?? 0
        let gridWidth = Double(perColumn.count) * width + gap * Double(perColumn.count - 1)
        let left = margin + anchor.x * (stage.width - 2 * margin - gridWidth)
        let top = margin + anchor.y * (stage.height - 2 * margin - tallest)
        let upward = anchor.growsUpward
        let fromRight = anchor.x > 0.5
        var out: [(x: Double, y: Double, width: Double, height: Double)] = []
        for (j, column) in perColumn.enumerated() {
            let step = Double(j) * (width + gap)
            let x = fromRight ? left + gridWidth - width - step : left + step
            var offset = 0.0
            for h in column {
                let y = upward ? top + tallest - offset - h : top + offset
                out.append((x, y, width, h))
                offset += h + gap
            }
        }
        return out
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
