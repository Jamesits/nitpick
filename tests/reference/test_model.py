"""Checks of the display model expectations.

They show that the cases find a wrong stage order, a transposed matrix, a
matrix in the wrong basis and a wrong SDR transfer.
"""

from fractions import Fraction as Q

import pytest

import model
import profiles
import reference as ref
from test_profiles import ENTRIES, MANIFEST

CASES = {c["id"]: c for c in MANIFEST["model"]}
CALIBRATIONS = {**profiles.MHC2_SET, **profiles.MHC2_VALID}


def codes(c):
    return [e["code"] for e in c["expected"]]


def calibration(c):
    return model.Calibration.from_mhc2(CALIBRATIONS[c["calibration"]["profile"]])


def wire_space(c):
    return {k: v for k, v in c["wire"].items() if k != "depth"}


def other_codes(c, matrix):
    """Codes of the case with `matrix` in place of $M_{wire}^{-1} A M_{src}$."""
    cal, wire = calibration(c), wire_space(c)
    return [model.evaluate(cal, c["source"], wire, c["wire"]["depth"], v, matrix=matrix, check=False)["code"]
            for v in c["input"]]


def codes_with(c, cal):
    return [model.evaluate(cal, c["source"], wire_space(c), c["wire"]["depth"], v, check=False)["code"]
            for v in c["input"]]


def test_ids_and_calibrations():
    assert len(CASES) == len(MANIFEST["model"])
    for c in CASES.values():
        assert len(c["input"]) == len(c["expected"]) > 0
        p = c["calibration"].get("profile")
        if p is not None:
            assert "error" not in ENTRIES[p]["mhc2"]


def test_every_clip_state_occurs():
    seen = {(stage, s) for c in CASES.values() for e in c["expected"] for stage in ("encode", "output")
            for s in e["clip"][stage]}
    assert seen == {(stage, s) for stage in ("encode", "output") for s in ("none", "low", "high")}


def test_xyz_gain_halves_the_wire_luminance():
    base, gain = CASES["pq_identity"], CASES["pq_xyz_gain"]
    assert base["input"] == gain["input"]
    for a, b in zip(base["expected"], gain["expected"]):
        assert b["wire_nits"] == [v / 2 for v in a["wire_nits"]]


def test_lut_knots_give_the_stored_entries():
    c = CASES["pq_lut"]
    luts = calibration(c).luts
    for v, e in zip(c["input"], c["expected"]):
        k = Q(v[0]) * 255
        if v[0] == v[1] == v[2] and abs(k - round(k)) < Q(1, 10**9):
            for ch in range(3):
                assert e["lut"][ch] == pytest.approx(float(luts[ch][round(k)]), abs=1e-12)


def reversed_order(c, rgb):
    """The LUT before the matrix: encode without the matrix, apply the LUT,
    decode, apply the matrix in the wire basis and encode again."""
    cal, src, wire = calibration(c), c["source"], c["wire"]
    plain = model.Calibration(model.IDENTITY, None)
    lin = ref.mat_vec(model.mpf_mat(model.wire_matrix(plain, src["primaries"], wire["primaries"])),
                      [ref.mpf(model.decode(src["encoding"], Q(v))) for v in rgb])
    signal = [model.encode(wire["encoding"], n)[0] for n in lin]
    lut = [model.lut_eval(cal.luts[k], s) for k, s in enumerate(signal)]
    back = [model.decode(wire["encoding"], Q(ref.exact(model.clamp(v)[0]))) for v in lut]
    m = model.wire_matrix(cal, wire["primaries"], wire["primaries"])
    out = [model.encode(wire["encoding"], n)[0] for n in ref.mat_vec(model.mpf_mat(m), back)]
    return [ref.round_half_up(v * ref.max_code(wire["depth"])) for v in out]


@pytest.mark.parametrize("cid", ["pq_combined", "sdr_srgb_combined"])
def test_combined_cases_find_reversed_order(cid):
    c = CASES[cid]
    assert [reversed_order(c, v) for v in c["input"]] != codes(c)


def test_swap_exchanges_red_and_green():
    c = CASES["sdr_srgb_swap_rg"]
    got = dict(zip(map(tuple, c["input"]), codes(c)))
    assert got[(1.0, 0.0, 0.0)] == [0, 255, 0]
    assert got[(0.0, 1.0, 0.0)] == [255, 0, 0]
    assert got[(0.0, 0.0, 1.0)] == [0, 0, 255]


def test_swap_finds_transpose_and_wrong_basis():
    c = CASES["sdr_srgb_swap_rg"]
    a = calibration(c).matrix
    m709 = model.npm("bt709")
    transposed = tuple(tuple(a[j][i] for j in range(3)) for i in range(3))
    wrong = {
        "transpose": ref.mat_mul(ref.mat_inv(m709), ref.mat_mul(transposed, m709)),
        # The XYZ matrix applied to linear RGB.
        "rgb basis": a,
    }
    for name, m in wrong.items():
        assert other_codes(c, m) != codes(c), name


def test_srgb_and_gamma22_differ_near_black():
    srgb, g22 = CASES["sdr_srgb_identity"], CASES["sdr_gamma22_identity"]
    assert srgb["input"] == g22["input"]
    near_black = 0
    for v, a, b in zip(srgb["input"], srgb["expected"], g22["expected"]):
        if v[0] == v[1] == v[2] and 0 < v[0] <= 0.01:
            near_black += 1
            assert a["code"] != b["code"]
            # Below 0.0031308 the sRGB encoding is linear.
            if v[0] < float(model.SRGB_ENCODE_KNEE):
                assert a["signal"][0] == pytest.approx(12.92 * v[0], rel=1e-14)
    assert near_black >= 4


IDD_CASES = {"idd_affine": model.IDD, "idd_affine_lut": model.IDD_WITH_LUT, "idd_sdr_affine": model.IDD}


@pytest.mark.parametrize("cid", list(IDD_CASES))
def test_idd_cases_find_a_wrong_offset_or_scalar(cid):
    """Each wrong reading of the 3×4 callback gives other codes: the MHC2
    reading (fourth column ignored, no scalar), no offset, the scalar only
    on the 3×3 part, and the scalar only on the offset."""
    c, t = CASES[cid], IDD_CASES[cid]
    s, u = t.scalar, t.offset_nits
    a = tuple(tuple(v for v in row[:3]) for row in t.matrix)
    scaled = tuple(tuple(s * v for v in row) for row in a)
    offset = tuple(row[3] * u for row in t.matrix)
    wrong = {
        "mhc2": model.Calibration(a, t.luts),
        "no offset": model.Calibration(scaled, t.luts),
        "unscaled offset": model.Calibration(scaled, t.luts, offset),
        "unscaled matrix": model.Calibration(a, t.luts, tuple(s * o for o in offset)),
    }
    for name, cal in wrong.items():
        assert codes_with(c, cal) != codes(c), name


def test_idd_black_gives_the_scaled_offset():
    c, t = CASES["idd_affine"], model.IDD
    offset = [t.scalar * row[3] * t.offset_nits for row in t.matrix]
    want = ref.mat_vec(ref.mat_inv(model.npm(c["wire"]["primaries"])), offset)
    black = c["expected"][c["input"].index([0.0, 0.0, 0.0])]
    assert black["wire_nits"] == [ref.to_f64(v) for v in want]
    assert black["code"] != [0, 0, 0]
