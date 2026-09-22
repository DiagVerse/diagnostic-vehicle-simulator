"""Drive the simulator's DoIP entity as a real tester would, repeatedly.

Usage:  python3 scripts/doip-loop-test.py [cycles]

Needs an engine on 127.0.0.1:8099 and a DoIP capture to rebuild a vehicle from. It speaks
ISO 13400-2 on the socket rather than going through the HTTP API, so what it exercises is the
entity a real tester meets: routing activation, diagnostic messages, their acknowledgements,
and the refusals that should come back when something is wrong.

The negative cases matter more than the positive ones. Anything that answers correctly is
easy to get right by accident; what separates a conformant entity from a plausible one is
whether it refuses properly, and says why.
"""
import json, socket, struct, sys, time, urllib.request

B = "http://127.0.0.1:8099"
TESTER = 0x0E80
ECUS = [0x006D, 0x00D4, 0x1004, 0x1026, 0x1061, 0x10A7]
RESULTS = []

def call(p, d=None, m=None):
    if d is None and m is None:
        return json.loads(urllib.request.urlopen(B + p, timeout=20).read())
    r = urllib.request.Request(B + p, data=json.dumps(d).encode(),
                               headers={"Content-Type": "application/json"}, method=m or "POST")
    return json.loads(urllib.request.urlopen(r, timeout=60).read())

def check(name, got, want):
    ok = want(got) if callable(want) else got == want
    RESULTS.append((name, "PASS" if ok else "FAIL", repr(got)))
    return ok

def hdr(ptype, body, ver=0x02):
    return struct.pack(">BBHI", ver, (~ver) & 0xFF, ptype, len(body)) + body

def recv_msg(sock):
    head = b""
    while len(head) < 8:
        chunk = sock.recv(8 - len(head))
        if not chunk: raise ConnectionError("entity closed the connection")
        head += chunk
    ver, inv, ptype, plen = struct.unpack(">BBHI", head)
    body = b""
    while len(body) < plen:
        chunk = sock.recv(plen - len(body))
        if not chunk: raise ConnectionError("entity closed mid-message")
        body += chunk
    return ver, ptype, body

# ---------------------------------------------------------------- set-up
with open("/Users/sri/Downloads/Reprolog3.pcapng", "rb") as f:
    raw = f.read()
req = urllib.request.Request(B + "/simulation/pcap", data=raw,
                             headers={"Content-Type": "application/octet-stream"})
state = json.loads(urllib.request.urlopen(req, timeout=120).read())
check("pcap loads", state["loaded"], True)
check("6 ECUs reconstructed", len(state["ecus"]), 6)

# A previous run may have left the entity up; stopping first makes the harness re-runnable.
try:
    call("/doip/stop", {})
except Exception:
    pass
time.sleep(0.2)
status = call("/doip/start", {"bind": "127.0.0.1:13400"})
check("DoIP entity starts", status["running"], True)
time.sleep(0.4)

# ---------------------------------------------------------------- loop
CYCLES = int(sys.argv[1]) if len(sys.argv) > 1 else 5
t0 = time.time()
activation_codes, answered, acked = set(), 0, 0

for cycle in range(1, CYCLES + 1):
    sock = socket.create_connection(("127.0.0.1", 13400), timeout=5)
    try:
        # Routing activation: activation type 0x00 (default), ISO 13400-2 clause 7.1.
        sock.sendall(hdr(0x0005, struct.pack(">HBI", TESTER, 0x00, 0)))
        ver, ptype, body = recv_msg(sock)
        if cycle == 1:
            check("routing activation answered with 0x0006", ptype, 0x0006)
            check("activation echoes the tester address", struct.unpack(">H", body[0:2])[0], TESTER)
        activation_codes.add(body[4])

        for ecu in ECUS:
            # TesterPresent to each ECU over the activated socket.
            sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, ecu) + bytes([0x3E, 0x00])))
            seen_ack = seen_resp = False
            deadline = time.time() + 3
            while time.time() < deadline and not (seen_ack and seen_resp):
                ver, ptype, body = recv_msg(sock)
                if ptype == 0x8002: seen_ack = True; acked += 1
                elif ptype == 0x8001:
                    seen_resp = True; answered += 1
                    if cycle == 1 and ecu == ECUS[0]:
                        s, t = struct.unpack(">HH", body[0:4])
                        check("response source is the ECU", s, ecu)
                        check("response target is the tester", t, TESTER)
                        check("TesterPresent answered 7E 00", body[4:6].hex().upper(), "7E00")
                elif ptype == 0x8003:
                    RESULTS.append((f"cycle {cycle} ECU 0x{ecu:04X} NACKed", "FAIL", body.hex()))
                    break
    finally:
        sock.close()

elapsed = time.time() - t0
check(f"{CYCLES} cycles x 6 ECUs answered", answered, CYCLES * len(ECUS))
check("every diagnostic message acknowledged", acked, CYCLES * len(ECUS))
check("activation code was always 0x10 (success)", activation_codes, {0x10})

# ---------------------------------------------------------------- negatives
# REQ 3.DoIP-131 NL: a diagnostic message before routing activation is not answered AND not
# negatively acknowledged. Silence is the conformant behaviour here — what the tester learns
# from is the socket closing underneath it when the 2 s initial inactivity timer elapses. The
# timer is a measure against exactly this, so the traffic must not postpone it either.
sock = socket.create_connection(("127.0.0.1", 13400), timeout=6)
try:
    sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, ECUS[0]) + bytes([0x3E, 0x00])))
    t_sent = time.time()
    try:
        ver, ptype, body = recv_msg(sock)
        RESULTS.append(("diagnostic before activation gets no answer", "FAIL",
                        f"payload type 0x{ptype:04X}"))
    except ConnectionError:
        waited = time.time() - t_sent
        check("diagnostic before activation gets no answer, then the socket closes", True, True)
        check("and it closes on the initial inactivity timer, not immediately",
              round(waited, 2), lambda w: 1.0 < w < 5.0)
    except TimeoutError:
        RESULTS.append(("diagnostic before activation gets no answer, then the socket closes",
                        "FAIL", "the socket was still open after 6 s"))
finally:
    sock.close()

sock = socket.create_connection(("127.0.0.1", 13400), timeout=3)
try:
    sock.sendall(hdr(0x0005, struct.pack(">HBI", TESTER, 0x00, 0)))
    recv_msg(sock)
    sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, 0x7777) + bytes([0x3E, 0x00])))
    ver, ptype, body = recv_msg(sock)
    check("unknown target address is NACKed", ptype, 0x8003)
    if ptype == 0x8003:
        check("NACK code 0x03 unknown target address", body[4], 0x03)
except (TimeoutError, ConnectionError) as e:
    RESULTS.append(("unknown target address is NACKed", "FAIL",
                    f"no answer at all ({type(e).__name__})"))
finally:
    sock.close()

sock = socket.create_connection(("127.0.0.1", 13400), timeout=3)
try:
    sock.sendall(hdr(0x0005, struct.pack(">HBI", TESTER, 0x00, 0), ver=0x07))
    ver, ptype, body = recv_msg(sock)
    check("bad protocol version gets a generic header NACK", ptype, 0x0000)
except (TimeoutError, ConnectionError) as e:
    RESULTS.append(("bad protocol version gets a generic header NACK", "FAIL",
                    f"no answer at all ({type(e).__name__})"))
finally:
    sock.close()

# A functional group address (ISO 13400-2 Table 13, 0xE000-0xEFFF) reaches the whole vehicle
# rather than being refused as an unknown target.
sock = socket.create_connection(("127.0.0.1", 13400), timeout=5)
try:
    sock.sendall(hdr(0x0005, struct.pack(">HBI", TESTER, 0x00, 0)))
    recv_msg(sock)
    sock.sendall(hdr(0x8001, struct.pack(">HH", TESTER, 0xE000) + bytes([0x3E, 0x00])))

    sources, nacked = set(), False
    deadline = time.time() + 4
    while time.time() < deadline:
        try:
            ver, ptype, body = recv_msg(sock)
        except TimeoutError:
            break
        if ptype == 0x8003:
            nacked = True
            break
        if ptype == 0x8001:
            sources.add(struct.unpack(">H", body[0:2])[0])
            if len(sources) == len(ECUS):
                break
    check("a functional group address is not refused", nacked, False)
    check("every ECU answers the broadcast", sorted(sources), sorted(ECUS))
except (TimeoutError, ConnectionError) as e:
    RESULTS.append(("a functional group address reaches the vehicle", "FAIL",
                    f"{type(e).__name__}"))
finally:
    sock.close()

# REQ 8.DoIP-051 APP: the vehicle identification response is delayed by A_DoIP_Announce_Wait so
# that many entities answering one broadcast do not reply in the same instant.
udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
udp.settimeout(3)
try:
    t_sent = time.time()
    udp.sendto(hdr(0x0001, b""), ("127.0.0.1", 13400))
    data, _ = udp.recvfrom(1024)
    waited_ms = (time.time() - t_sent) * 1000.0
    ptype = struct.unpack(">H", data[2:4])[0]
    check("a vehicle identification request is answered", ptype, 0x0004)
    check("and the answer is inside A_DoIP_Announce_Wait",
          round(waited_ms, 1), lambda w: w < 700.0)
except (TimeoutError, OSError) as e:
    RESULTS.append(("a vehicle identification request is answered", "FAIL",
                    f"{type(e).__name__}"))
finally:
    udp.close()

call("/doip/stop", {})
print(json.dumps({"results": RESULTS, "elapsed": elapsed, "cycles": CYCLES}))
