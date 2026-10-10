#!/usr/bin/env bash
# Generates the synthetic audio fixtures used by crates/audio-pipeline and
# tools/mock-nvr. Everything comes from ffmpeg's `aevalsrc` expression
# generator: no recorded sound. Requires ffmpeg with its native AAC encoder.
#
# Each .aac file is a raw ADTS stream (one AAC-LC frame of 1024 samples per
# ADTS frame, no CRC), which is what a device sends as one frame per audio
# record. FFmpeg's encoder puts 1024 samples of priming in front, so decoded
# sample n + 1024 is source sample n. The .s16 files are FFmpeg's own decode
# of the matching .aac (signed 16-bit little-endian, interleaved), the
# reference the decoder tests compare against.
#
#   aac-tone.aac          16 kHz mono, 2 s: 440 Hz at 0.3 plus 1250 Hz at 0.2.
#   aac-tone.s16          FFmpeg's decode of aac-tone.aac.
#   aac-clicks.aac        16 kHz mono, 4 s of silence with a click every
#                         0.5 s starting at 0.25 s (source samples 4000,
#                         12000, ...): a 4 ms, 2 kHz burst at 0.8 under a
#                         sine-squared envelope, so its energy peaks 2 ms
#                         after it starts.
#   aac-stereo-48k.aac    48 kHz stereo, 0.5 s: 440 Hz left, 660 Hz right,
#                         each at 0.4 (a configuration other than the
#                         devices' 16 kHz mono).
#   aac-stereo-48k.s16    FFmpeg's decode of aac-stereo-48k.aac.
#
# Usage: ./make-audio.sh   (rewrites the files next to this script)
set -euo pipefail
cd "$(dirname "$0")"

enc() { # expression rate channels seconds bitrate out
  local expr=$1 rate=$2 ch=$3 secs=$4 rate_bits=$5 out=$6
  ffmpeg -v error -y -f lavfi -i "aevalsrc=exprs=$expr:s=$rate:d=$secs" \
    -c:a aac -profile:a aac_low -b:a "$rate_bits" -ac "$ch" -ar "$rate" \
    -flags +bitexact -fflags +bitexact -f adts "$out"
}

ref() { # in channels out
  ffmpeg -v error -y -flags +bitexact -i "$1" -ac "$2" -f s16le -c:a pcm_s16le "$3"
}

enc '0.3*sin(2*PI*440*t)+0.2*sin(2*PI*1250*t)' 16000 1 2 32k aac-tone.aac
ref aac-tone.aac 1 aac-tone.s16

# mod(t+0.25,0.5) is 0 at 0.25 s, 0.75 s, ...
click='if(lt(mod(t+0.25\,0.5)\,0.004)\,0.8*sin(2*PI*2000*t)*pow(sin(PI*mod(t+0.25\,0.5)/0.004)\,2)\,0)'
enc "$click" 16000 1 4 32k aac-clicks.aac

enc '0.4*sin(2*PI*440*t)|0.4*sin(2*PI*660*t)' 48000 2 0.5 96k aac-stereo-48k.aac
ref aac-stereo-48k.aac 2 aac-stereo-48k.s16

/bin/ls -l aac-*
