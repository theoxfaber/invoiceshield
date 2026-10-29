"""Narration: edge-tts, en-US-AndrewMultilingualNeural, one mp3 per segment + word boundaries."""
import json,asyncio,os,sys,edge_tts
for k in list(os.environ):
    if 'proxy' in k.lower(): os.environ.pop(k)
V="en-US-AndrewMultilingualNeural"; R=sys.argv[1] if len(sys.argv)>1 else "+0%"
os.makedirs('vo',exist_ok=True); segs=json.load(open('segs.json')); only=sys.argv[2:]
async def one(k,t):
    for a in range(4):
        try:
            c=edge_tts.Communicate(t,V,rate=R); audio=b''
            async for ch in c.stream():
                if ch['type']=='audio': audio+=ch['data']
            if len(audio)>2000: open(f'vo/{k}.mp3','wb').write(audio); return
        except Exception as e: print(k,'retry',a,type(e).__name__,e); await asyncio.sleep(2+2*a)
    raise SystemExit('tts failed '+k)
async def main():
    for k,t in segs:
        if not only or k in only: await one(k,t)
asyncio.run(main())
