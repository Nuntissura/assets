#!/usr/bin/env bash
# Generate synthetic VC-3 (DNxHR/DNxHD) fixtures with ffmpeg 8.1.1 (gyan.dev) as a black-box generator/oracle.
set -euo pipefail
FF="${FFMPEG:-ffmpeg}"
cd "$1"
export TMP="$1" TEMP="$1"
BX="-fflags +bitexact -flags:v +bitexact"
G8="color=c=gray:size=264x120:rate=25,format=yuv422p,geq=lum='clip(20+X*2+Y*1+N*9+2*sin(X*Y/9+N),16,235)':cb='clip(128+X/2-N,16,240)':cr='clip(128-Y/2+N,16,240)'"
G10="color=c=gray:size=264x120:rate=25,format=yuv422p10le,geq=lum='clip(80+X*9+Y*5+N*37+8*sin(X*Y/9+N),64,940)':cb='clip(512+2*X-3*N,64,960)':cr='clip(512-2*Y+5*N,64,960)'"
for p in lb sq hq; do
  "$FF" -nostdin -hide_banner -loglevel error -y -f lavfi -i "$G8" -frames:v 3 -c:v dnxhd -profile:v dnxhr_$p -pix_fmt yuv422p $BX dnxhr-$p-264x120.mov
  "$FF" -nostdin -hide_banner -loglevel error -y -i dnxhr-$p-264x120.mov -f rawvideo dnxhr-$p-264x120.ref.yuv
  "$FF" -nostdin -hide_banner -loglevel error -y -i dnxhr-$p-264x120.mov -c:v copy -f rawvideo dnxhr-$p-264x120.pk
done
"$FF" -nostdin -hide_banner -loglevel error -y -f lavfi -i "$G10" -frames:v 3 -c:v dnxhd -profile:v dnxhr_hqx -pix_fmt yuv422p10le $BX dnxhr-hqx-264x120.mov
"$FF" -nostdin -hide_banner -loglevel error -y -i dnxhr-hqx-264x120.mov -f rawvideo dnxhr-hqx-264x120.ref.yuv
"$FF" -nostdin -hide_banner -loglevel error -y -i dnxhr-hqx-264x120.mov -c:v copy -f rawvideo dnxhr-hqx-264x120.pk
