#!/usr/bin/env python3
"""Lay the narration segments (vo/*.mp3 from tts.py) on a timeline, mix, normalise to -16 LUFS, write captions.
Outputs: vo/timeline.json, vo/narration.wav, captions.srt, script.txt"""
import json, re, subprocess, pathlib
W = pathlib.Path(__file__).resolve().parent; V = W / 'vo'; (V / 'wav').mkdir(parents=True, exist_ok=True)
segs = json.load(open(W / 'segs.json'))
LEAD, TAIL = 0.8, 2.0
GAP = {'s02': .9, 's03': 1.0, 's04': 1.4, 's05': .6, 's06': 1.4, 's07': .9, 's08': 1.4, 's09': .6, 's10': .9, 's11': 1.0, 's12': 1.0}
dur = lambda f: float(subprocess.check_output(['ffprobe', '-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', str(f)]))
starts, durs, t = {}, {}, LEAD
for k, _ in segs:
    w = V / 'wav' / f'{k}.wav'
    subprocess.run(['ffmpeg', '-y', '-v', 'error', '-i', str(V / f'{k}.mp3'), '-af',
                    'silenceremove=start_periods=1:start_threshold=-50dB,areverse,silenceremove=start_periods=1:start_threshold=-50dB,areverse',
                    '-ar', '48000', '-ac', '1', str(w)], check=True)
    t += GAP.get(k, 0); starts[k] = round(t, 3); durs[k] = round(dur(w), 3); t += durs[k]
total = round(t + TAIL, 3)
json.dump({'starts': starts, 'dur': durs, 'total': total}, open(V / 'timeline.json', 'w'), indent=1)
# mix: delay each segment to its start, sum, then two-pass loudnorm to -16 LUFS / -1.5 dBTP
ins, fl = [], []
for i, (k, _) in enumerate(segs):
    ins += ['-i', str(V / 'wav' / f'{k}.wav')]; ms = int(starts[k] * 1000); fl.append(f'[{i}]adelay={ms}|{ms}[a{i}]')
fc = ';'.join(fl) + ';' + ''.join(f'[a{i}]' for i in range(len(segs))) + f'amix=inputs={len(segs)}:normalize=0,apad=whole_dur={total},atrim=0:{total}[m]'
raw = V / 'mix_raw.wav'
subprocess.run(['ffmpeg', '-y', '-v', 'error', *ins, '-filter_complex', fc, '-map', '[m]', '-ar', '48000', '-ac', '2', str(raw)], check=True)
m = subprocess.run(['ffmpeg', '-hide_banner', '-i', str(raw), '-af', 'loudnorm=I=-16:TP=-1.5:LRA=11:print_format=json', '-f', 'null', '-'], capture_output=True, text=True).stderr
j = json.loads(m[m.rfind('{'):m.rfind('}') + 1])
ln = (f"loudnorm=I=-16:TP=-1.5:LRA=11:measured_I={j['input_i']}:measured_TP={j['input_tp']}:measured_LRA={j['input_lra']}"
      f":measured_thresh={j['input_thresh']}:offset={j['target_offset']}:linear=true")
subprocess.run(['ffmpeg', '-y', '-v', 'error', '-i', str(raw), '-af', ln, '-ar', '48000', str(V / 'narration.wav')], check=True)
# captions: split each segment into sentences/clauses, time by character share
def ts(x): return f"{int(x // 3600):02d}:{int(x % 3600 // 60):02d}:{x % 60:06.3f}".replace('.', ',')
cues = []
for k, txt in segs:
    parts = [p.strip() for p in re.split(r'(?<=[.:?;])\s+', txt) if p.strip()]
    mg = []
    for p in parts:
        if mg and (len(p) < 24 or len(mg[-1]) < 24) and len(mg[-1]) + len(p) < 84: mg[-1] += ' ' + p
        else: mg.append(p)
    tot = sum(len(p) + 6 for p in mg); t0 = starts[k]
    for p in mg:
        d = durs[k] * (len(p) + 6) / tot; cues.append((t0, t0 + d - 0.04, p)); t0 += d
with open(W / 'captions.srt', 'w') as f:
    for i, (a, b, p) in enumerate(cues, 1): f.write(f'{i}\n{ts(a)} --> {ts(b)}\n{p}\n\n')
with open(W / 'script.txt', 'w') as f:
    f.write('InvoiceShield submission video narration (Microsoft Edge neural voice en-US-AndrewMultilingualNeural, rate +5%, via edge-tts)\n\n')
    for k, txt in segs: f.write(f'[{starts[k]:6.1f}s] {txt}\n\n')
    f.write(f'[{total:6.1f}s] end\n')
print('total', total, starts)
