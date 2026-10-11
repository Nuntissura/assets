# hsk-studio-reel test fixtures

All fixtures are synthetic (no third-party media, no personal data), each below 200 KB. They were authored ONCE, offline, under the
artifact root on D: with `TMP`/`TEMP` there as well; the committed bytes are the fixtures. ffmpeg/ffprobe 8.1.1 (gyan.dev full build) are
validator-only oracles and generators: they are never a runtime or test-time dependency, and no test runs them.

| Directory | Fixture | Generator | Oracle |
|---|---|---|---|
| `av1/` | `av1-16x16-3f.{mp4,mkv}`, `av1-1920x1080-2f.mp4` | `av1/generate.sh` | libdav1d per-frame SHA-256 (`av1/provenance.json`) |
| `mkv/` | `pcm-*.mkv` (5 files, A_PCM/INT/LIT, INT/BIG, FLOAT/IEEE, 8/16/24/32 bit) | `mkv/generate-pcm.sh` (ffmpeg lavfi `sine` -> matroska) | libavcodec raw decode SHA-256 and libavformat packet positions, printed by the script; values are pinned in `tests/mkv_content_pcm.rs` |
| `mkv/` | `enc-zlib.mkv`, `enc-headerstrip.mkv`, `enc-stacked-private.mkv`, `enc-zlib-bomb.mkv` (ContentEncodings) | `mkv/make_encoded_mkv.py` (Python `zlib`, byte-level EBML from RFC 9559) | `ffprobe -show_packets -show_data_hash sha256` for the single-encoding files (ffmpeg undoes one encoding per track), Python's own frames for the stacked file |
| `mxf/` | `mxf-*.mxf` (OP1a) | `mxf/generate-mxf.sh` | `ffprobe -show_packets` byte positions, pts/dts and sync flags; values pinned in `tests/mxf_index.rs` |
| `mov/` | `mov-h264-bf2-negcts-tc-df-64x36.mov` (H.264 with B-frames, version 1 `ctts` with negative offsets, edit list, drop-frame `tmcd` track; 4197 bytes) | `mov/generate-mov.sh` (ffmpeg lavfi `testsrc2` + libx264, byte-reproducible) | `ffprobe -show_entries packet=pts,dts,flags` and `-show_packets -show_data` of the timecode track, recorded in `mov/provenance.json`; values pinned in `tests/engine_video.rs` |

## Exact commands

PCM Matroska (run from `$GEN` on D:):

    bash generate-pcm.sh

which runs, per file, `ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=<rate>:duration=0.1" -ac <ch> -c:a <pcm codec> -fflags +bitexact -f matroska <name>.mkv`
and the two oracle commands recorded in the script.

ContentEncodings Matroska (run from `$GEN` on D:):

    python make_encoded_mkv.py

Verification used when the fixtures were made (both agree with the frames Python generated):

    ffprobe -v error -show_packets -show_data_hash sha256 -show_entries packet=pts,size,pos,data_hash -of compact enc-zlib.mkv
    ffprobe -v error -show_packets -show_data_hash sha256 -show_entries packet=pts,size,pos,data_hash -of compact enc-headerstrip.mkv

MXF (run from `$GEN` on D:): `bash generate-mxf.sh`. It runs the two ffmpeg commands recorded in the script (OP1a: mpeg2video `-g 6 -bf 2` + `pcm_s16le`, `-f mxf`; OP-Atom: mono `pcm_s16le`, `-f mxf_opatom`) and prints the ffprobe `-show_packets` oracle that `tests/mxf_index.rs` pins.

MOV (run from `$GEN` on D:): `bash generate-mov.sh`. One ffmpeg command (recorded in the script and in `mov/provenance.json`) writes the file; the two ffprobe oracle commands follow it. Regenerating with the same ffmpeg build gave the same sha256.
