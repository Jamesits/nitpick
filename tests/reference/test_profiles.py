"""Checks of the profile fixtures.

The parser here reads the bytes without `icc.py`, so a layout error in the
serializer does not hide itself. Pillow's Little CMS build checks that the
valid profiles open as display profiles.
"""

import hashlib
import io
import struct
from fractions import Fraction as Q

import pytest
from PIL import ImageCms

import generate
import icc
import profiles
import reference as ref

FILES, MANIFEST = profiles.build()
ENTRIES = {e["id"]: e for e in MANIFEST["profiles"]}
OPENABLE = [e for e in MANIFEST["profiles"] if e["display"] is not None]
VALID_MHC2 = [e for e in MANIFEST["profiles"] if "error" not in e["mhc2"]]


def u32(b, at):
    return struct.unpack_from(">I", b, at)[0]


def i32(b, at):
    return struct.unpack_from(">i", b, at)[0]


def tag_table(data):
    n = u32(data, 128)
    return {
        data[132 + 12 * i:136 + 12 * i].decode("ascii"): (u32(data, 136 + 12 * i), u32(data, 140 + 12 * i))
        for i in range(n)
    }


def tag(data, sig):
    off, size = tag_table(data)[sig]
    return data[off:off + size]


def decode_mhc2(t):
    """Encoded fields of a valid MHC2 tag; offsets are relative to the tag."""
    assert t[0:4] == b"MHC2" and u32(t, 4) == 0
    count, mo = u32(t, 8), u32(t, 20)
    out = {"min": i32(t, 12), "peak": i32(t, 16), "matrix": None, "lut": None}
    if mo:
        out["matrix"] = [[i32(t, mo + 16 * r + 4 * c) for c in range(4)] for r in range(3)]
    if count:
        out["lut"] = []
        for k in range(3):
            lo = u32(t, 24 + 4 * k)
            assert t[lo:lo + 8] == b"sf32\0\0\0\0"
            out["lut"].append([i32(t, lo + 8 + 4 * i) for i in range(count)])
    return out


@pytest.mark.parametrize(("value", "want"), [
    (Q(1, 1 << 17), 1),
    (Q(3, 1 << 17), 2),
    (Q(-1, 1 << 17), 0),
    (Q(-3, 1 << 17), -1),
    (Q(-5, 1 << 17), -2),
    (Q("0.05"), 3277),
    (Q(-1), -65536),
])
def test_s15f16_rounds_halves_up(value, want):
    assert icc.s15f16(value) == want


def test_s15f16_range():
    assert icc.s15f16(Q((1 << 31) - 1, 65536)) == (1 << 31) - 1
    assert icc.s15f16(-32768) == -(1 << 31)
    with pytest.raises(ValueError):
        icc.s15f16(32768)


def test_generation_is_deterministic():
    assert generate.build_profiles() == generate.build_profiles()


def test_manifest_lists_every_file_with_its_hash():
    assert sorted(FILES) == sorted(e["file"] for e in MANIFEST["profiles"])
    for e in MANIFEST["profiles"]:
        data = FILES[e["file"]]
        assert e["size"] == len(data)
        assert e["sha256"] == hashlib.sha256(data).hexdigest()


@pytest.mark.parametrize("e", OPENABLE, ids=lambda e: e["id"])
def test_header_and_profile_id(e):
    data = FILES[e["file"]]
    assert u32(data, 0) == len(data) and len(data) % 4 == 0
    assert data[36:40] == b"acsp" and data[12:24] == b"mntrRGB XYZ "
    zeroed = bytearray(data)
    for a, b in ((44, 48), (64, 68), (84, 100)):
        zeroed[a:b] = bytes(b - a)
    assert data[84:100] == hashlib.md5(zeroed).digest()


def test_mhc2_set_differs_only_in_desc_and_mhc2():
    sets = [FILES[e["file"]] for e in MANIFEST["profiles"] if e["use"] == "mhc2"]
    want = {s: tag(sets[0], s) for s in tag_table(sets[0]) if s not in ("desc", "MHC2")}
    assert "lumi" in want
    for data in sets:
        assert {s: tag(data, s) for s in tag_table(data) if s not in ("desc", "MHC2")} == want
    assert len({decode_mhc2(tag(d, "MHC2"))["peak"] for d in sets}) == 1


def test_ordinary_profiles_have_no_mhc2():
    for e in MANIFEST["profiles"]:
        if e["use"] == "ordinary":
            assert "MHC2" not in tag_table(FILES[e["file"]])
            assert e["mhc2"] == {"error": "missing_tag"}


@pytest.mark.parametrize("e", VALID_MHC2, ids=lambda e: e["id"])
def test_mhc2_bytes_match_manifest(e):
    got = decode_mhc2(tag(FILES[e["file"]], "MHC2"))
    m = e["mhc2"]
    assert got["min"] == m["min_nits"]["encoded"]
    assert got["peak"] == m["peak_nits"]["encoded"]
    assert got["matrix"] == (m["matrix"] and m["matrix"]["encoded"])
    assert got["lut"] == (m["lut"] and m["lut"]["encoded"])


@pytest.mark.parametrize("e", OPENABLE, ids=lambda e: e["id"])
def test_display_tags_match_manifest(e):
    data, d = FILES[e["file"]], e["display"]

    def xyz(sig):
        t = tag(data, sig)
        assert t[:8] == b"XYZ \0\0\0\0"
        return [i32(t, 8 + 4 * k) for k in range(3)]

    assert xyz("wtpt") == d["media_white"]["encoded"]
    assert [xyz(s) for s in ("rXYZ", "gXYZ", "bXYZ")] == d["colorants"]["encoded"]
    chad = tag(data, "chad")
    assert [[i32(chad, 8 + 12 * r + 4 * c) for c in range(3)] for r in range(3)] == d["chad"]["encoded"]
    for s in ("rTRC", "gTRC", "bTRC"):
        t = tag(data, s)
        assert t[:8] == b"para\0\0\0\0" and struct.unpack_from(">H", t, 8)[0] == d["trc"]["function"]
        assert [i32(t, 12 + 4 * k) for k in range((len(t) - 12) // 4)] == d["trc"]["encoded"]
    if d["lumi"] is None:
        assert "lumi" not in tag_table(data)
    else:
        assert xyz("lumi") == [0, d["lumi"]["encoded"], 0]


def test_luminance_values_are_distinct():
    m = ENTRIES["mhc2_identity"]
    fm = MANIFEST["frame_metadata"]
    values = [
        m["mhc2"]["min_nits"]["intended"], m["mhc2"]["peak_nits"]["intended"],
        m["display"]["lumi"]["intended"], fm["max_mastering_nits"], fm["min_mastering_nits"],
        fm["light_level"]["max_cll"], fm["light_level"]["max_fall"],
    ]
    assert len(set(values)) == len(values)


def test_chad_maps_d65_to_d50():
    assert ref.mat_vec(profiles.CHAD, ref.xyz_of_xy(ref.D65)) == icc.D50


def test_swap_rg_exchanges_red_and_green_xyz():
    m = ref.rgb_to_xyz("bt709")
    a = [row[:3] for row in profiles.rgb_action_to_xyz(profiles.SWAP_RG, "bt709")]
    col = [tuple(m[i][j] for i in range(3)) for j in range(3)]
    assert ref.mat_vec(a, col[0]) == col[1]
    assert ref.mat_vec(a, col[1]) == col[0]
    assert ref.mat_vec(a, col[2]) == col[2]


def test_swap_rg_agrees_with_public_sample():
    # profiles/SwapRedGreen.icm in github.com/dantmnf/MHC2, printed to 6 decimals.
    sample = ((0.769257, 0.187073, 0.029602), (2.115753, -0.715317, -0.271454), (0.420456, -0.340866, 0.94606))
    got = ENTRIES["mhc2_swap_rg"]["mhc2"]["matrix"]["intended"]
    for row, want in zip(got, sample):
        for v, w in zip(row, want):
            assert abs(v - w) < 2e-5


def test_signed_case_has_half_steps_of_both_signs():
    m = ENTRIES["mhc2_signed"]["mhc2"]
    pairs = [
        (i, e)
        for part in ("matrix", "lut")
        for ri, ei in zip(m[part]["intended"], m[part]["encoded"])
        for i, e in zip(ri, ei)
    ]
    halves = [(Q(i) * 65536, e) for i, e in pairs if (Q(i) * 65536).denominator == 2]
    assert any(x > 0 for x, _ in halves) and any(x < 0 for x, _ in halves)
    assert all(e == x + Q(1, 2) for x, e in halves)
    assert any(e < 0 for _, e in pairs)
    assert any(row[3] != 0 for row in m["matrix"]["encoded"])


def test_relocated_case_is_not_contiguous():
    data = FILES["mhc2_relocated.icc"]
    t = tag(data, "MHC2")
    offsets = [u32(t, 20 + 4 * k) for k in range(4)]
    assert offsets != sorted(offsets)
    table = tag_table(data)
    assert table["MHC2"][0] == min(o for o, _ in table.values())
    assert decode_mhc2(t) == decode_mhc2(tag(FILES["mhc2_combined.icc"], "MHC2"))


def mhc2_location(pid):
    data = FILES[f"{pid}.icc"]
    off, size = tag_table(data)["MHC2"]
    return data, off, size


@pytest.mark.parametrize(("pid", "field", "length"), [
    ("mhc2_matrix_outside_tag", 20, 48),
    ("mhc2_lut_outside_tag", 32, 16),
])
def test_out_of_tag_array_stays_in_file(pid, field, length):
    data, off, size = mhc2_location(pid)
    end = u32(data, off + field) + length
    assert size < end and off + end <= len(data)


def test_file_relative_offsets_leave_the_tag():
    data, off, size = mhc2_location("mhc2_file_relative_offsets")
    base = tag(FILES["mhc2_matrix.icc"], "MHC2")
    for k in range(4):
        got = u32(data, off + 20 + 4 * k)
        assert got == u32(base, 20 + 4 * k) + off and got >= size


def test_wrapped_offsets_land_inside_the_file():
    data, off, _ = mhc2_location("mhc2_matrix_offset_wrap")
    assert (off + u32(data, off + 20)) % (1 << 32) == 128
    data, off, _ = mhc2_location("mhc2_lut_offset_wrap")
    assert (u32(data, off + 24) + 8) % (1 << 32) == 4


def test_count_cases():
    _, off, size = mhc2_location("mhc2_truncated_header")
    assert size == 32
    for pid, count in (("mhc2_lut_count_1", 1), ("mhc2_lut_count_4097", 4097), ("mhc2_lut_4096", 4096)):
        data, off, size = mhc2_location(pid)
        assert u32(data, off + 8) == count
        assert size == 36 + 48 + 3 * (8 + 4 * count)


@pytest.mark.parametrize("e", OPENABLE, ids=lambda e: e["id"])
def test_little_cms_opens_display_profile(e):
    p = ImageCms.ImageCmsProfile(io.BytesIO(FILES[e["file"]])).profile
    d = e["display"]
    assert p.version == 4.3 and p.device_class == "mntr" and p.is_matrix_shaper
    assert list(p.media_white_point[0]) == [v / 65536 for v in d["media_white"]["encoded"]]
    got = [list(c[0]) for c in (p.red_colorant, p.green_colorant, p.blue_colorant)]
    assert got == [[v / 65536 for v in c] for c in d["colorants"]["encoded"]]
    if d["lumi"] is not None:
        assert p.luminance[0][1] == d["lumi"]["encoded"] / 65536


def test_little_cms_rejects_truncated_header():
    with pytest.raises(OSError):
        ImageCms.ImageCmsProfile(io.BytesIO(FILES["icc_truncated.icc"]))
