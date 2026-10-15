#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors AIFF / AIFF-C files from synthetic lavfi sine input (no third-party media).
# Oracle: sha256 of libavcodec's decode of each file as raw f32le (exact for integer PCM and G.711: power-of-two scaling).
set -euo pipefail
BE="-fflags +bitexact -flags:a +bitexact"
two() { echo "sine=frequency=$1:sample_rate=$3:duration=$4[a];sine=frequency=$2:sample_rate=$3:duration=$4[b];[a][b]amerge=inputs=2"; }
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 440 1250 44100 0.1)" -c:a pcm_s16be $BE aiff-s16-44k1-2ch.aiff
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=0.1" -c:a pcm_s24be $BE aiff-s24-48k-1ch.aiff
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 300 700 48000 0.1)" -c:a pcm_f32be $BE aifc-f32-48k-2ch.aifc
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=500:sample_rate=8000:duration=0.1" -c:a pcm_mulaw $BE -f aiff aifc-ulaw-8k-1ch.aifc
ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=500:sample_rate=8000:duration=0.1" -c:a pcm_alaw $BE -f aiff aifc-alaw-8k-1ch.aifc
for f in aiff-s16-44k1-2ch.aiff aiff-s24-48k-1ch.aiff aifc-f32-48k-2ch.aifc aifc-ulaw-8k-1ch.aifc aifc-alaw-8k-1ch.aifc; do
  ffmpeg -hide_banner -loglevel error -y -i "$f" -f f32le -c:a pcm_f32le - | sha256sum | sed "s/^/$f ref-f32le-sha256 /"
  ffmpeg -hide_banner -loglevel error -y -i "$f" -f f32le -c:a pcm_f32le - | wc -c | sed "s/^/$f ref-bytes /"
  sha256sum "$f"; wc -c < "$f"
done
