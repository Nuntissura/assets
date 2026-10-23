#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes (libvorbis, libopus and ffmpeg's FLAC encoder, Ogg muxer);
# synthetic lavfi sine input, no third-party media. Run under the artifact root on D: (never C:), TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-ogg.sh
set -euo pipefail
BE="-fflags +bitexact -flags:a +bitexact"
two() { echo "sine=frequency=$1:sample_rate=$3:duration=$4[a];sine=frequency=$2:sample_rate=$3:duration=$4[b];[a][b]amerge=inputs=2"; }
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 440 1250 44100 0.25)" -c:a libvorbis -q:a 4 $BE vorbis-44k1-2ch.ogg
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 440 1250 48000 0.25)" -c:a libopus -b:a 96k $BE opus-48k-2ch.opus
ffmpeg -hide_banner -loglevel error -y -filter_complex \
  "sine=f=300:r=48000:d=0.2[a];sine=f=500:r=48000:d=0.2[b];sine=f=700:r=48000:d=0.2[c];sine=f=60:r=48000:d=0.2[d];sine=f=900:r=48000:d=0.2[e];sine=f=1100:r=48000:d=0.2[f];[a][b][c][d][e][f]amerge=inputs=6,aformat=channel_layouts=5.1" \
  -c:a libopus -b:a 192k -mapping_family 1 $BE opus-48k-6ch.opus
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=880:sample_rate=32000:duration=0.25" -c:a flac -sample_fmt s16 $BE -f ogg flac-in-ogg-32k-1ch.oga
# Oracles: raw s16le of libavcodec's decode (native vorbis decoder; libopus decoder for Opus; native flac), zlib-compressed
# with Python's zlib (level 9) as FILE.ref-s16.zz.
dec() { ffmpeg -hide_banner -loglevel error -y $2 -i "$1" -f s16le -c:a pcm_s16le "$1.ref-s16"; }
dec vorbis-44k1-2ch.ogg ""
dec opus-48k-2ch.opus "-c:a libopus"
dec opus-48k-6ch.opus "-c:a libopus"
dec flac-in-ogg-32k-1ch.oga ""
for f in *.ref-s16; do python -c "import sys,zlib;d=open(sys.argv[1],'rb').read();open(sys.argv[1]+'.zz','wb').write(zlib.compress(d,9))" "$f"; done
sha256sum *.ogg *.opus *.oga *.ref-s16; wc -c *.ogg *.opus *.oga *.ref-s16 *.zz
