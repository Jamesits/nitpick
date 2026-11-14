"""Checks of the conventional transform expectations and their fixtures."""

import struct

import pytest

import profiles
import transforms
from test_profiles import FILES, MANIFEST, tag, tag_table, u32

CASES = {c["id"]: c for c in MANIFEST["transforms"]}
PROFILE_IDS = {e["id"] for e in MANIFEST["profiles"]}
# Far above the f32 error bound of the adapter test, far below a white
# point change.
DISTINCT = 1e-3


def max_diff(a, b):
    return max(abs(x - y) for pa, pb in zip(a, b) for x, y in zip(pa, pb))


def test_ids_and_endpoints():
    assert len(CASES) == len(MANIFEST["transforms"])
    for c in CASES.values():
        assert {c["source"], c["target"]} - {None} <= PROFILE_IDS
        assert len(c["input"]) == len(c["expected"]) > 0


def test_identity_returns_input():
    c = CASES["srgb_identity"]
    assert c["expected"] == c["input"]


@pytest.mark.parametrize("cid", [
    "srgb_to_p3_gamma22_perceptual",
    "srgb_to_p3_gamma22_bpc",
])
def test_intent_and_bpc_do_not_change_matrix_shaper_result(cid):
    assert CASES[cid]["expected"] == CASES["srgb_to_p3_gamma22"]["expected"]


def test_adaptation_applies_only_to_unadapted_absolute_intent():
    rel = CASES["srgb_to_d50_relative"]["expected"]
    assert CASES["srgb_to_d50_relative_unadapted"]["expected"] == rel
    assert CASES["srgb_to_d50_absolute_adapted"]["expected"] == rel
    assert max_diff(CASES["srgb_to_d50_absolute_unadapted"]["expected"], rel) > DISTINCT


def test_relative_intent_keeps_white_neutral():
    c = CASES["srgb_to_d50_relative"]
    white = c["expected"][c["input"].index([1.0, 1.0, 1.0])]
    assert max(abs(v - 1) for v in white) < DISTINCT


def test_unadapted_white_is_d65_in_d50_display():
    c = CASES["srgb_to_d50_absolute_unadapted"]
    r, g, b = c["expected"][c["input"].index([1.0, 1.0, 1.0])]
    # D65 is bluer than D50.
    assert b - r > 10 * DISTINCT


def test_stored_values_change_the_result():
    """The expectations use stored tag values; intended values differ by
    more than the f32 error bound of the adapter test."""
    d = profiles.DISPLAYS["ordinary_srgb"]
    exact = transforms.Stored(d)
    exact.m = tuple(tuple(d.adapted[j][i] for j in range(3)) for i in range(3))
    pcs = transforms.to_pcs(exact, [1, 0, 0])
    want = CASES["srgb_to_pcs"]["expected"][CASES["srgb_to_pcs"]["input"].index([1.0, 0.0, 0.0])]
    assert max(abs(float(p) - w) for p, w in zip(pcs, want)) > 2 ** -20


def test_d50_display_has_identity_chad():
    d = next(e for e in MANIFEST["profiles"] if e["id"] == "display_d50")["display"]
    assert d["chad"]["encoded"] == [[65536, 0, 0], [0, 65536, 0], [0, 0, 65536]]
    assert d["colorants"]["intended"] == d["colorants"]["native"]


def test_vcgt_profile_adds_only_vcgt():
    data, base = FILES["vcgt_srgb.icc"], FILES["ordinary_srgb.icc"]
    rest = {s: tag(base, s) for s in tag_table(base) if s != "desc"}
    assert {s: tag(data, s) for s in tag_table(data) if s not in ("desc", "vcgt")} == rest
    t = tag(data, "vcgt")
    assert t[:8] == b"vcgt\0\0\0\0" and u32(t, 8) == 0
    channels, count, size = struct.unpack_from(">HHH", t, 12)
    assert (channels, size) == (3, 2) and len(t) == 18 + channels * count * size
    tables = [struct.unpack_from(f">{count}H", t, 18 + 2 * count * k) for k in range(3)]
    identity = tuple(round(65535 * i / (count - 1)) for i in range(count))
    assert all(tb != identity for tb in tables) and len(set(tables)) == 3

