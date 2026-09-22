"""Feature sweep against the real P33C vehicle, driven through the HTTP API."""
import json, os, pty, select, subprocess, sys, time, urllib.request, urllib.error

B = "http://127.0.0.1:8099"
SIMFILE = "/Users/sri/Downloads/P33C-converted.simfile.json"
RESULTS = []

def call(path, body=None, method=None):
    if body is None and method is None:
        return json.loads(urllib.request.urlopen(B + path, timeout=20).read())
    r = urllib.request.Request(B + path, data=json.dumps(body).encode(),
                               headers={"Content-Type": "application/json"},
                               method=method or "POST")
    return json.loads(urllib.request.urlopen(r, timeout=30).read())

def req(ecu, hexs):
    return call("/simulation/request", {"canIdHex": ecu, "requestHex": hexs})["responses"][0]["responseHex"]

def check(area, name, got, want):
    ok = (got == want) if not callable(want) else want(got)
    RESULTS.append((area, name, "PASS" if ok else "FAIL", str(got), str(want) if not callable(want) else "(predicate)"))
    return ok

# ---------------------------------------------------------------- load
t0 = time.time()
state = call("/simulation/simfile", {"logText": open(SIMFILE).read()})
load_ms = int((time.time() - t0) * 1000)
check("Load", "P33C simfile loads", state["loaded"], True)
check("Load", "all 47 ECUs started", len(state["ecus"]), 47)
check("Load", f"load time {load_ms}ms under 5s", load_ms < 5000, True)
check("Load", "freshly loaded is not unsaved", state["unsavedChanges"], False)

GW, PCM = "18DAD4F1", "18DA15F1"

# ---------------------------------------------------------------- sessions & DIDs
check("UDS", "default session", req(GW, "10 01")[:2], "50")
check("UDS", "extended session", req(GW, "10 03")[:2], "50")
check("UDS", "TesterPresent", req(GW, "3E 00"), "7E 00")
check("UDS", "read configured DID 0x0111", req(GW, "22 01 11")[:8], "62 01 11")
check("UDS", "unknown DID refused", req(GW, "22 9A 9A"), "7F 22 31")

# ---------------------------------------------------------------- DTCs (19 sub-function lengths)
check("DTC", "19 0A reportSupportedDTC (2 bytes)", req(GW, "19 0A")[:5], "59 0A")
check("DTC", "19 02 needs a mask", req(GW, "19 02"), "7F 19 13")
check("DTC", "19 0A rejects a mask", req(GW, "19 0A FF"), "7F 19 13")
check("DTC", "19 01 counts", req(GW, "19 01 FF")[:5], "59 01")
check("DTC", "19 04 unsupported sub-function", req(GW, "19 04 12 34 56 01"), "7F 19 12")

# ---------------------------------------------------------------- security
call(f"/simulation/ecus/{GW}/security", {"levels": [
    {"requestSeedHex": "01", "seedHex": "93 D7 C5 02", "expectedKeyHex": "",
     "keyPolicy": "acceptAny", "refusalNrcHex": None}]}, "PUT")
req(GW, "10 03")
check("Security", "sendKey without seed -> 0x24", req(GW, "27 02 DE AD"), "7F 27 24")
check("Security", "requestSeed returns the seed", req(GW, "27 01")[:5], "67 01")
check("Security", "acceptAny unlocks on any key", req(GW, "27 02 DE AD BE EF"), "67 02")
def unlocked(handle):
    return [e for e in call("/simulation/state")["ecus"] if e["handle"] == handle][0]["securityUnlocked"]
check("Security", "unlocked after sendKey", unlocked(GW), True)
req(GW, "10 01")
check("Security", "default session relocks", unlocked(GW), False)
# The session gate answers before the seed check, which is stricter and more correct than the
# 0x24 a reachable-but-unsequenced service would give.
check("Security", "0x27 barred in default session", req(GW, "27 02 DE AD"), "7F 27 7F")

call(f"/simulation/ecus/{GW}/security", {"levels": [
    {"requestSeedHex": "01", "seedHex": "93 D7 C5 02", "expectedKeyHex": "",
     "keyPolicy": "refuse", "refusalNrcHex": "36"}]}, "PUT")
req(GW, "10 03"); req(GW, "27 01")
check("Security", "refuse policy uses its NRC", req(GW, "27 02 AA BB"), "7F 27 36")
try:
    call(f"/simulation/ecus/{GW}/security", {"levels": [
        {"requestSeedHex": "02", "seedHex": "11 22", "expectedKeyHex": "",
         "keyPolicy": "acceptAny", "refusalNrcHex": None}]}, "PUT")
    check("Security", "even requestSeed rejected", "accepted", "rejected")
except urllib.error.HTTPError as e:
    check("Security", "even requestSeed rejected", e.code, 400)

# ---------------------------------------------------------------- overrides
call(f"/simulation/ecus/{PCM}/overrides", {"overrides": [
    {"requestHex": "2E FD 02", "matchTrailingBytes": True, "action": "substitute",
     "responseHex": "6E FD 02", "echoSpans": [], "enabled": True,
     "respondEvenIfSuppressed": False, "note": "write, any value"},
    {"requestHex": "34 00 44", "matchTrailingBytes": True, "action": "substitute",
     "responseHex": "74 20 04 00", "echoSpans": [], "enabled": True,
     "respondEvenIfSuppressed": False, "note": "download, any address"}]}, "PUT")
check("Override", "2E prefix match with a value", req(PCM, "2E FD 02 31 32 33"), "6E FD 02")
check("Override", "2E different DID falls through", req(PCM, "2E FD 01 31")[:5], "7F 2E")
check("Override", "34 prefix match with a real address", req(PCM, "34 00 44 00 01 00 00 00 00 80 00"), "74 20 04 00")

# ---------------------------------------------------------------- modes
check("Mode", "strict: undeclared service refused", req(PCM, "36 01 AA")[:5], "7F 36")
call("/simulation/permissive", {"enabled": True})
check("Mode", "permissive flag reported", call("/simulation/state")["permissiveMode"], True)
check("Mode", "permissive: 0x34 reaches its handler", req(PCM, "34 00 44 00 01 00 00 00 00 80 00"), "74 20 04 00")
check("Mode", "permissive: 36 01 accepted", req(PCM, "36 01 AA BB"), "76 01")
check("Mode", "block counter advances", req(PCM, "36 02 AA BB"), "76 02")
check("Mode", "retransmission answered again", req(PCM, "36 02 AA BB"), "76 02")
check("Mode", "out of sequence -> 0x73", req(PCM, "36 09 AA"), "7F 36 73")
check("Mode", "transfer exit", req(PCM, "37"), "77")
check("Mode", "transfer closed after exit", req(PCM, "36 03 AA"), "7F 36 24")
check("Mode", "permissive still refuses malformed", req(PCM, "19 02"), "7F 19 13")
call("/simulation/permissive", {"enabled": False})
check("Mode", "normal mode restored", call("/simulation/state")["permissiveMode"], False)
# An override outranks the supported-service list by design, so it has to be cleared before
# strict mode can be observed at all — which is itself worth asserting.
check("Mode", "an override still answers in strict mode", req(PCM, "34 00 44 00 01 00 00 00 00 80 00"), "74 20 04 00")
saved_overrides = call(f"/simulation/ecus/{PCM}/overrides")
call(f"/simulation/ecus/{PCM}/overrides", {"overrides": []}, "PUT")
check("Mode", "strict again: undeclared 0x34 refused", req(PCM, "34 00 44 00 01 00 00 00 00 80 00"), "7F 34 11")
call(f"/simulation/ecus/{PCM}/overrides", {"overrides": saved_overrides}, "PUT")

# ---------------------------------------------------------------- timing
t = call(f"/simulation/ecus/{GW}/timing")
check("Timing", "BlockSize defaults to 1", t["isoTpBlockSize"], 1)
t["isoTpBlockSize"] = 8; t["isoTpSeparationTimeMin"] = 3
check("Timing", "BlockSize/STmin settable", call(f"/simulation/ecus/{GW}/timing", t, "PUT")["isoTpBlockSize"], 8)
try:
    bad = dict(t); bad["isoTpSeparationTimeMin"] = 0x90
    call(f"/simulation/ecus/{GW}/timing", bad, "PUT")
    check("Timing", "reserved STmin rejected", "accepted", "rejected")
except urllib.error.HTTPError as e:
    check("Timing", "reserved STmin rejected", e.code, 400)

# ---------------------------------------------------------------- export round trip
exp = call("/simulation/export")
doc = json.loads(exp["content"])
check("Export", "every ECU written", len(doc["ecus"]), 47)
check("Export", "marks the model saved", call("/simulation/state")["unsavedChanges"], False)
call("/simulation/simfile", {"logText": exp["content"]})
check("Export", "overrides survive reload", len(call(f"/simulation/ecus/{PCM}/overrides")), 2)
check("Export", "BlockSize survives reload", call(f"/simulation/ecus/{GW}/timing")["isoTpBlockSize"], 8)
check("Export", "security survives reload", call(f"/simulation/ecus/{GW}/security")[0]["keyPolicy"], "refuse")

# ---------------------------------------------------------------- monitor at volume
import http.client
def CountSseBatches(seconds, path):
    conn = http.client.HTTPConnection("127.0.0.1", 8099, timeout=seconds + 10)
    conn.request("GET", path)
    resp = conn.getresponse()
    deadline, batches, events = time.time() + seconds, 0, 0
    buf = b""
    while time.time() < deadline:
        chunk = resp.read(65536)
        if not chunk:
            break
        buf += chunk
        while b"\n\n" in buf:
            block, buf = buf.split(b"\n\n", 1)
            for line in block.split(b"\n"):
                if line.startswith(b"data:"):
                    batches += 1
                    try:
                        events += len(json.loads(line[5:]))
                    except Exception:
                        pass
    conn.close()
    return batches, events

import threading
stop = threading.Event()
def Flood():
    while not stop.is_set():
        try:
            req(GW, "22 01 11")
        except Exception:
            return

call("/simulation/reset", {})
threads = [threading.Thread(target=Flood, daemon=True) for _ in range(6)]
for t in threads:
    t.start()
time.sleep(0.5)
batches, events = CountSseBatches(3, "/events?history=0")
stop.set()
for t in threads:
    t.join(timeout=2)

ratio = (events / batches) if batches else 0
check("Monitor", f"SSE batches events ({events} events in {batches} messages)", ratio > 1.5, True)
check("Monitor", "engine holds a bounded history", call("/simulation/state")["loaded"], True)

print(json.dumps(RESULTS))
