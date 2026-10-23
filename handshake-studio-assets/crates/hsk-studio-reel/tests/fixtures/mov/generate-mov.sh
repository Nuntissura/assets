#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev (libx264) authors the bytes; the committed output is the fixture.
# Run under the artifact root on D: (never C:), with TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-mov.sh
# Byte-reproducible: regenerating with the same ffmpeg gives the sha256 recorded in provenance.json.
set -euo pipefail

# 24 frames, 29.97 fps, 64x36, H.264 High with two B-frames and closed 12-frame GOPs, version 1 ctts with
# negative composition offsets, one edit-list entry, and a drop-frame timecode track starting 00:59:59;29.
ffmpeg -hide_banner -y -f lavfi -i "testsrc2=size=64x36:rate=30000/1001" -frames:v 24 \
  -c:v libx264 -preset veryfast -profile:v high -pix_fmt yuv420p -bf 2 -g 12 \
  -x264-params "threads=1:open-gop=0:scenecut=0:no-info=1" \
  -fflags +bitexact -flags:v +bitexact -movflags +negative_cts_offsets \
  -timecode "00:59:59;29" mov-h264-bf2-negcts-tc-df-64x36.mov

# Oracles (libavformat's view of the file): composition times and key flags of the video packets, and the
# raw bytes of the timecode sample. ffprobe's dts column is shifted by -1001 and is not used.
ffprobe -v error -select_streams v:0 -show_entries packet=pts,dts,flags -of csv=p=0 mov-h264-bf2-negcts-tc-df-64x36.mov
ffprobe -v error -select_streams 1 -show_packets -show_data mov-h264-bf2-negcts-tc-df-64x36.mov
