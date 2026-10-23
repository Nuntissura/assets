#!/usr/bin/env bash
# ProRes 4444 (no alpha) fixture and the encoder sources for the ProRes fixtures (ffmpeg as black-box generator).
set -euo pipefail
FF="${FFMPEG:-ffmpeg}"
cd "$1"
export TMP="$1" TEMP="$1"
G422="color=c=gray:size=40x24:rate=25,format=yuv422p10le,geq=lum='clip(80+X*9+Y*5+N*37+8*sin(X*Y/9+N),64,940)':cb='clip(512+2*X-3*N,64,960)':cr='clip(512-2*Y+5*N,64,960)'"
G444="color=c=gray:size=40x24:rate=25,format=yuv444p10le,geq=lum='clip(80+X*9+Y*5+N*37+8*sin(X*Y/9+N),64,940)':cb='clip(512+2*X-3*N,64,960)':cr='clip(512-2*Y+5*N,64,960)'"
"$FF" -nostdin -hide_banner -loglevel error -y -f lavfi -i "$G444" -frames:v 4 -c:v prores_ks -profile:v 4 -pix_fmt yuv444p10le -mbs_per_slice 2 -fflags +bitexact -vendor apl0 -flags:v +bitexact prores-4444-40x24.mov
"$FF" -nostdin -hide_banner -loglevel error -y -i prores-4444-40x24.mov -f rawvideo prores-4444-40x24.ref.yuv
"$FF" -nostdin -hide_banner -loglevel error -y -f lavfi -i "$G422" -frames:v 4 -f rawvideo source-422-10bit-40x24.yuv
"$FF" -nostdin -hide_banner -loglevel error -y -f lavfi -i "$G444" -frames:v 4 -f rawvideo source-444-10bit-40x24.yuv
"$FF" -nostdin -hide_banner -loglevel error -i prores-4444-40x24.mov -f null - 2>&1 | head -2 || true
