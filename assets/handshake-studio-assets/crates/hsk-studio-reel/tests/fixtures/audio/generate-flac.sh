#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes (its own FLAC encoder, LPC subframes); the committed outputs
# are the fixtures. Source content is synthetic (lavfi sine), no third-party media.
# Run under the artifact root on D: (never C:), with TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-flac.sh
set -euo pipefail
src2() { echo "sine=frequency=$1:sample_rate=$3:duration=$4[a];sine=frequency=$2:sample_rate=$3:duration=$4[b];[a][b]amerge=inputs=2"; }
# 16-bit stereo 44.1 kHz, compression level 8 (LPC up to order 12, mid/side), 0.25 s.
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(src2 440 997 44100 0.25)" -c:a flac -compression_level 8 \
  -sample_fmt s16 -fflags +bitexact -flags:a +bitexact flac-s16-44k1-2ch.flac
# 24-bit mono 48 kHz, compression level 5, 0.2 s.
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=1234:sample_rate=48000:duration=0.2" -c:a flac \
  -compression_level 5 -sample_fmt s32 -bits_per_raw_sample 24 -fflags +bitexact -flags:a +bitexact flac-s24-48k-1ch.flac
# Oracle: libavcodec's decode of each file as raw little-endian integer PCM (s16le / s24le packed by -f s24le).
for f in flac-s16-44k1-2ch:s16le flac-s24-48k-1ch:s24le; do
  name=${f%%:*}; fmt=${f##*:}
  ffmpeg -hide_banner -loglevel error -y -i "$name.flac" -f "$fmt" -c:a "pcm_$fmt" - | sha256sum | sed "s/^/$name ref-$fmt-sha256 /"
  ffmpeg -hide_banner -loglevel error -y -i "$name.flac" -f "$fmt" -c:a "pcm_$fmt" - | wc -c | sed "s/^/$name ref-bytes /"
  sha256sum "$name.flac"; wc -c < "$name.flac"
done
