#!/usr/bin/env python3
# Minimal stdlib-only PNG/ICO generator for the Codex Recovery app icon.
import math, struct, zlib, pathlib
from pathlib import Path
N=256
pixels=[]
for y in range(N):
  row=bytearray()
  for x in range(N):
    dx=x-(N-1)/2;dy=y-(N-1)/2
    rounded=abs(dx)<86 or abs(dy)<86 or math.hypot(max(abs(dx)-86,0),max(abs(dy)-86,0))<37
    if not rounded: color=(0,0,0,0)
    else:
      color=(41+int(13*y/N),102+int(15*x/N),232,255)
      rho=math.hypot(dx,dy)
      angle=(math.degrees(math.atan2(dy,dx))+360)%360
      arc=(abs(rho-61)<12 and not(245<angle<320))
      # Arrowhead at the upper-right end of the arc.
      arrow= ((dx-37)/27)**2+((dy+49)/27)**2<1 and (dx+dy)>-28 and dx>24
      if arc or arrow: color=(255,255,255,255)
    row.extend(color)
  pixels.append(b'\x00'+bytes(row))
def chunk(kind,payload):
  return struct.pack('!I',len(payload))+kind+payload+struct.pack('!I',zlib.crc32(kind+payload)&0xffffffff)
png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('!2I5B',N,N,8,6,0,0,0))+chunk(b'IDAT',zlib.compress(b''.join(pixels),9))+chunk(b'IEND',b'')
root=Path(__file__).resolve().parent.parent/'src-tauri'/'icons'
(root/'icon.png').write_bytes(png)
# Modern Windows ICO can contain a PNG payload; 0 dimensions mean 256.
ico=struct.pack('<HHH',0,1,1)+struct.pack('<BBBBHHII',0,0,0,0,1,32,len(png),22)+png
(root/'icon.ico').write_bytes(ico)
print('icon PNG',len(png),'ICO',len(ico))
