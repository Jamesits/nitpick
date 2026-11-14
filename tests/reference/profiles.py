"""ICC and MHC2 profile fixtures and their manifest.

Every matrix, curve and luminance value is defined here as an exact
rational before serialization. The manifest stores the intended value
(nearest binary64) and the signed 15.16 integer written to the profile
(`icc.s15f16`: exact halves go up, also for negative values).

Uses in the manifest:
- `ordinary`: no MHC2 tag; for profile assignment on every OS.
- `mhc2`: Windows calibration set. All tags except `desc` and `MHC2` are
  byte-identical, so only the calibration transform differs.
- `adapter`: boundary and malformed cases for the profile adapter only.
  Never install them.
"""

import hashlib
from fractions import Fraction as Q

import icc
import reference as ref

# Bradford cone response matrix (ICC.1:2010 Annex E).
BRADFORD = (
    (Q("0.8951"), Q("0.2664"), Q("-0.1614")),
    (Q("-0.7502"), Q("1.7135"), Q("0.0367")),
    (Q("0.0389"), Q("-0.0685"), Q("1.0296")),
)


def bradford(src, dst):
    """XYZ adaptation matrix from white `src` to white `dst`."""
    s, d = ref.mat_vec(BRADFORD, src), ref.mat_vec(BRADFORD, dst)
    gain = tuple(tuple(d[i] / s[i] if i == j else Q(0) for j in range(3)) for i in range(3))
    return ref.mat_mul(ref.mat_inv(BRADFORD), ref.mat_mul(gain, BRADFORD))


CHAD = bradford(ref.xyz_of_xy(ref.D65), icc.D50)

# parametricCurveType function and parameters.
# IEC 61966-2-1 sRGB: g, a, b, c, d.
SRGB_TRC = (3, (Q("2.4"), Q(1, Q("1.055")), Q("0.055") / Q("1.055"), Q(1, Q("12.92")), Q("0.04045")))
GAMMA22_TRC = (0, (Q("2.2"),))

# Display metadata of the MHC2 and adapter sets. All luminance values in
# this file differ, so a swapped field cannot pass.
MHC2_MIN_NITS = Q("0.05")
MHC2_PEAK_NITS = Q(1000)
LUMI_NITS = Q(400)
# HDR10 metadata for frames shown during calibration checks.
FRAME_METADATA = {
    "primaries": [[ref.to_f64(c) for c in p] for p in ref.PRIMARIES["bt2020"]],
    "white": [ref.to_f64(c) for c in ref.D65],
    "max_mastering_nits": 4000.0,
    "min_mastering_nits": 0.005,
    "light_level": {"kind": "explicit", "max_cll": 800.0, "max_fall": 200.0},
}

IDENTITY = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0))
# Asymmetric XYZ matrix with unequal row sums.
MATRIX = (
    (Q("0.9"), Q("0.05"), Q("0.02"), 0),
    (Q("0.03"), Q("0.85"), Q("0.04"), 0),
    (Q("0.01"), Q("0.06"), Q("0.8"), 0),
)
# Red/green exchange as an RGB action in the BT.709/D65 basis, without D50
# adaptation: A = M P M^-1, so that A maps the XYZ of red to that of green.
SWAP_RG = ((0, 1, 0), (1, 0, 0), (0, 0, 1))


def rgb_action_to_xyz(action, space):
    m = ref.rgb_to_xyz(space)
    a = ref.mat_mul(m, ref.mat_mul(action, ref.mat_inv(m)))
    return tuple((*row, 0) for row in a)


IDENTITY_LUT = ([0, 1], [0, 1], [0, 1])


def lut(count, curves):
    return tuple([f(Q(i, count - 1)) for i in range(count)] for f in curves)


# Unequal channels with interior knots: R ends below 1, G starts above 0.
LUT_256 = lut(256, (
    lambda x: Q("0.96") * (x + x * x) / 2,
    lambda x: Q("0.02") + Q("0.98") * x,
    lambda x: Q("0.99") * (3 * x - x * x) / 2,
))
LUT_4096 = lut(4096, (lambda x: x, lambda x: x * x, lambda x: 1 - (1 - x) ** 2))


def half(k):
    """k half steps of 2^-16."""
    return Q(k, 1 << 17)


# Negative values and exact half steps of both signs; nonzero ignored
# fourth column.
SIGNED_MATRIX = (
    (1 + half(3), Q(-1, 8), -half(3), half(1)),
    (-half(1), 1, -half(5), -half(1)),
    (Q(-1, 16), Q(1, 16), Q(7, 8), Q(3, 2)),
)
SIGNED_LUT = (
    [Q(-1, 16), Q(1, 2) + half(1), Q(17, 16)],
    [-half(3), Q(1, 2), 1 + half(3)],
    [0, Q(1, 4) - half(1), 1],
)


class Display:
    def __init__(self, space, trc, lumi):
        self.space, self.trc, self.lumi = space, trc, lumi
        m = ref.rgb_to_xyz(space)
        self.native = tuple(tuple(m[i][j] for i in range(3)) for j in range(3))
        self.adapted = tuple(ref.mat_vec(CHAD, c) for c in self.native)

    def tags(self):
        """Tags other than `desc` and `MHC2`, as `(signature, data)`."""
        f, params = self.trc
        trc = icc.para_type(f, params)
        tags = [
            ("cprt", icc.mluc_type("nitpick test fixture, GPL-3.0-or-later")),
            ("wtpt", icc.xyz_type(icc.D50)),
            ("chad", icc.sf32_type([v for row in CHAD for v in row])),
            ("rXYZ", icc.xyz_type(self.adapted[0])),
            ("gXYZ", icc.xyz_type(self.adapted[1])),
            ("bXYZ", icc.xyz_type(self.adapted[2])),
        ]
        if self.lumi is not None:
            # ICC.1:2010 9.2.32: X and Z are zero.
            tags.append(("lumi", icc.xyz_type((0, self.lumi, 0))))
        return tags + [("rTRC", trc), ("gTRC", trc), ("bTRC", trc)]

    def manifest(self):
        f, params = self.trc
        return {
            "primaries": [[ref.to_f64(c) for c in p] for p in ref.PRIMARIES[self.space]],
            "white": [ref.to_f64(c) for c in ref.D65],
            "media_white": serialized(icc.D50),
            "chad": serialized(CHAD),
            "colorants": {"native": f64(self.native), **serialized(self.adapted)},
            "trc": {"function": f, **serialized(params)},
            "lumi": None if self.lumi is None else serialized(self.lumi),
        }


SRGB_DISPLAY = Display("bt709", SRGB_TRC, LUMI_NITS)


def f64(v):
    return [f64(x) for x in v] if isinstance(v, (list, tuple)) else ref.to_f64(v)


def encoded(v):
    return [encoded(x) for x in v] if isinstance(v, (list, tuple)) else icc.s15f16(v)


def serialized(v):
    return {"intended": f64(v), "encoded": encoded(v)}


class Mhc2:
    def __init__(self, matrix, luts, rgb_action=None):
        self.matrix, self.luts, self.rgb_action = matrix, luts, rgb_action
        self.count = len(luts[0])

    def tag(self, **layout):
        return icc.mhc2_type(self.count, MHC2_MIN_NITS, MHC2_PEAK_NITS, self.matrix, self.luts, **layout)

    def manifest(self):
        out = {
            "min_nits": serialized(MHC2_MIN_NITS),
            "peak_nits": serialized(MHC2_PEAK_NITS),
            "matrix": None if self.matrix is None else serialized(self.matrix),
            "lut": serialized(self.luts) if self.count else None,
        }
        if self.rgb_action is not None:
            out["rgb_action"] = {"space": self.rgb_action[1], "matrix": f64(self.rgb_action[0])}
        return out


# Table order; MHC2 is before the curves, so data follows it in the file.
TAG_ORDER = ("desc", "cprt", "wtpt", "chad", "rXYZ", "gXYZ", "bXYZ", "lumi", "MHC2", "rTRC", "gTRC", "bTRC")


def profile_tags(pid, display, mhc2_data):
    tags = dict(display.tags())
    tags["desc"] = icc.mluc_type(f"nitpick {pid}")
    if mhc2_data is not None:
        tags["MHC2"] = mhc2_data
    return [(s, tags[s]) for s in TAG_ORDER if s in tags]


def error(kind):
    return {"error": kind}


def put(b, field, value):
    b[field:field + 4] = (value % (1 << 32)).to_bytes(4, "big")


def malform(pid, b, pos, offset):
    """Apply defect `pid` to MHC2 data `b` placed at file `offset`; return the data."""
    lut_bytes = 8 + 4 * int.from_bytes(b[icc.MHC2_COUNT:icc.MHC2_COUNT + 4], "big")
    match pid:
        case "mhc2_truncated_header":
            return b[:32]
        case "mhc2_wrong_type_signature":
            b[0:4] = b"MHC1"
        case "mhc2_wrong_lut_signature":
            b[pos["g"]:pos["g"] + 4] = b"curv"
        case "mhc2_file_relative_offsets":
            # Offsets relative to the file start instead of the tag start.
            for f in (icc.MHC2_MATRIX_OFFSET, *icc.MHC2_LUT_OFFSET.values()):
                put(b, f, int.from_bytes(b[f:f + 4], "big") + offset)
        case "mhc2_matrix_offset_wrap":
            # Tag start + offset wraps in 32 bits to file offset 128, the tag table.
            put(b, icc.MHC2_MATRIX_OFFSET, 128 - offset)
        case "mhc2_lut_offset_wrap":
            # Offset + 8 wraps in 32 bits to tag offset 4.
            put(b, icc.MHC2_LUT_OFFSET["r"], -4)
        case "mhc2_matrix_outside_tag":
            # The array ends 4 bytes after the tag; the file continues.
            put(b, icc.MHC2_MATRIX_OFFSET, len(b) - 48 + 4)
        case "mhc2_lut_outside_tag":
            put(b, icc.MHC2_LUT_OFFSET["b"], len(b) - lut_bytes + 4)
        case _:
            raise KeyError(pid)
    return b


MALFORMED = (
    "mhc2_truncated_header",
    "mhc2_wrong_type_signature",
    "mhc2_wrong_lut_signature",
    "mhc2_file_relative_offsets",
    "mhc2_matrix_offset_wrap",
    "mhc2_lut_offset_wrap",
    "mhc2_matrix_outside_tag",
    "mhc2_lut_outside_tag",
)


def patched(base, pid):
    """MHC2 data as a function of its file offset, for `icc.serialize`."""
    data, pos = base
    return lambda offset: bytes(malform(pid, bytearray(data), pos, offset))


def cases():
    """`(id, use, display, tags, data_order, mhc2 expectation)` in manifest order."""
    srgb = SRGB_DISPLAY
    mhc2 = {
        "mhc2_identity": Mhc2(IDENTITY, IDENTITY_LUT),
        "mhc2_matrix": Mhc2(MATRIX, IDENTITY_LUT),
        "mhc2_lut": Mhc2(IDENTITY, LUT_256),
        "mhc2_combined": Mhc2(MATRIX, LUT_256),
        "mhc2_swap_rg": Mhc2(rgb_action_to_xyz(SWAP_RG, "bt709"), IDENTITY_LUT, (SWAP_RG, "bt709")),
    }
    valid = {
        "mhc2_signed": Mhc2(SIGNED_MATRIX, SIGNED_LUT),
        "mhc2_zero_matrix_offset": Mhc2(None, LUT_256),
        "mhc2_zero_lut_count": Mhc2(MATRIX, ([], [], [])),
        "mhc2_metadata_only": Mhc2(None, ([], [], [])),
        "mhc2_lut_4096": Mhc2(IDENTITY, LUT_4096),
    }
    out = []
    for pid, d in (
        ("ordinary_srgb", Display("bt709", SRGB_TRC, None)),
        ("ordinary_p3_gamma22", Display("p3-d65", GAMMA22_TRC, None)),
    ):
        out.append((pid, "ordinary", d, profile_tags(pid, d, None), None, error("missing_tag")))
    for pid, m in mhc2.items():
        out.append((pid, "mhc2", srgb, profile_tags(pid, srgb, m.tag()[0]), None, m.manifest()))
    for pid, m in valid.items():
        out.append((pid, "adapter", srgb, profile_tags(pid, srgb, m.tag()[0]), None, m.manifest()))

    # Tag data first in the file, elements out of order with filler gaps.
    combined = mhc2["mhc2_combined"]
    relocated = combined.tag(order=("b", "r", "matrix", "g"), gap=b"\xa5" * 8)[0]
    order = ["MHC2", *[s for s in TAG_ORDER if s != "MHC2"]]
    out.append(("mhc2_relocated", "adapter", srgb, profile_tags("mhc2_relocated", srgb, relocated),
                order, combined.manifest()))

    out.append(("mhc2_lut_count_1", "adapter", srgb,
                profile_tags("mhc2_lut_count_1", srgb, Mhc2(MATRIX, ([Q(1, 2)],) * 3).tag()[0]),
                None, error("unsupported")))

    base = Mhc2(MATRIX, IDENTITY_LUT).tag()
    for pid in MALFORMED:
        out.append((pid, "adapter", srgb, profile_tags(pid, srgb, patched(base, pid)), None,
                    error("malformed_tag")))
    over = Mhc2(MATRIX, lut(4097, (lambda x: x,) * 3)).tag()[0]
    out.append(("mhc2_lut_count_4097", "adapter", srgb, profile_tags("mhc2_lut_count_4097", srgb, over),
                None, error("malformed_tag")))
    return out


def truncated_profile(full):
    """Header cut at 100 bytes; the size field still gives the full size."""
    return full[:100]


def build():
    """File name to bytes for the profiles, and the manifest object."""
    files, entries = {}, []
    for pid, use, display, tags, order, mhc2 in cases():
        files[f"{pid}.icc"] = icc.serialize(tags, order)
        entries.append((pid, use, display.manifest(), mhc2))
    files["icc_truncated.icc"] = truncated_profile(files["mhc2_identity.icc"])
    entries.append(("icc_truncated", "adapter", None, error("invalid_profile")))

    manifest = {"frame_metadata": FRAME_METADATA, "profiles": []}
    for pid, use, display, mhc2 in entries:
        data = files[f"{pid}.icc"]
        manifest["profiles"].append({
            "id": pid,
            "file": f"{pid}.icc",
            "use": use,
            "size": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
            "display": display,
            "mhc2": mhc2,
        })
    return files, manifest
