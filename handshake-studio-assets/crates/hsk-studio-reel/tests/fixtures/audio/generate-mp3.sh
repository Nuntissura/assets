#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes (libmp3lame for Layer III with a Xing/Info + LAME tag; ffmpeg's
# mp2 encoder for Layer II); synthetic lavfi sine input, no third-party media. Run under the artifact root on D: (never C:),
# TMP/TEMP there too:  mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-mp3.sh
set -euo pipefail
BE="-fflags +bitexact -flags:a +bitexact"
two() { echo "sine=frequency=$1:sample_rate=$3:duration=$4[a];sine=frequency=$2:sample_rate=$3:duration=$4[b];[a][b]amerge=inputs=2"; }
# MPEG-1 Layer III joint stereo, 44.1 kHz, CBR 128 kb/s (Info + LAME tag with encoder delay / padding).
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 440 1250 44100 0.3)" -c:a libmp3lame -b:a 128k $BE mp3-44k1-2ch-cbr.mp3
# MPEG-2 LSF Layer III mono, 22.05 kHz, VBR (Xing + LAME tag), short blocks from a click train mixed in.
ffmpeg -hide_banner -loglevel error -y -filter_complex "sine=frequency=700:sample_rate=22050:duration=0.4[a];aevalsrc='if(lt(mod(n,2205),20),0.8,0)':s=22050:d=0.4[b];[a][b]amix=inputs=2" \
  -ac 1 -c:a libmp3lame -q:a 4 $BE mp3-22k05-1ch-vbr.mp3
# MPEG-1 Layer II stereo, 48 kHz, 192 kb/s (no Xing frame).
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 330 880 48000 0.25)" -c:a mp2 -b:a 192k $BE -f mp2 mp2-48k-2ch.mp2
# Oracles: libavcodec mp3float / mp2float decode (gapless trimming by libavformat from the LAME tag) as raw s16le,
# zlib-compressed (Python zlib level 9) as FILE.ref-s16.zz.
for f in mp3-44k1-2ch-cbr.mp3 mp3-22k05-1ch-vbr.mp3 mp2-48k-2ch.mp2; do
  ffmpeg -hide_banner -loglevel error -y -i "$f" -f s16le -c:a pcm_s16le "$f.ref-s16"
  python -c "import sys,zlib;d=open(sys.argv[1],'rb').read();open(sys.argv[1]+'.zz','wb').write(zlib.compress(d,9))" "$f.ref-s16"
done
sha256sum *.mp3 *.mp2 *.ref-s16; wc -c *.mp3 *.mp2 *.ref-s16 *.ref-s16.zz
