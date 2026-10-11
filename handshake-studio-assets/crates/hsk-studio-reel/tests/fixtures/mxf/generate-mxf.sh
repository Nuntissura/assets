#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes; the committed outputs are the fixtures.
# Run under the artifact root on D: (never C:), with TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-mxf.sh
set -euo pipefail

# 1. OP1a, frame-wrapped MPEG-2 long GOP (I B B P B B P ... two B pictures, GOP 6, so the index carries
#    temporal offsets and the open-GOP I pictures are not random access points) interleaved with
#    PCM 48 kHz stereo in the same essence container (variable edit unit size, two slices).
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=128x72:rate=25:duration=0.4" \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.4" \
  -c:v mpeg2video -g 6 -bf 2 -b:v 200k -pix_fmt yuv422p -c:a pcm_s16le -ac 2 \
  -fflags +bitexact -flags:v +bitexact -f mxf mpeg2-pcm-op1a.mxf

# 2. OPAtom, clip-wrapped PCM 48 kHz mono: one essence element, constant edit unit size (2 bytes), the
#    index table says EditUnitByteCount only (no index entries).
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.1" \
  -ac 1 -c:a pcm_s16le -fflags +bitexact -f mxf_opatom pcm-opatom.mxf

# Oracles (libavformat's view of both files): stream facts and per-packet byte positions.
for f in mpeg2-pcm-op1a pcm-opatom; do
  ffprobe -v error -show_entries stream=index,codec_name,codec_type,width,height,r_frame_rate,sample_rate,channels,duration_ts,time_base:format=format_name,size -of compact "$f.mxf" | sed "s/^/$f /"
  ffprobe -v error -show_packets -show_entries packet=stream_index,pts,dts,duration,size,pos,flags -of csv=p=0 "$f.mxf" | sed "s/^/$f packet /"
done
