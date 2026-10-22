#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg is used only to author the bytes; the committed outputs are the fixtures.
# Run under the artifact root on D: (never C:), with TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate.sh
# Tool: ffmpeg 8.1.1-full_build-www.gyan.dev (libaom-av1 encoder, libdav1d oracle decoder).
set -euo pipefail
# 16x16 4:2:0 8-bit, 3 frames @24 fps, one GOP (K, P, P), distinct per-frame content, BT.709 matrix tag.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=black:s=16x16:r=24,format=yuv420p,geq=lum='mod(X*13+Y*7+N*53,256)':cb='mod(X*5+N*31+64,256)':cr='mod(Y*9+N*17+32,256)'" \
  -frames:v 3 -vf format=yuv420p -c:v libaom-av1 -cpu-used 8 -crf 30 -b:v 0 -g 3 -lag-in-frames 0 -threads 1 \
  -pix_fmt yuv420p -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv -an \
  -fflags +bitexact -flags:v +bitexact -movflags +faststart av1-16x16-3f.mp4
# Same coded bitstream remuxed to Matroska (CodecID V_AV1, CodecPrivate = av1C).
ffmpeg -hide_banner -loglevel error -y -i av1-16x16-3f.mp4 -c copy -fflags +bitexact av1-16x16-3f.mkv
# Oracle: per-frame SHA-256 of the raw planar yuv420p frames decoded by dav1d (normative AV1 output).
ffmpeg -hide_banner -loglevel error -c:v libdav1d -i av1-16x16-3f.mp4 -f framehash -hash sha256 -

# ---- 1080p section (STUDIO-R5 SMB-REEL-ENG-3 regression), run once, same env as above -----------------
# 1920x1080 4:2:0 8-bit, 2 frames (K, P), synthetic geq pattern; libaom crf 45 keeps the file at ~11 KB.
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "color=c=black:s=1920x1080:r=24,format=yuv420p,geq=lum='mod(X/6+Y/5+N*37,256)':cb='mod(X/16+N*29+64,256)':cr='mod(Y/16+N*13+32,256)'" \
  -frames:v 2 -vf format=yuv420p -c:v libaom-av1 -cpu-used 8 -crf 45 -b:v 0 -g 2 -lag-in-frames 0 -threads 1 \
  -pix_fmt yuv420p -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv -an \
  -fflags +bitexact -flags:v +bitexact -movflags +faststart av1-1920x1080-2f.mp4
# Oracle: per-frame SHA-256 of the raw planar frames decoded by dav1d.
ffmpeg -hide_banner -loglevel error -c:v libdav1d -i av1-1920x1080-2f.mp4 -f framehash -hash sha256 -
