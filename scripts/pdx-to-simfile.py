"""Turn a PDX (ODX) archive into a simulation file this engine can load.

Usage:
    python3 scripts/pdx-to-simfile.py <input.pdx | directory | archive.zip> -o vehicle.simfile.json
    python3 scripts/pdx-to-simfile.py "PZ1A PDX.zip" -o pz1a.simfile.json --vehicle PZ1A

Needs `odxtools` (pip install odxtools).

WHY THIS IS A SCRIPT AND NOT A RUST CRATE
-----------------------------------------
The README's one governing rule is: do not build a separate simulator per input format, build
one Unified Vehicle Model and provide multiple ways to populate it. A populator's job is to
produce that model; the simulation engine consumes only the model.

The simulation file *is* that model in document form, so a PDX-to-simfile converter is a
populator in exactly the sense the architecture means, and the engine gains a fifth source
without gaining a line of ODX code or a Python dependency.

That matters here more than usual. ODX is not one document: a per-ECU PDX inherits most of its
services from shared layers through PARENT-REFs and IMPORT-REFs across several files — in the
sample this was written against, an ECU declares 7 services of its own and resolves to 38.
Re-implementing that inheritance in Rust would produce a worse parser than `odxtools`, which
already does it correctly, and would put thousands of lines of XML handling inside a binary
whose job is to answer diagnostic requests.

WHAT ODX CAN AND CANNOT TELL US
-------------------------------
ODX describes what an ECU *can* answer and the shape of each answer. It does not record what
any particular vehicle's answers *are* — a specification is not a capture.

So every data identifier here gets a placeholder value of the declared length, and the file
says so in `vehicle`. That is the one place this converter invents anything, it is invented
visibly, and it is the operator's to edit. Everything else — addresses, trouble codes, services,
sessions, security levels — is read out of the ODX and is as true as the file it came from.
"""

import argparse
import glob
import json
import os
import re
import shutil
import sys
import tempfile
import zipfile

# Imported softly so the readers below can be exercised without it. Only `main` needs a real
# ODX parser; the logic that decides what an address, a name or a placeholder should be is plain
# Python, and it is where the mistakes live — so it is worth being able to test on its own.
try:
    import odxtools
    import odxtools.exceptions
except ImportError:  # pragma: no cover - exercised by running without the dependency
    odxtools = None

c_strMissingOdxtools = (
    "odxtools is not installed. It is what reads ODX, including the inheritance a PDX\n"
    "spreads across several documents:\n\n    pip3 install odxtools\n"
)

# ---------------------------------------------------------------------------------------
# ODX communication parameters, by the names ISO 22901 gives them.
# ---------------------------------------------------------------------------------------

# The table that carries an ECU's addressing. Its rows are (prefix, format, identifier), and
# the format string is what says whether a row is a transmit or a receive identifier and how
# wide it is — there is no separate field for either.
c_strUniqueRespIdTable = "CP_UniqueRespIdTable"
c_strCanFuncReqId = "CP_CanFuncReqId"
c_strDoIpFunctionalAddress = "CP_DoIPLogicalFunctionalAddress"

# A row of the table is a transmit identifier (tester to ECU) or a receive one (ECU to tester).
c_strTransmit = "transmit"
c_strReceive = "receive"

# UDS service identifiers this converter recognises well enough to say something about.
c_bySidSessionControl = 0x10
c_bySidSecurityAccess = 0x27
c_bySidReadDataByIdentifier = 0x22

# Sub-function values for DiagnosticSessionControl, by the names the simulation file uses.
c_mapSessionNames = {0x01: "default", 0x02: "programming", 0x03: "extended", 0x04: "safety"}

# An 11-bit identifier is at most this. Anything larger came off a 29-bit link.
c_u32MaxStandardCanId = 0x7FF


def ParseComparamValue(value):
    """Flatten a comparam value to a list of strings.

    Simple parameters arrive as a scalar and complex ones as a nested list, and the table that
    carries addressing is the complex kind. Flattening here keeps every reader below working on
    one shape instead of each one re-discovering which it was handed.
    """
    if value is None:
        return []
    if isinstance(value, (list, tuple)):
        vecOut = []
        for item in value:
            vecOut.extend(ParseComparamValue(item))
        return vecOut
    return [str(value)]


def CollectComparams(ecu):
    """Every comparam this ECU resolves, as {name: [flattened values]}.

    An ECU is described once per protocol it speaks, so the same parameter appears several
    times with different values — CAN 29-bit, CAN 11-bit and DoIP all define their own
    addressing. They are gathered rather than overwritten so the caller can choose between
    them knowingly instead of keeping whichever happened to be read last.
    """
    mapValues = {}
    for ref in getattr(ecu, "comparam_refs", []) or []:
        strName = getattr(ref, "short_name", None)
        if strName is None:
            continue
        mapValues.setdefault(strName, []).append(ParseComparamValue(getattr(ref, "value", None)))
    return mapValues


def ReadAddressingTable(vecTables):
    """Pull request, response and width out of the UniqueRespIdTable rows.

    The table is a flat list in groups of three: a prefix, a format description, and an
    identifier. The format string is the only thing that says what the identifier *is*, which is
    why this reads it rather than trusting position — "normal segmented 29-bit transmit with FC"
    is a request identifier, the matching "receive" is the response.

    A row whose identifier is 0xFFFFFFFF is the standard's "not used" marker and is skipped;
    reading it as an address would invent a CAN id of four billion.
    """
    vecFound = []
    for vecTable in vecTables:
        u32Request = None
        u32Response = None
        for uIndex in range(0, len(vecTable) - 2, 3):
            strFormat = vecTable[uIndex + 1].lower()
            strId = vecTable[uIndex + 2]
            if not strId.isdigit():
                continue
            u32Id = int(strId)
            if u32Id == 0xFFFFFFFF:
                continue
            if c_strTransmit in strFormat and u32Request is None:
                u32Request = u32Id
            elif c_strReceive in strFormat and u32Response is None:
                u32Response = u32Id
        if u32Request is not None and u32Response is not None:
            vecFound.append((u32Request, u32Response))
    return vecFound


def ReadLogicalAddress(vecTables):
    """The DoIP logical address, which the same table carries as a lone value.

    A DoIP row has no transmit/receive format because there are no CAN identifiers to describe
    — just the address. So it is recognised by being a short table whose first entry is a
    number, which is exactly the shape the CAN rows are not.
    """
    for vecTable in vecTables:
        if len(vecTable) > 3:
            continue
        if not vecTable or not vecTable[0].isdigit():
            continue
        u16Address = int(vecTable[0])
        if 0 < u16Address <= 0xFFFF:
            return u16Address
    return None


def ReadServiceSid(service):
    """The UDS service identifier a service sends, or None if it does not state one."""
    for param in service.request.parameters:
        if type(param).__name__ == "CodedConstParameter" and param.short_name == "SID":
            return param.coded_value
    return None


def ReadConstantParameter(service, strName):
    """A constant parameter's value from a service's request, by parameter name."""
    for param in service.request.parameters:
        if type(param).__name__ == "CodedConstParameter" and param.short_name == strName:
            return param.coded_value
    return None


def ReadSubFunctions(service):
    """Every sub-function value a service accepts.

    ODX states these two different ways and both appear in one file. A service pinned to a
    single sub-function carries a `CodedConstParameter`; one that accepts several carries a
    `TableKeyParameter` pointing at a TABLE whose rows are the permitted values — which is how
    this sample describes both DiagnosticSessionControl and SecurityAccess.

    Reading only the constant form, as this first did, produced a vehicle where every ECU knew
    exactly one session and no security levels at all: technically parsed, and wrong about the
    one thing the file was most explicit about.
    """
    setValues = set()
    for param in service.request.parameters:
        strType = type(param).__name__
        if strType == "CodedConstParameter":
            if param.short_name == "SID":
                continue
            value = param.coded_value
            if isinstance(value, int) and 0 <= value <= 0xFF:
                setValues.add(value)

        elif strType == "TableKeyParameter":
            table = getattr(param, "table", None)
            for row in getattr(table, "table_rows", []) or []:
                key = getattr(row, "key", None)
                # A row's key is the raw bytes that go on the wire; a sub-function is one byte.
                if isinstance(key, (bytes, bytearray)) and len(key) == 1:
                    setValues.add(key[0])
                elif isinstance(key, int) and 0 <= key <= 0xFF:
                    setValues.add(key)
    return setValues


def ReadDataIdentifier(service):
    """The DID a ReadDataByIdentifier service reads."""
    value = ReadConstantParameter(service, "DID")
    if isinstance(value, int) and 0 <= value <= 0xFFFF:
        return value
    return None


def ReadDopLengths(dop, uDepth=0):
    """The (minimum, maximum) byte length a data object property encodes to.

    Either bound may be `None` where ODX leaves it open. A value is wrapped in a STRUCTURE whose
    parameters hold the real types, so this recurses rather than reading the outer object — the
    outer one reports no length at all, which is what first made every placeholder fall back to
    a default.

    Two shapes matter. A fixed-width type states a bit length. A `MIN-MAX-LENGTH-TYPE` states a
    range instead, and that is not a gap in the file: it is the file saying the answer really is
    variable, which several identity identifiers genuinely are.
    """
    if dop is None or uDepth > 4:
        return (None, None)

    codedType = getattr(dop, "diag_coded_type", None)
    if codedType is not None:
        uBitLength = getattr(codedType, "bit_length", None)
        if uBitLength:
            uBytes = max(1, uBitLength // 8)
            return (uBytes, uBytes)
        uMin = getattr(codedType, "min_length", None)
        uMax = getattr(codedType, "max_length", None)
        if uMin is not None or uMax is not None:
            return (uMin, uMax)

    uTotalMin = 0
    uTotalMax = 0
    bHasAny = False
    for param in getattr(dop, "parameters", []) or []:
        uMin, uMax = ReadDopLengths(getattr(param, "dop", None), uDepth + 1)
        if uMin is None and uMax is None:
            continue
        bHasAny = True
        uTotalMin += uMin or 0
        uTotalMax += uMax or uMin or 0
    if bHasAny:
        return (uTotalMin or None, uTotalMax or None)
    return (None, None)


def ReadResponseLengths(service):
    """The (minimum, maximum) payload bytes a positive response carries after the identifier."""
    for response in service.positive_responses:
        for param in response.parameters:
            if type(param).__name__ in ("CodedConstParameter", "MatchingRequestParameter"):
                continue
            uMin, uMax = ReadDopLengths(getattr(param, "dop", None))
            if uMin is not None or uMax is not None:
                return (uMin, uMax)
    return (None, None)


# What to answer with when ODX pins no length at all. Long enough to be recognisable, short
# enough not to need segmenting on a link that may be slow.
c_uDefaultPlaceholderBytes = 16

# Below this, a text label is truncated past the point of being recognisable as one.
c_uMinLabelBytes = 6


def BuildPlaceholderValue(uMinBytes, uMaxBytes, strEcuName, u16Did):
    """A stand-in for a value ODX does not record.

    Deliberately not a plausible-looking VIN or part number. A specification says an ECU answers
    0xF190 with a string; it does not say which string, and filling in something that reads like
    real data would be a claim the source never made — the exact thing this project's
    reconstruction rules forbid. So the placeholder names itself, and the operator can see at a
    glance which values are real and which are waiting to be filled in.

    The length obeys whatever ODX *did* say. A fixed-width identifier gets exactly its width, so
    a tester's length check still passes. A `MIN-MAX-LENGTH-TYPE` gets the marker's own length
    clamped into the permitted range, because every length in that range is legal and the
    readable one is the most useful of them.

    Returns `(text, hex)` with exactly one of the two set, matching the two forms the simulation
    file accepts for a value.
    """
    strMarker = f"ODX-{strEcuName}-{u16Did:04X}"

    if uMinBytes is not None and uMinBytes == uMaxBytes:
        uLength = uMinBytes
    else:
        uLength = len(strMarker)
        if uMinBytes is not None:
            uLength = max(uLength, uMinBytes)
        if uMaxBytes is not None:
            uLength = min(uLength, uMaxBytes)
        if uMinBytes is None and uMaxBytes is None:
            uLength = c_uDefaultPlaceholderBytes

    uLength = max(1, uLength)

    # Too short to carry a label. A one-byte diagnosticVersion truncated to "O" says nothing
    # and reads like data; neutral hex at least reads like a placeholder.
    if uLength < c_uMinLabelBytes:
        return None, " ".join("00" for _ in range(uLength))

    return (strMarker * (uLength // len(strMarker) + 1))[:uLength], None


def FormatTroubleCode(u32TroubleCode):
    """An ODX trouble code as the simulation file writes one.

    ODX stores the three bytes a tester sees: two of code and one of failure type. The file
    format accepts exactly that as `0xRRRRRR`, so it is passed through rather than decoded into
    a `P0123-11` spelling that would have to be undone again on load.
    """
    return f"0x{u32TroubleCode:06X}"


def BuildEcu(ecu, strDisplayName, u16FallbackLogicalAddress):
    """One ECU of the simulation file, from one ODX ECU variant."""
    mapComparams = CollectComparams(ecu)
    vecTables = mapComparams.get(c_strUniqueRespIdTable, [])

    dtoEcu = {"name": strDisplayName}

    # Addressing. An ECU described on both a 29-bit and an 11-bit link yields two candidate
    # pairs; the widest is taken, because that is the link a modern vehicle's tester actually
    # uses and the one the sample's own traffic logs are recorded on.
    vecPairs = ReadAddressingTable(vecTables)
    if vecPairs:
        u32Request, u32Response = max(vecPairs, key=lambda pair: pair[0])
        dtoCan = {"request": f"0x{u32Request:X}", "response": f"0x{u32Response:X}"}

        vecFunctional = mapComparams.get(c_strCanFuncReqId, [])
        for vecValue in vecFunctional:
            if vecValue and vecValue[0].isdigit():
                u32Functional = int(vecValue[0])
                # A functional identifier only belongs on the link it was declared for. Putting
                # a 29-bit broadcast on an 11-bit ECU would have it answer a request no tester
                # could address to it.
                bSameWidth = (u32Functional > c_u32MaxStandardCanId) == (
                    u32Request > c_u32MaxStandardCanId
                )
                if bSameWidth:
                    dtoCan["functional"] = f"0x{u32Functional:X}"
                break
        dtoEcu["can"] = dtoCan

    u16LogicalAddress = ReadLogicalAddress(vecTables) or u16FallbackLogicalAddress
    if u16LogicalAddress:
        dtoEcu["doip"] = {"logicalAddress": f"0x{u16LogicalAddress:04X}"}

    # Services, sessions, security and data identifiers, read from what the ECU declares.
    setSids = set()
    setSessions = set()
    mapSecurity = {}
    mapDids = {}

    for service in ecu.services:
        bySid = ReadServiceSid(service)
        if bySid is None:
            continue
        setSids.add(bySid)

        if bySid == c_bySidSessionControl:
            for bySubFunction in ReadSubFunctions(service):
                strSession = c_mapSessionNames.get(bySubFunction)
                if strSession is not None:
                    setSessions.add(strSession)

        elif bySid == c_bySidSecurityAccess:
            for bySubFunction in ReadSubFunctions(service):
                # Odd sub-functions request a seed, even ones send the key. Only the seed side
                # defines a level; counting both would double every one of them.
                if bySubFunction % 2 == 1:
                    mapSecurity[bySubFunction] = True

        elif bySid == c_bySidReadDataByIdentifier:
            u16Did = ReadDataIdentifier(service)
            if u16Did is not None:
                mapDids[u16Did] = ReadResponseLengths(service)

    if setSids:
        dtoEcu["services"] = [f"0x{bySid:02X}" for bySid in sorted(setSids)]
    # Every ECU can sit in the default session whether or not it lists a service for it.
    setSessions.add("default")
    dtoEcu["sessions"] = [
        strName for strName in ("default", "programming", "extended", "safety") if strName in setSessions
    ]

    if mapDids:
        mapDidValues = {}
        for u16Did, pair in sorted(mapDids.items()):
            strText, strHex = BuildPlaceholderValue(pair[0], pair[1], strDisplayName, u16Did)
            mapDidValues[f"{u16Did:04X}"] = {"text": strText} if strText is not None else strHex
        dtoEcu["dids"] = mapDidValues

    if mapSecurity:
        # `acceptAny`, for the same reason a capture-derived level gets it: ODX describes that a
        # level exists and how the seed and key are shaped. The algorithm that ties them is the
        # one thing a specification deliberately never carries, so comparing against a key
        # nobody has would refuse every tester.
        dtoEcu["security"] = [
            {
                "requestSeed": f"{bySubFunction:02X}",
                "seed": "11 22 33 44",
                "keyPolicy": "acceptAny",
            }
            for bySubFunction in sorted(mapSecurity)
        ]

    vecDtcs = []
    setSeen = set()
    for dop in ecu.diag_data_dictionary_spec.dtc_dops:
        for dtc in dop.dtcs:
            u32Code = getattr(dtc, "trouble_code", None)
            if u32Code is None or u32Code in setSeen:
                continue
            setSeen.add(u32Code)
            vecDtcs.append({"code": FormatTroubleCode(u32Code)})
    if vecDtcs:
        dtoEcu["dtcs"] = vecDtcs

    return dtoEcu


def ReadNameAndAddress(strPath):
    """A readable ECU name and its logical address, from the PDX file name.

    Worth doing rather than falling back to the ODX short name, which in this sample is a UUID
    — `_0x00E2_E_ACT_EBA_29b_3b9c41be_96fb...`. A vehicle whose ECUs are all named like that is
    technically correct and unusable. The supplier's file name carries both the address and a
    human name, so it is read when it has that shape and ignored when it does not.
    """
    strStem = os.path.splitext(os.path.basename(strPath))[0]
    match = re.search(r"0x([0-9A-Fa-f]{4})[_ ]+(.+)", strStem)
    if match is None:
        return strStem, None

    u16Address = int(match.group(1), 16)
    strRest = match.group(2)

    # Everything from the separator on is the file's own provenance — supplier, protocol,
    # revision date — not the ECU's name. Suppliers spell that separator both ways in the same
    # delivery ("Name - N_PZ1A_..." and "Name_-_N_PZ1A_..."), so both are cut, and before
    # underscores become spaces rather than after, when the two forms are no longer telling
    # apart.
    strName = re.split(r"_-_| - ", strRest)[0]
    strName = strName.strip().strip("_").replace("_", " ").strip()
    return (strName or strStem), u16Address


def LoadPdx(strPath):
    """Read one PDX, strictly if it will and leniently if it must.

    Returns `(database, note)`. `note` is empty on a clean read, describes the degradation when
    lenient parsing was needed, and carries the error when nothing could be read.

    Supplier files are not always valid ODX. In the sample this was written against, one of
    fifty-four references a physical unit its own unit library does not define — and a strict
    reader is right to refuse that. But the broken reference is to a *unit*, which affects how a
    value would be displayed and nothing about the diagnostic behaviour extracted here, so
    losing the whole ECU over it would be the worse mistake. The fallback is reported rather
    than silent, because "read with errors ignored" and "read cleanly" are not the same claim.
    """
    try:
        return odxtools.load_pdx_file(strPath), ""
    except Exception as strictError:
        strFirstLine = str(strictError).strip().splitlines()[0][:140]

    bWasStrict = odxtools.exceptions.strict_mode
    try:
        odxtools.exceptions.strict_mode = False
        return odxtools.load_pdx_file(strPath), strFirstLine
    except Exception as lenientError:
        return None, str(lenientError).strip().splitlines()[0][:140]
    finally:
        odxtools.exceptions.strict_mode = bWasStrict


def KeepOneEcuPerAddress(vecEcus, strPrefer):
    """Reduce ECU *variants* to the one ECU a vehicle actually carries.

    A PDX delivery describes every variant a platform can be built with, not the set fitted to
    one car. This sample ships two files for address 0x0004 — an EPS and an EPS for the AD2
    build — and four such pairs in all. They are alternatives, and a vehicle has one of each.

    Loading both is not a harmless duplicate: the simulation refuses two ECUs on one CAN
    identifier, because a bus cannot have them either. So the choice has to be made here, and
    made visibly — the discarded variants are returned so the operator is told what was dropped
    and can choose the other one instead.

    `strPrefer` picks between them by matching the source file name, which is the only place the
    variants differ in a way a person can read.
    """
    mapByAddress = {}
    vecOrder = []

    for dtoEcu in vecEcus:
        strKey = (dtoEcu.get("can") or {}).get("request") or (dtoEcu.get("doip") or {}).get(
            "logicalAddress"
        )
        if strKey is None:
            vecOrder.append(dtoEcu)
            continue
        mapByAddress.setdefault(strKey, []).append(dtoEcu)

    vecDropped = []
    for strKey, vecCandidates in mapByAddress.items():
        chosen = vecCandidates[0]
        if strPrefer:
            for candidate in vecCandidates:
                if strPrefer.lower() in candidate["_source"].lower():
                    chosen = candidate
                    break
        for candidate in vecCandidates:
            if candidate is not chosen:
                vecDropped.append((strKey, candidate["name"], candidate["_source"]))
        vecOrder.append(chosen)

    for dtoEcu in vecOrder:
        dtoEcu.pop("_source", None)
    return vecOrder, vecDropped


def CollectPdxPaths(strInput, strScratchDir):
    """Every PDX to read, from a file, a directory, or a zip holding a set of them."""
    if os.path.isdir(strInput):
        return sorted(glob.glob(os.path.join(strInput, "**", "*.pdx"), recursive=True))

    if strInput.lower().endswith(".pdx"):
        return [strInput]

    if zipfile.is_zipfile(strInput):
        with zipfile.ZipFile(strInput) as archive:
            archive.extractall(strScratchDir)
        return sorted(glob.glob(os.path.join(strScratchDir, "**", "*.pdx"), recursive=True))

    sys.exit(f"{strInput} is not a .pdx, a directory, or a zip archive of them")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("input", help="a .pdx, a directory of them, or a zip archive holding them")
    parser.add_argument("-o", "--output", required=True, help="where to write the simulation file")
    parser.add_argument("--vehicle", default=None, help="what to call the vehicle")
    parser.add_argument(
        "--prefer",
        default=None,
        help="when two files describe one address, keep the one whose file name contains this",
    )
    parser.add_argument(
        "--network",
        default=None,
        help="name one CAN bus and put every ECU on it, instead of leaving wiring unstated",
    )
    args = parser.parse_args()

    if odxtools is None:
        sys.exit(c_strMissingOdxtools)

    strScratchDir = tempfile.mkdtemp(prefix="pdx-")
    try:
        vecPaths = CollectPdxPaths(args.input, strScratchDir)
        if not vecPaths:
            sys.exit(f"no .pdx files found in {args.input}")

        print(f"reading {len(vecPaths)} PDX file(s)", file=sys.stderr)
        vecEcus = []
        vecFailed = []
        vecDegraded = []

        for strPath in vecPaths:
            strName, u16Address = ReadNameAndAddress(strPath)
            database, strDegraded = LoadPdx(strPath)
            if database is None:
                # One unreadable supplier file must not cost the other fifty-three. Which ones
                # failed is reported at the end rather than swallowed.
                vecFailed.append((os.path.basename(strPath), strDegraded))
                continue
            if strDegraded:
                vecDegraded.append((os.path.basename(strPath), strDegraded))

            for ecu in database.ecus:
                try:
                    dtoEcu = BuildEcu(ecu, strName, u16Address)
                    dtoEcu["_source"] = os.path.basename(strPath)
                    vecEcus.append(dtoEcu)
                except Exception as error:
                    vecFailed.append((os.path.basename(strPath), str(error)[:120]))

        if not vecEcus:
            sys.exit("no ECUs could be read from those files")

        vecEcus, vecVariants = KeepOneEcuPerAddress(vecEcus, args.prefer)

        # After that, any name still repeated belongs to genuinely different ECUs that the
        # supplier happened to name alike. Disambiguated visibly rather than dropped.
        mapSeen = {}
        for dtoEcu in vecEcus:
            strBase = dtoEcu["name"]
            uCount = mapSeen.get(strBase, 0)
            mapSeen[strBase] = uCount + 1
            if uCount:
                dtoEcu["name"] = f"{strBase} ({uCount + 1})"

        strVehicle = args.vehicle or os.path.splitext(os.path.basename(args.input))[0]
        dtoFile = {
            "simfileVersion": 2,
            "vehicle": (
                f"{strVehicle} (from ODX — data identifier values are placeholders; "
                f"ODX states what an ECU answers and the shape of it, never the value)"
            ),
            "ecus": vecEcus,
        }

        if args.network:
            dtoFile["networks"] = [
                {"id": "can", "name": args.network, "kind": "CAN", "entryPoint": True}
            ]
            for dtoEcu in vecEcus:
                dtoEcu["network"] = "can"

        with open(args.output, "w", encoding="utf-8") as handle:
            json.dump(dtoFile, handle, indent=2)
            handle.write("\n")

        if vecVariants:
            print(
                f"{len(vecVariants)} ECU variant(s) set aside — a vehicle carries one build per "
                f"address, not every build the platform offers. Use --prefer to pick the other:",
                file=sys.stderr,
            )
            for strKey, strName, strSource in vecVariants:
                print(f"  {strKey}  {strName}  ({strSource})", file=sys.stderr)

        uDids = sum(len(dtoEcu.get("dids", {})) for dtoEcu in vecEcus)
        uDtcs = sum(len(dtoEcu.get("dtcs", [])) for dtoEcu in vecEcus)
        print(
            f"wrote {args.output}: {len(vecEcus)} ECU(s), {uDids} data identifier(s), "
            f"{uDtcs} trouble code(s)",
            file=sys.stderr,
        )
        if vecDegraded:
            print(
                f"{len(vecDegraded)} file(s) needed lenient parsing — a reference their own ODX "
                f"does not resolve. What was extracted is sound; the unresolved part was not "
                f"used:",
                file=sys.stderr,
            )
            for strFile, strReason in vecDegraded:
                print(f"  {strFile}: {strReason}", file=sys.stderr)
        if vecFailed:
            print(f"{len(vecFailed)} file(s) could not be read at all:", file=sys.stderr)
            for strFile, strError in vecFailed:
                print(f"  {strFile}: {strError}", file=sys.stderr)
    finally:
        shutil.rmtree(strScratchDir, ignore_errors=True)


if __name__ == "__main__":
    main()
