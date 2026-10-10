#!/usr/bin/env bash
# Generates the synthetic video fixtures used by apps/macos tests and the demo.
# Everything comes from ffmpeg's built-in `testsrc2` pattern generator: no real
# footage. Requires ffmpeg with libx264 and libx265, and python3.
#
# All files are raw Annex-B elementary streams, 20 fps, no B-frames (so decode
# order equals presentation order), one keyframe every 20 frames, parameter sets
# repeated in front of every keyframe, no access unit delimiters.
#
#   h264-basic.h264        320x240, 60 frames (3 keyframes).
#   h265-basic.h265        320x240, 60 frames (3 keyframes).
#   h264-resize.h264       320x240 x 40 frames, then 480x270 x 40 frames, made by
#                          concatenating two separate encodes: the second starts
#                          with a keyframe carrying new parameter sets.
#   h265-resize.h265       Same for H.265.
#   h264-leading-p.h264    h264-basic with its first access unit's SPS, PPS and
#                          IDR removed, so the stream starts with 19 P-frames
#                          that cannot be decoded; the first usable keyframe is
#                          frame 20 (0-based index 19 of the file).
#   h265-leading-p.h265    Same for H.265.
#   h264-colors.h264       192x96, 10 frames (1 keyframe): six vertical bars,
#                          32x64 px, of known RGB colors (see COLOR_BARS
#                          below, left to right), above a 32 px strip of the
#                          moving test pattern (so the picture changes over
#                          time); converted with the BT.601
#                          matrix to video range and flagged that way in the
#                          VUI (matrix, primaries and transfer SMPTE 170M,
#                          video_full_range_flag 0). Encoded at a very low CRF
#                          so the decoded bar centers stay within a few codes
#                          of the source.
#   h265-colors.h265       Same bars, BT.709 matrix, full range
#                          (video_full_range_flag 1), flagged that way in the
#                          VUI.
#
# Usage: ./make-video.sh   (rewrites the files next to this script)
set -euo pipefail
cd "$(dirname "$0")"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

enc() { # codec size frames out
  local codec=$1 size=$2 frames=$3 out=$4
  local lib fmt params
  if [ "$codec" = h264 ]; then
    lib=libx264; fmt=h264
    params="-x264-params keyint=20:min-keyint=20:scenecut=0:bframes=0:repeat-headers=1:aud=0"
  else
    lib=libx265; fmt=hevc
    params="-x265-params keyint=20:min-keyint=20:scenecut=0:bframes=0:repeat-headers=1:aud=0:log-level=error"
  fi
  ffmpeg -v error -y -f lavfi -i "testsrc2=size=$size:rate=20" -frames:v "$frames" \
    -c:v $lib -preset medium -crf 32 -pix_fmt yuv420p $params -f $fmt "$out"
}

for codec in h264 h265; do
  ext=$codec
  enc $codec 320x240 60 "$tmp/a.$ext"
  enc $codec 320x240 40 "$tmp/b.$ext"
  enc $codec 480x270 40 "$tmp/c.$ext"
  cp "$tmp/a.$ext" "$codec-basic.$ext"
  cat "$tmp/b.$ext" "$tmp/c.$ext" > "$codec-resize.$ext"
  python3 -I - "$codec" "$tmp/a.$ext" "$codec-leading-p.$ext" <<'PY'
import sys
codec, src, dst = sys.argv[1:4]
data = open(src, "rb").read()
starts, i = [], 0
while True:
    i = data.find(b"\x00\x00\x01", i)
    if i < 0:
        break
    starts.append(i)
    i += 3
def nal_type(pos):
    b = data[pos + 3]
    return (b & 0x1F) if codec == "h264" else (b >> 1) & 0x3F
# Drop every NAL up to the first non-keyframe slice (the first access unit's
# parameter sets, SEI and keyframe slice).
for n, pos in enumerate(starts):
    t = nal_type(pos)
    if (codec == "h264" and t == 1) or (codec == "h265" and t < 16):
        cut = pos
        # Keep a 4-byte start code if one precedes this NAL.
        if cut > 0 and data[cut - 1] == 0:
            cut -= 1
        open(dst, "wb").write(data[cut:])
        break
PY
done

# Known-color bars. Keep in sync with apps/macos/Tests/VideoKitTests/ColorTests.swift.
COLOR_BARS="B42828 28A03C 323CBE DCDCDC 808080 181818"

colors() { # codec matrix range out
  local codec=$1 matrix=$2 range=$3 out=$4
  local lib fmt params graph="" labels="" n=0 c
  if [ "$codec" = h264 ]; then
    lib=libx264; fmt=h264
    params="-x264-params keyint=20:min-keyint=20:scenecut=0:bframes=0:repeat-headers=1:aud=0"
  else
    lib=libx265; fmt=hevc
    params="-x265-params keyint=20:min-keyint=20:scenecut=0:bframes=0:repeat-headers=1:aud=0:log-level=error"
  fi
  for c in $COLOR_BARS; do
    graph="${graph}color=c=0x$c:s=32x64:r=20[c$n];"
    labels="$labels[c$n]"
    n=$((n + 1))
  done
  # The tags end up in the VUI colour description.
  local tag=smpte170m
  [ "$matrix" = bt709 ] && tag=bt709
  ffmpeg -v error -y -filter_complex \
    "${graph}${labels}hstack=inputs=$n[bars];testsrc2=size=192x32:rate=20[strip];[bars][strip]vstack,format=gbrp,scale=out_color_matrix=$matrix:out_range=$range,format=yuv420p,setparams=range=$range:color_primaries=$tag:color_trc=$tag:colorspace=$tag" \
    -frames:v 10 -c:v $lib -preset medium -crf 4 \
    -colorspace $tag -color_primaries $tag -color_trc $tag -color_range $range $params -f $fmt "$out"
}

colors h264 bt601 tv h264-colors.h264
colors h265 bt709 pc h265-colors.h265

/bin/ls -l *.h264 *.h265
