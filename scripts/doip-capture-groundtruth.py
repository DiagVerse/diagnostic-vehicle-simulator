"""Independent ground truth from a DoIP packet capture.

Usage:  python3 scripts/doip-capture-groundtruth.py <capture.pcapng>

Deliberately shares no code with the engine: it walks the pcapng blocks itself, reassembles the
TCP streams touching port 13400 by sequence number, and walks the DoIP headers by hand. The
point is to have something to check the engine's answer against that cannot be wrong in the same
way the engine is.

It stops at the first malformed header in a stream rather than resynchronising. Resyncing finds
"messages" in the middle of payloads and reports payload types nobody sent — an earlier version
of this script did exactly that and invented four.
"""

import struct, collections
import sys
f=open(sys.argv[1] if len(sys.argv)>1 else "/Users/sri/Downloads/Reprolog3.pcapng","rb"); data=f.read()
off=0; pkts=[]
while off+12<=len(data):
    bt,bl=struct.unpack_from("<II",data,off)
    if bl<12 or off+bl>len(data): break
    if bt==0x6:
        cl=struct.unpack_from("<I",data,off+20)[0]
        pkts.append(data[off+28:off+28+cl])
    off+=bl

# Only streams touching 13400 (the DoIP port), reassembled by TCP sequence number.
segs=collections.defaultdict(dict)
for p in pkts:
    if len(p)<34 or p[12:14]!=b"\x08\x00" or p[23]!=6: continue
    ihl=(p[14]&0x0F)*4; o=14+ihl
    if len(p) < o+20: continue
    sp,dp=struct.unpack_from(">HH",p,o)
    if 13400 not in (sp,dp): continue
    seq=struct.unpack_from(">I",p,o+4)[0]
    doff=(p[o+12]>>4)*4
    if len(p) < o+doff: continue
    pl=p[o+doff:]
    if not pl: continue
    src=".".join(map(str,p[26:30])); dst=".".join(map(str,p[30:34]))
    segs[(src,sp,dst,dp)][seq]=pl

NAMES={0x0000:"GenericHeaderNack",0x0001:"VehicleIdRequest",0x0004:"VehicleAnnouncement",
       0x0005:"RoutingActivationRequest",0x0006:"RoutingActivationResponse",
       0x0007:"AliveCheckRequest",0x0008:"AliveCheckResponse",
       0x4001:"EntityStatusRequest",0x4002:"EntityStatusResponse",
       0x8001:"DiagnosticMessage",0x8002:"DiagMessageAck",0x8003:"DiagMessageNack"}

types=collections.Counter(); vers=collections.Counter(); pairs=collections.Counter()
bad=0; total=0
for key,byseq in segs.items():
    buf=b"".join(byseq[s] for s in sorted(byseq))
    i=0
    while i+8<=len(buf):
        ver,inv=buf[i],buf[i+1]
        pt=struct.unpack_from(">H",buf,i+2)[0]; pl=struct.unpack_from(">I",buf,i+4)[0]
        if ver!=(~inv&0xFF) or i+8+pl>len(buf): bad+=1; break   # stop, don't resync
        body=buf[i+8:i+8+pl]; total+=1
        types[pt]+=1; vers[ver]+=1
        if pt in (0x8001,0x8002,0x8003) and len(body)>=4:
            s,t=struct.unpack_from(">HH",body,0); pairs[(s,t)]+=1
        i+=8+pl

print(f"DoIP-port streams: {len(segs)}   well-formed messages: {total}   truncated tails: {bad}")
print(f"protocol versions: {dict(vers)}   (0x02 = ISO 13400-2:2012, 0x03 = :2019)")
print("\npayload types:")
for t,c in types.most_common(): print(f"  0x{t:04X} {NAMES.get(t,'?'):<28} {c}")
print("\nsource -> target pairs on diagnostic messages:")
for (s,t),c in pairs.most_common(): print(f"  0x{s:04X} -> 0x{t:04X}   {c}")
ecus=sorted({t for (s,t) in pairs} | {s for (s,t) in pairs})
print("\nall logical addresses:", [f"0x{a:04X}" for a in ecus])
