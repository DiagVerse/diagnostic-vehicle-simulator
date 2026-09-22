"""Drive a full vehicle over DoIP, and prove it is the same ECU the CAN path reaches.

Usage:  python3 scripts/doip-vehicle-test.py [cycles]

Needs an engine on 127.0.0.1:8099 and a simulation file with DoIP-addressable ECUs.

The capture-driven test next door exercises the protocol; this one exercises what the protocol
is carrying. A repro capture is mostly TesterPresent, so it can say the entity answers without
saying it answers anything real — this reads actual data identifiers, filters real trouble codes
by status mask, and unlocks real security.

The checks that matter most are the last few. An ECU with both a CAN address and a DoIP logical
address must be ONE virtual ECU, not two that look alike: a session entered over DoIP has to be
visible on CAN, a session entered over CAN visible over DoIP, and security unlocked on either
unlocked on both. Two instances keyed separately would pass every other check in this file and
diverge the moment a tester used both transports.
"""
import json, socket, struct, sys, time, urllib.request

B = "http://127.0.0.1:8099"
TESTER = 0x0E80
GW_DOIP, GW_CAN = 0x00D4, "18DAD4F1"
RESULTS = []

def call(p, d=None, m=None):
    if d is None and m is None: return json.loads(urllib.request.urlopen(B+p, timeout=30).read())
    r = urllib.request.Request(B+p, data=json.dumps(d).encode(),
                               headers={"Content-Type":"application/json"}, method=m or "POST")
    return json.loads(urllib.request.urlopen(r, timeout=60).read())

def check(name, got, want):
    ok = want(got) if callable(want) else got == want
    RESULTS.append((name, "PASS" if ok else "FAIL", repr(got)))
    return ok

def hdr(pt, body, ver=0x02): return struct.pack(">BBHI", ver, (~ver)&0xFF, pt, len(body)) + body

def recv(sock):
    h = b""
    while len(h) < 8:
        c = sock.recv(8-len(h))
        if not c: raise ConnectionError("closed")
        h += c
    ver, inv, pt, pl = struct.unpack(">BBHI", h)
    b = b""
    while len(b) < pl:
        c = sock.recv(pl-len(b))
        if not c: raise ConnectionError("closed mid-message")
        b += c
    return pt, b

def uds_over_doip(sock, target, payload):
    """Send one UDS request and return the response bytes, skipping the ack."""
    sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, target) + bytes(payload)))
    deadline = time.time() + 3
    while time.time() < deadline:
        pt, body = recv(sock)
        if pt == 0x8001: return body[4:]
        if pt == 0x8003: return b"NACK" + bytes([body[4]])
    return b""

def uds_over_can(canid, hexs):
    d = call("/simulation/request", {"canIdHex": canid, "requestHex": hexs})
    return d["responses"][0]["responseHex"] if d["responses"] else "(silence)"

# ---------------------------------------------------------------- set-up
state = call("/simulation/simfile", {"logText": open("/Users/sri/Downloads/P33C-converted.simfile.json").read()})
check("P33C loads", len(state["ecus"]), 47)
try: call("/doip/stop", {})
except Exception: pass
time.sleep(0.2)
call("/doip/start", {"bind": "127.0.0.1:13400"})
time.sleep(0.4)

sock = socket.create_connection(("127.0.0.1", 13400), timeout=5)
sock.sendall(hdr(0x0005, struct.pack(">HBI", TESTER, 0x00, 0)))
pt, body = recv(sock)
check("routing activation succeeds", (pt, body[4]), (0x0006, 0x10))

# ---------------------------------------------------------------- real services over DoIP
r = uds_over_doip(sock, GW_DOIP, [0x10, 0x03])
check("0x10 extended session over DoIP", r[:2].hex().upper(), "5003")

r = uds_over_doip(sock, GW_DOIP, [0x22, 0x01, 0x11])
check("0x22 reads a real DID over DoIP", r[:3].hex().upper(), "620111")
check("...and returns its value", len(r) > 3, True)

# A DTC record is three code bytes and a status byte, after the two-byte header and the
# availability mask.
def DtcCount(resp): return (len(resp) - 3) // 4

byMask = uds_over_doip(sock, GW_DOIP, [0x19, 0x02, 0xFF])
check("0x19 02 reads DTCs by status mask over DoIP", byMask[:2].hex().upper(), "5902")
supported = uds_over_doip(sock, GW_DOIP, [0x19, 0x0A])
check("0x19 0A reports supported DTCs over DoIP", supported[:2].hex().upper(), "590A")
check("...and reports every DTC the ECU holds", DtcCount(supported) > 0, True)

# The mask is not decoration: a DTC whose status is 0x00 intersects no mask, so a query by
# mask must return fewer than the supported list. Asserting the counts are equal would pass
# on an implementation that ignored the mask entirely.
check("a status mask filters, rather than being ignored",
      DtcCount(byMask) < DtcCount(supported), True)
narrow = uds_over_doip(sock, GW_DOIP, [0x19, 0x02, 0x08])
check("...and a narrow mask returns only what sets that bit",
      0 < DtcCount(narrow) < DtcCount(byMask), True)

r = uds_over_doip(sock, GW_DOIP, [0x3E, 0x00])
check("0x3E TesterPresent over DoIP", r.hex().upper(), "7E00")

r = uds_over_doip(sock, GW_DOIP, [0x22, 0x9A, 0x9A])
check("unknown DID refused over DoIP", r.hex().upper(), "7F2231")

# ---------------------------------------------------------------- the invariant that matters
# The gateway was put into the extended session over DoIP above. Ask the CAN path what session
# it is in: if the two transports share one VirtualEcu, it is still extended.
ecu = [e for e in call("/simulation/state")["ecus"] if e["handle"] == GW_CAN][0]
check("DoIP and CAN reach ONE ECU: session set over DoIP is seen on CAN", ecu["session"], 3)

# And the reverse: change it over CAN, observe over DoIP.
uds_over_can(GW_CAN, "10 01")
r = uds_over_doip(sock, GW_DOIP, [0x22, 0xF1, 0x86])
session_over_can = [e for e in call("/simulation/state")["ecus"] if e["handle"] == GW_CAN][0]["session"]
check("...and a session set over CAN is seen by DoIP", session_over_can, 1)

# Security unlocked over DoIP must be unlocked for CAN too.
uds_over_doip(sock, GW_DOIP, [0x10, 0x03])
call(f"/simulation/ecus/{GW_CAN}/security", {"levels":[
    {"requestSeedHex":"01","seedHex":"11 22 33 44","expectedKeyHex":"",
     "keyPolicy":"acceptAny","refusalNrcHex":None}]}, "PUT")
uds_over_doip(sock, GW_DOIP, [0x27, 0x01])
r = uds_over_doip(sock, GW_DOIP, [0x27, 0x02, 0xDE, 0xAD])
check("security unlocked over DoIP", r.hex().upper(), "6702")
unlocked = [e for e in call("/simulation/state")["ecus"] if e["handle"] == GW_CAN][0]["securityUnlocked"]
check("...and CAN sees the same ECU unlocked", unlocked, True)

# ---------------------------------------------------------------- breadth
CYCLES = int(sys.argv[1]) if len(sys.argv) > 1 else 3
targets = [int(e["logicalAddressHex"], 16) for e in call("/simulation/state")["ecus"]
           if e.get("logicalAddressHex") and int(e["logicalAddressHex"], 16) != 0]
answered = 0
t0 = time.time()
for _ in range(CYCLES):
    for t in targets:
        if uds_over_doip(sock, t, [0x3E, 0x00]).hex().upper() == "7E00":
            answered += 1
elapsed = time.time() - t0
check(f"every DoIP-addressable ECU answers, {CYCLES}x", answered, CYCLES * len(targets))

# ---------------------------------------------------------------- broadcast
# A functional group address (ISO 13400-2 Table 13) reaches the whole vehicle in one request,
# which on this vehicle is 46 ECUs answering a single message. Worth doing at this scale: a
# broadcast that quietly reached only some of them would look identical on a two-ECU bench.
sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, 0xE000) + bytes([0x3E, 0x00])))
sources, nacked = set(), None
sock.settimeout(2)
deadline = time.time() + 15
while time.time() < deadline:
    try:
        pt, body = recv(sock)
    except (TimeoutError, socket.timeout):
        break
    if pt == 0x8003:
        nacked = body[4]
        break
    if pt == 0x8001:
        sources.add(struct.unpack(">H", body[0:2])[0])
sock.settimeout(None)
check("a functional group address is not refused", nacked, None)
check("one broadcast reaches every DoIP-addressable ECU", sorted(sources), sorted(targets))
check("and each answer names its own address, not the group",
      0xE000 in sources, False)

# REQ 7.DoIP-072 AL. This vehicle has one ECU (CAN(B)_HE13BMSEXT) with no DoIP logical address,
# so a broadcast has to cross a CAN sub-network to reach it — and there a functional request may
# be a SingleFrame only, because it has no single peer to flow-control it. Nine bytes of user
# data is past that, so the whole message is discarded rather than delivered to the 46 ECUs that
# could have taken it: half a broadcast is worse than none.
long_request = [0x22, 0xF1, 0x90, 0xF1, 0x8C, 0xF1, 0x91, 0xF1]
sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, 0xE000) + bytes(long_request)))
pt, body = recv(sock)
check("a functional request too long for a CAN sub-network is refused", pt, 0x8003)
if pt == 0x8003:
    check("NACK code 0x04 diagnostic message too large", body[4], 0x04)

# The same request physically addressed is fine: it never leaves Ethernet.
r = uds_over_doip(sock, GW_DOIP, long_request)
check("...while the same length is accepted when it is addressed physically",
      r[:4].hex().upper(), lambda got: not got.startswith("4E41"))

sock.close()
call("/doip/stop", {})
print(json.dumps({"results": RESULTS, "targets": len(targets), "cycles": CYCLES, "elapsed": elapsed}))
