#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev REMUXES (-c copy, no re-encode) the elementary-stream fixtures of this directory into
# one MP4 (AAC, FLAC, Opus, MP3 tracks) and one Matroska file (AAC, FLAC, Opus, Vorbis, MP3 tracks), so a container decode
# can be compared with the standalone decode of the same coded bytes. Run in this directory's copy under the artifact root
# on D: (never C:), TMP/TEMP there too.
set -euo pipefail
BE="-fflags +bitexact -flags:a +bitexact"
ffmpeg -hide_banner -loglevel error -y -i aac-lc-44k1-2ch.aac -i flac-s16-44k1-2ch.flac -i opus-48k-2ch.opus -i mp3-44k1-2ch-cbr.mp3 \
  -map 0:a -map 1:a -map 2:a -map 3:a -c copy -strict experimental $BE -f mp4 audio-tracks.mp4
ffmpeg -hide_banner -loglevel error -y -i aac-lc-44k1-2ch.aac -i flac-s16-44k1-2ch.flac -i opus-48k-2ch.opus -i vorbis-44k1-2ch.ogg \
  -i mp3-44k1-2ch-cbr.mp3 -map 0:a -map 1:a -map 2:a -map 3:a -map 4:a -c copy $BE -f matroska audio-tracks.mkv
sha256sum audio-tracks.mp4 audio-tracks.mkv; wc -c audio-tracks.mp4 audio-tracks.mkv
ffprobe -v error -show_entries stream=index,codec_name,codec_tag_string,sample_rate,channels,start_pts,duration_ts:stream_tags= -of compact audio-tracks.mp4
ffprobe -v error -show_entries stream=index,codec_name,sample_rate,channels -of compact audio-tracks.mkv
