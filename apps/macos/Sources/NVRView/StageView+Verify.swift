import AppKit
import NVRCore

// Live smart-alert checks in a viewer window (docs/UX.md "Smart alerts",
// "Live checks"). The checks live in `SmartAlertsModel.board`; this
// window shows each one where it belongs: a band over the camera's tile when
// the window shows the camera, else (per the setting) a verification inset,
// the same mechanism as the alert insets (draggable column, snapping,
// stacking, "+N more"). Nothing here takes the keyboard.

extension StageView {
    func installVerification() {
        guard !fixedTile else { return }
        verifyToken = model.alertsModel.subscribe { [weak self] in self?.syncVerification() }
    }

    func tearDownVerification() {
        if let verifyToken { model.alertsModel.unsubscribe(verifyToken) }
        verifyToken = nil
        for v in verifyBands.values { v.removeFromSuperview() }
        for v in verifyInsetOverlays.values { v.removeFromSuperview() }
        verifyBands = [:]
        verifyInsetOverlays = [:]
    }

    /// The camera of a check as this window names it.
    private func ref(_ c: VerifyCheck) -> CameraRef? { c.primaryCamera.flatMap { model.cameraRef($0) } }

    /// Where each shown check goes in this window: tiles (by camera) and
    /// insets (by camera), newest first.
    private func verificationPlaces() -> (tiles: [CameraRef: [VerifyCheck]], insets: [CameraRef: [VerifyCheck]]) {
        var tiles: [CameraRef: [VerifyCheck]] = [:]
        var insets: [CameraRef: [VerifyCheck]] = [:]
        let alerts = model.alertsModel!
        for c in alerts.board.shown {
            // Every angle the check looks at that this window shows gets the
            // band; the first angle decides the inset.
            var inTile = false
            for cam in c.cameras.compactMap({ model.cameraRef($0) }) where showsCamera(cam) {
                if alerts.place(c, onScreen: true) == .tile {
                    tiles[cam, default: []].append(c)
                    inTile = true
                }
            }
            guard !inTile, let cam = ref(c) else { continue }
            if alerts.place(c, onScreen: showsCamera(cam)) == .inset { insets[cam, default: []].append(c) }
        }
        return (tiles, insets)
    }

    /// After any change of the checks: inset items in and out, and the
    /// bands' and overlays' contents.
    func syncVerification() {
        guard !fixedTile else { return }
        let places = verificationPlaces()
        let wanted = Set(places.insets.keys)
        let before = insetBoard
        // An inset the user enlarged (swapped onto the stage) is on screen,
        // so its check is placed in the tile and is no longer "wanted" as an
        // inset; it stays enlarged until the user swaps back or its check
        // ends, not until the next refresh.
        var enlarged: CameraRef?
        if let s = insetBoard.swap, s.item.kind == .verification,
            model.alertsModel.board.shown.contains(where: { ref($0) == s.shown })
        {
            enlarged = s.shown
        }
        insetBoard.remove { $0.kind == .verification && !wanted.contains($0.camera) && $0.camera != enlarged }
        let now = clock.now
        for cam in wanted where insetBoard.item(for: cam) == nil && insetBoard.swap?.shown != cam {
            insetBoard.started(.verification, camera: cam, visible: false, now: now, at: Date())
        }
        if insetBoard != before { syncInsets() }
        layoutVerification(currentVerificationLeaves)
        layoutVerificationInsets()
    }

    private var currentVerificationLeaves: [LeafFrame] { insetBoard.swap == nil ? frames.leaves : [] }

    /// The bands over the tiles (from the stage's chrome layout).
    func layoutVerification(_ leaves: [LeafFrame]) {
        guard !fixedTile, let alerts = model.alertsModel else { return }
        let tiles = verificationPlaces().tiles
        var used: Set<NodePath> = []
        for leaf in leaves {
            guard let cam = leaf.camera, let checks = tiles[cam], let top = checks.first else { continue }
            used.insert(leaf.path)
            let view = verifyBands[leaf.path] ?? makeOverlay(in: tileOverlay)
            verifyBands[leaf.path] = view
            if view.frame != leaf.rect { view.frame = leaf.rect }
            let picture = pictureSizes[cam] ?? CGSize(width: 16, height: 9)
            let key = state.viewKey(leaf.path)
            let shown = TilePlacement.shownRect(
                rect: leaf.rect, picture: picture, fill: fills(atTile: leaf.path),
                zoom: key.map { state.views.zoom($0) } ?? .none)
            view.pictureRect = shown.offsetBy(dx: -leaf.rect.minX, dy: -leaf.rect.minY)
            view.bandFraction = 0.4
            view.apply(
                VerifyOverlayContent(
                    check: top, camera: model.alertCamera(cam), model: model, lines: 4, others: checks.count - 1))
        }
        for (path, view) in verifyBands where !used.contains(path) {
            view.removeFromSuperview()
            verifyBands[path] = nil
        }
        _ = alerts
    }

    /// The overlays on the verification insets (and on any inset of a
    /// camera with a check, such as a doorbell's).
    func layoutVerificationInsets() {
        guard !fixedTile else { return }
        let insets = verificationPlaces().insets
        var used: Set<CameraRef> = []
        for (cam, checks) in insets {
            guard let top = checks.first, let inset = alertInsetViews[cam], !inset.isHidden else { continue }
            used.insert(cam)
            let view = verifyInsetOverlays[cam] ?? makeOverlay(in: insetLayer)
            verifyInsetOverlays[cam] = view
            let video = inset.videoRect
            let frame = video.offsetBy(dx: inset.frame.minX, dy: inset.frame.minY)
            if view.frame != frame { view.frame = frame }
            let picture = pictureSizes[cam] ?? CGSize(width: 16, height: 9)
            view.pictureRect = PictureFit.fit(picture: picture, in: CGRect(origin: .zero, size: video.size), fill: false).destination
            view.bandFraction = 0.5
            view.apply(
                VerifyOverlayContent(
                    check: top, camera: model.alertCamera(cam), model: model, lines: 5, others: checks.count - 1))
        }
        for (cam, view) in verifyInsetOverlays where !used.contains(cam) {
            view.removeFromSuperview()
            verifyInsetOverlays[cam] = nil
        }
        restackInsetLayer()
    }

    /// The inset layer's order, bottom to top: the stage's own controls, then
    /// each inset (oldest first) with its verification overlay directly above
    /// it. Views are moved only when the order is wrong (the set of insets
    /// changed), never on every refresh.
    func restackInsetLayer() {
        var wanted: [NSView] = []
        for item in insetBoard.shown.reversed() {
            guard let v = alertInsetViews[item.camera], v.superview === insetLayer else { continue }
            wanted.append(v)
            if let o = verifyInsetOverlays[item.camera], o.superview === insetLayer { wanted.append(o) }
        }
        let subviews = insetLayer.subviews
        let base = subviews.firstIndex { $0 === stageCall } ?? -1
        let indices = wanted.map { w in subviews.firstIndex { $0 === w } ?? -1 }
        var ordered = indices.first.map { $0 > base } ?? true
        for (a, b) in zip(indices, indices.dropFirst()) where b <= a { ordered = false }
        if ordered { return }
        var below: NSView = stageCall
        for v in wanted {
            insetLayer.addSubview(v, positioned: .above, relativeTo: below)
            below = v
        }
    }

    private func makeOverlay(in parent: NSView) -> VerificationOverlayView {
        let v = VerificationOverlayView()
        v.onClose = { [weak self] id in self?.model.alertsModel.dismiss(id) }
        v.onDismissAll = { [weak self] in self?.model.alertsModel.dismissAll() }
        v.onHover = { [weak self] id, inside in self?.model.alertsModel.hover(id, inside) }
        parent.addSubview(v)
        return v
    }

    /// The inset of a check was closed (its × or Esc): its checks go too.
    func verificationInsetDismissed(_ camera: CameraRef) {
        let alerts = model.alertsModel!
        for c in alerts.board.shown where ref(c) == camera { alerts.dismiss(c.id) }
    }

    // MARK: Self-test

    /// The band over the tile showing `camera`, if one is up.
    func verificationBand(for camera: CameraRef) -> VerificationOverlayView? {
        guard let leaf = frames.leaves.first(where: { $0.camera == camera }) else { return nil }
        return verifyBands[leaf.path].flatMap { $0.isHidden || $0.superview == nil ? nil : $0 }
    }

    func verificationInsetOverlay(for camera: CameraRef) -> VerificationOverlayView? {
        verifyInsetOverlays[camera].flatMap { $0.isHidden || $0.superview == nil ? nil : $0 }
    }
}
