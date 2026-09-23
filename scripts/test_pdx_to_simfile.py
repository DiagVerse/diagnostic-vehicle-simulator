"""Tests for the PDX-to-simfile converter's readers.

Run:  python3 scripts/test_pdx_to_simfile.py

Deliberately covers only the parts that are plain Python — how an address, a name, a placeholder
and a variant choice are decided. Those are where the mistakes live, and they need no ODX file
and no `odxtools`, so they run anywhere including CI. Reading actual ODX is `odxtools`' job and
is tested by the project that owns it.

Every case here is drawn from the real delivery this converter was written against, because the
shapes that broke it were the supplier's, not ones anybody would invent.
"""

import importlib.util
import os
import sys

c_strScriptPath = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pdx-to-simfile.py")
spec = importlib.util.spec_from_file_location("pdx_to_simfile", c_strScriptPath)
conv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(conv)

g_uFailures = 0


def Check(strName, got, want):
    global g_uFailures
    if got == want:
        print(f"  PASS  {strName}")
        return
    g_uFailures += 1
    print(f"  FAIL  {strName}\n          got  {got!r}\n          want {want!r}")


def TestAddressingTable():
    print("addressing table")

    # A real row set: the format string is the only thing saying which identifier is which, and
    # the 0xFFFFFFFF row is the standard's "not used" marker.
    vecTable = [
        "0", "normal segmented 29-bit transmit with FC", "416998129",
        "0", "normal segmented 29-bit receive with FC", "417001954",
        "0", "normal unsegmented 29-bit receive", "4294967295",
    ]
    Check(
        "request and response are told apart by the format string",
        conv.ReadAddressingTable([vecTable]),
        [(416998129, 417001954)],
    )

    Check(
        "the not-used marker is skipped rather than read as an address",
        conv.ReadAddressingTable([["0", "normal unsegmented 29-bit receive", "4294967295"]]),
        [],
    )

    # Both links, as an ECU reachable on either declares them.
    vecEleven = [
        "0", "normal segmented 11-bit transmit with FC", "1806",
        "0", "normal segmented 11-bit receive with FC", "1807",
    ]
    Check(
        "an ECU on two links yields both pairs, for the caller to choose between",
        conv.ReadAddressingTable([vecTable, vecEleven]),
        [(416998129, 417001954), (1806, 1807)],
    )


def TestLogicalAddress():
    print("DoIP logical address")

    # The DoIP row has no transmit/receive format, which is how it is told from a CAN one.
    Check(
        "a short numeric table is the logical address",
        conv.ReadLogicalAddress([["4322", "0", "None"]]),
        4322,
    )
    Check(
        "a CAN row is not mistaken for one",
        conv.ReadLogicalAddress(
            [["0", "normal segmented 29-bit transmit with FC", "416998129"]]
        ),
        None,
    )


def TestNameAndAddress():
    print("name and address from the file name")

    # Suppliers spell the separator both ways in one delivery.
    Check(
        "a spaced separator",
        conv.ReadNameAndAddress("30009_ODX_0x0004_EPS - N_PZ1A_EPAS_UDS_v2.0_24th June 2021.pdx"),
        ("EPS", 0x0004),
    )
    Check(
        "an underscored separator",
        conv.ReadNameAndAddress(
            "30033_ODX_0x0058_Navigation-UCC-ITM_-_N_PZ1A_ITM11_UDS_20240911.pdx"
        ),
        ("Navigation-UCC-ITM", 0x0058),
    )
    Check(
        "a 29-bit ECU keeps the marker the supplier put in its name",
        conv.ReadNameAndAddress("30003_ODX_0x002D_ABS-VDC_29b - N_PZ1A_ABS_UDS_20250116.pdx"),
        ("ABS-VDC 29b", 0x002D),
    )
    Check(
        "a file name in no recognised shape yields no address rather than a wrong one",
        conv.ReadNameAndAddress("some-other-file.pdx"),
        ("some-other-file", None),
    )


def TestPlaceholderValue():
    print("placeholder values")

    strText, strHex = conv.BuildPlaceholderValue(17, 17, "PCM", 0xF190)
    Check("a fixed-width identifier gets exactly its width", len(strText or ""), 17)
    Check("and is text, so it reads as a placeholder", strHex, None)

    # ODX's MIN-MAX-LENGTH-TYPE: every length in the range is legal.
    strText, _ = conv.BuildPlaceholderValue(1, 64, "PCM", 0xF18A)
    Check("a variable-length identifier gets a readable length", strText, "ODX-PCM-F18A")

    strText, _ = conv.BuildPlaceholderValue(1, 8, "PCM", 0xF18A)
    Check("clamped to the maximum the file allows", len(strText), 8)

    # Too short to carry a label: "O" would read like data rather than a placeholder.
    strText, strHex = conv.BuildPlaceholderValue(1, 1, "PCM", 0xF1A0)
    Check("a one-byte identifier gets hex, not a truncated word", (strText, strHex), (None, "00"))

    strText, _ = conv.BuildPlaceholderValue(None, None, "PCM", 0xF190)
    Check("no stated length at all falls back to a default", len(strText), 16)


def TestVariantSelection():
    print("ECU variants")

    def Ecu(strName, strRequest, strSource):
        return {"name": strName, "can": {"request": strRequest}, "_source": strSource}

    vecEcus = [
        Ecu("EPS", "0x742", "30009_EPS_EPAS.pdx"),
        Ecu("EPS", "0x742", "30030_EPS_EPAS_AD2.pdx"),
        Ecu("ABS", "0x743", "30003_ABS.pdx"),
    ]

    vecKept, vecDropped = conv.KeepOneEcuPerAddress([dict(e) for e in vecEcus], None)
    Check("two files for one address become one ECU", len(vecKept), 2)
    Check("and the discarded one is reported, not silently lost", len(vecDropped), 1)
    Check("the first is kept by default", vecDropped[0][2], "30030_EPS_EPAS_AD2.pdx")
    Check("the bookkeeping field does not reach the file", "_source" in vecKept[0], False)

    vecKept, vecDropped = conv.KeepOneEcuPerAddress([dict(e) for e in vecEcus], "AD2")
    Check("--prefer picks the other build", vecDropped[0][2], "30009_EPS_EPAS.pdx")

    # An ECU the simulation cannot key by address is kept rather than dropped.
    vecKept, _ = conv.KeepOneEcuPerAddress([{"name": "Unaddressed", "_source": "x.pdx"}], None)
    Check("an ECU with no address is not discarded", len(vecKept), 1)


def TestComparamFlattening():
    print("comparam values")

    Check("a scalar becomes one string", conv.ParseComparamValue(417018865), ["417018865"])
    Check("a nested list is flattened", conv.ParseComparamValue([["a", "b"], "c"]), ["a", "b", "c"])
    Check("nothing stays nothing", conv.ParseComparamValue(None), [])


def main():
    TestAddressingTable()
    TestLogicalAddress()
    TestNameAndAddress()
    TestPlaceholderValue()
    TestVariantSelection()
    TestComparamFlattening()

    print()
    if g_uFailures:
        print(f"{g_uFailures} failure(s)")
        sys.exit(1)
    print("all passed")


if __name__ == "__main__":
    main()
