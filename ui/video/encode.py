#!/usr/bin/env python3
"""Turn the screencast frames (frames.json, variable timing) + vo/narration.wav into final.mp4 (1080p30 H.264 + AAC,
soft English subtitle track), plus poster.jpg. Usage: python3 encode.py OUTDIR"""
import json, subprocess, sys, pathlib
W = pathlib.Path(__file__).resolve().parent; OUT = pathlib.Path(sys.argv[1]); OUT.mkdir(parents=True, exist_ok=True)
d = json.load(open(W / 'frames.json')); st = d['start']; T = d['total']; fr = d['frames']
pre = [f for f in fr if f[0] <= st]; post = [f for f in fr if f[0] > st]
fr = ([(st, pre[-1][1])] if pre else []) + post
L = []
for i in range(len(fr)):
    nxt = fr[i + 1][0] if i + 1 < len(fr) else st + T
    L.append(f"file 'frames/{fr[i][1]}'\nduration {max(0.001, nxt - fr[i][0]):.4f}")
L.append(f"file 'frames/{fr[-1][1]}'"); (W / 'concat.txt').write_text('\n'.join(L) + '\n')
subprocess.run(['ffmpeg', '-y', '-v', 'error', '-f', 'concat', '-safe', '0', '-i', str(W / 'concat.txt'), '-i', str(W / 'vo/narration.wav'),
                '-i', str(W / 'captions.srt'), '-map', '0:v', '-map', '1:a', '-map', '2:s',
                '-vf', 'fps=30,scale=1920:1080:flags=lanczos,format=yuv420p', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '18',
                '-profile:v', 'high', '-c:a', 'aac', '-b:a', '192k', '-ar', '48000', '-c:s', 'mov_text', '-metadata:s:s:0', 'language=eng',
                '-t', str(T), '-movflags', '+faststart', str(OUT / 'final.mp4')], check=True, cwd=W)
import subprocess as sp
D=float(sp.check_output(["ffprobe","-v","error","-show_entries","format=duration","-of","csv=p=0",str(OUT/"final.mp4")]))
assert D>T-0.5, f"short encode {D}"
print('ok', T)
