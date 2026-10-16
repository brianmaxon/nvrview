import Foundation
import NVRCore
import AppAlerts

// How long ago a live check started, counting up (docs/UX.md "Live
// checks"): in the band over a tile, in the overlay on a verification inset,
// and in that inset's header next to its time. Driven by the stage's time
// step on the viewer clock, so a test advances it; the views are redrawn only
// when the words change (seconds under a minute, then minutes, then hours).

extension StageView {
    /// The inset's header, with "12 s ago" after the time for a
    /// verification inset.
    package func insetHeader(_ item: InsetItem, now: Date) -> String {
        var text = item.header(now: now)
        guard item.kind == .verification, let alerts = model.alertsModel, let camera = model.alertCamera(item.camera),
            let check = alerts.board.shown(on: camera).first
        else { return text }
        text += " · " + ElapsedText.ago(since: check.startedAt, now: model.clock.now)
        return text
    }

    /// From the stage's time step: redraws the live checks when an elapsed
    /// time changed.
    package func tickVerificationElapsed(now: Double) {
        guard !fixedTile, verifyToken != nil, let alerts = model.alertsModel else { return }
        var texts: [UInt64: String] = [:]
        for c in alerts.board.visible { texts[c.id] = ElapsedText.ago(since: c.startedAt, now: now) }
        guard texts != verifyElapsed else { return }
        verifyElapsed = texts
        guard !verifyBands.isEmpty || !verifyInsetOverlays.isEmpty else { return }
        layoutVerification(currentVerificationLeaves)
        layoutVerificationInsets()
        refreshInsetContents()
    }
}
