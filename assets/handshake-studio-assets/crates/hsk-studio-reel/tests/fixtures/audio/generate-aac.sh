#!/usr/bin/env bash
# OFFLINE fixture generator, run ONCE (never by tests, never a product dependency).
# ffmpeg 8.1.1-full_build-www.gyan.dev authors the bytes (its native AAC-LC encoder, ADTS muxer); synthetic lavfi sine
# input, no third-party media. Run under the artifact root on D: (never C:), TMP/TEMP there too:
#   mkdir -p "$GEN" && cd "$GEN" && export TMP="$GEN" TEMP="$GEN" TMPDIR="$GEN" && bash generate-aac.sh
set -euo pipefail
BE="-fflags +bitexact -flags:a +bitexact"
two() { echo "sine=frequency=$1:sample_rate=$3:duration=$4[a];sine=frequency=$2:sample_rate=$3:duration=$4[b];[a][b]amerge=inputs=2"; }
# AAC-LC stereo 44.1 kHz 128 kb/s, ADTS. Perceptual noise substitution off (-aac_pns 0): PNS bands are random noise by
# definition (ISO/IEC 14496-3 4.6.13), so two conforming decoders differ there and a sample oracle would not hold.
ffmpeg -hide_banner -loglevel error -y -filter_complex "$(two 440 1250 44100 0.3)" -c:a aac -aac_pns 0 -b:a 128k $BE -f adts aac-lc-44k1-2ch.aac
# AAC-LC mono 48 kHz 64 kb/s with a click train (short windows, TNS), ADTS.
ffmpeg -hide_banner -loglevel error -y -filter_complex "sine=frequency=600:sample_rate=48000:duration=0.3[a];aevalsrc='if(lt(mod(n,4800),24),0.7,0)':s=48000:d=0.3[b];[a][b]amix=inputs=2" \
  -ac 1 -c:a aac -aac_pns 0 -b:a 64k $BE -f adts aac-lc-48k-1ch.aac
# AAC-LC 5.1 48 kHz 256 kb/s (channel configuration 6: SCE, CPE, CPE, LFE), ADTS.
ffmpeg -hide_banner -loglevel error -y -filter_complex   "sine=f=300:r=48000:d=0.1[a];sine=f=500:r=48000:d=0.1[b];sine=f=700:r=48000:d=0.1[c];sine=f=60:r=48000:d=0.1[d];sine=f=900:r=48000:d=0.1[e];sine=f=1100:r=48000:d=0.1[f];[a][b][c][d][e][f]amerge=inputs=6,aformat=channel_layouts=5.1"   -c:a aac -aac_pns 0 -b:a 256k $BE -f adts aac-lc-48k-6ch.aac
# Oracles: raw s16le of libavcodec's native AAC decoder, zlib level 9 (Python zlib) as FILE.ref-s16.zz.
for f in aac-lc-44k1-2ch.aac aac-lc-48k-1ch.aac aac-lc-48k-6ch.aac; do
  ffmpeg -hide_banner -loglevel error -y -i "$f" -f s16le -c:a pcm_s16le "$f.ref-s16"
  python -c "import sys,zlib;d=open(sys.argv[1],'rb').read();open(sys.argv[1]+'.zz','wb').write(zlib.compress(d,9))" "$f.ref-s16"
done
sha256sum aac-lc-*.aac aac-lc-*.ref-s16; wc -c aac-lc-*.aac aac-lc-*.ref-s16 aac-lc-*.ref-s16.zz
