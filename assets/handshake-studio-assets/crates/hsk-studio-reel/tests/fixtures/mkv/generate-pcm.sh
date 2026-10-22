#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes; the committed outputs are the fixtures.
# Run under the artifact root on D: (never C:), with TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-pcm.sh
set -euo pipefail
gen() { # name, sample_rate, channels, pcm codec, duration, raw format
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=440:sample_rate=$2:duration=$5" \
    -ac "$3" -c:a "$4" -fflags +bitexact -f matroska "$1.mkv"
  # Oracle 1: raw interleaved little-endian PCM as libavcodec decodes it (hash + byte count).
  ffmpeg -hide_banner -loglevel error -y -i "$1.mkv" -f "$6" -c:a "pcm_$6" - | sha256sum | sed "s/^/$1 raw-sha256 /"
  ffmpeg -hide_banner -loglevel error -y -i "$1.mkv" -f "$6" -c:a "pcm_$6" - | wc -c | sed "s/^/$1 raw-bytes /"
  # Oracle 2: block (SimpleBlock) positions and timestamps as libavformat demuxes them.
  ffprobe -v error -show_entries packet=pts,size,pos -of csv=p=0 "$1.mkv" | sed "s/^/$1 packet /"
}
gen pcm-s16le-48k-2ch 48000 2 pcm_s16le 0.1 s16le
gen pcm-s16be-48k-2ch 48000 2 pcm_s16be 0.1 s16le
gen pcm-s24le-48k-2ch 48000 2 pcm_s24le 0.1 s24le
gen pcm-f32le-44k1-1ch 44100 1 pcm_f32le 0.1 f32le
gen pcm-u8-48k-1ch 48000 1 pcm_u8 0.1 u8
