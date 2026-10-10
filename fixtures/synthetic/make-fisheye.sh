#!/usr/bin/env bash
# Generates the synthetic fisheye video fixture used by the mock NVR, the
# apps' self-tests and the dewarping tests. Rendered from a made-up scene by
# the script below (python3, standard library only) and encoded with ffmpeg
# (libx264): no real footage.
#
#   fisheye/h264-fisheye.h264
#                       384x384, 20 frames, 20 fps, one keyframe (parameter
#                       sets in front of it), no B-frames, video range
#                       BT.601 (flagged in the VUI). In a directory of its
#                       own so that globs over the other fixtures (the demo
#                       self-tests, which expect pictures reaching every
#                       edge) leave it out.
#
# The picture is what an equidistant fisheye lens (r = f * theta) with a 180°
# field of view, mounted on a ceiling and looking down, would see of a
# colored sphere around it:
#
# - Image circle: center (0.49, 0.505) of the width and height (deliberately
#   off center), radius 0.46 of the width; black outside it.
# - Directions are named as the viewer names them for a ceiling mount
#   (docs/RENDERING.md, "Fisheye dewarping"): azimuth psi, 0 toward the top
#   of the picture and growing clockwise; tilt tau below the horizon,
#   0 at the rim, 90° at the center.
# - Color by azimuth quadrant: psi 0-90° red, 90-180° green, 180-270° blue,
#   270-360° yellow (RGB below). A 15° x 15° checkerboard in (psi, tau)
#   darkens every other cell to 45 %.
# - A white dot (4° radius) circles at tau 20°, 18° per frame, so the
#   picture changes over time.
#
# Keep the numbers in sync with apps/macos/Sources/NVRView/SelfTest.swift and
# the Windows app's self-test, which sample dewarped pixels against them.
#
# Usage: ./make-fisheye.sh   (rewrites fisheye/h264-fisheye.h264 next to it)
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p fisheye

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

python3 -I - "$tmp/fisheye.yuv" <<'PY'
import math, sys

W = H = 384
FRAMES = 20
CX, CY, R = 0.49 * W, 0.505 * H, 0.46 * W
COLORS = [(220, 40, 40), (40, 180, 60), (50, 70, 220), (230, 200, 40)]


def ycc(rgb):
    r, g, b = (c / 255 for c in rgb)
    y = 16 + 65.481 * r + 128.553 * g + 24.966 * b
    cb = 128 - 37.797 * r - 74.203 * g + 112.0 * b
    cr = 128 + 112.0 * r - 93.786 * g - 18.214 * b
    return y, cb, cr


def direction(i, j):
    """(psi, tau) in degrees for pixel (i, j), or None outside the circle."""
    dx, dy = i + 0.5 - CX, j + 0.5 - CY
    rho = math.hypot(dx, dy) / R
    if rho > 1:
        return None
    theta = rho * 90.0
    phi = math.degrees(math.atan2(dy, dx))
    return (phi + 90.0) % 360.0, 90.0 - theta


def background(d):
    if d is None:
        return (0, 0, 0)
    psi, tau = d
    base = COLORS[int(psi // 90) % 4]
    if (int(psi // 15) + int(tau // 15)) % 2:
        return tuple(round(c * 0.45) for c in base)
    return base


def angle_between(a, b):
    def vec(psi, tau):
        p, t = math.radians(psi), math.radians(tau)
        return (math.sin(p) * math.cos(t), math.sin(t), math.cos(p) * math.cos(t))
    u, v = vec(*a), vec(*b)
    return math.degrees(math.acos(max(-1.0, min(1.0, sum(x * y for x, y in zip(u, v))))))


dirs = [[direction(i, j) for i in range(W)] for j in range(H)]
base = [[background(dirs[j][i]) for i in range(W)] for j in range(H)]
with open(sys.argv[1], "wb") as out:
    for f in range(FRAMES):
        dot = ((f * 18.0) % 360.0, 20.0)
        rgb = [row[:] for row in base]
        for j in range(H):
            for i in range(W):
                d = dirs[j][i]
                if d is not None and abs(d[1] - dot[1]) < 6 and angle_between(d, dot) < 4.0:
                    rgb[j][i] = (255, 255, 255)
        conv = [[ycc(p) for p in row] for row in rgb]
        out.write(bytes(max(0, min(255, round(conv[j][i][0]))) for j in range(H) for i in range(W)))
        for k in (1, 2):
            out.write(bytes(
                max(0, min(255, round(sum(conv[2 * j + a][2 * i + b][k] for a in (0, 1) for b in (0, 1)) / 4)))
                for j in range(H // 2) for i in range(W // 2)))
PY

ffmpeg -v error -y -f rawvideo -pix_fmt yuv420p -s 384x384 -r 20 -i "$tmp/fisheye.yuv" \
  -frames:v 20 -c:v libx264 -preset medium -crf 26 -pix_fmt yuv420p \
  -colorspace smpte170m -color_primaries smpte170m -color_trc smpte170m -color_range tv \
  -x264-params keyint=20:min-keyint=20:scenecut=0:bframes=0:repeat-headers=1:aud=0 \
  -f h264 fisheye/h264-fisheye.h264

/bin/ls -l fisheye/h264-fisheye.h264
