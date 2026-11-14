"""ICC profile serialization for the profile fixtures.

Independent of Little CMS and of nitpick code. Field layouts follow
ICC.1:2010 (profile version 4.3) and the Microsoft MHC2 tag definition.
All multi-byte fields are big-endian.
"""

import hashlib
import struct
from fractions import Fraction as Q

import reference as ref

VERSION = 0x04300000
HEADER_LEN = 128
# ICC.1:2010 7.2.16: the PCS illuminant, exact decimal values.
D50 = (Q("0.9642"), Q(1), Q("0.8249"))
# Fixed creation date keeps the bytes reproducible.
DATE = (2026, 1, 1, 0, 0, 0)

S15F16_MIN = -(1 << 31)
S15F16_MAX = (1 << 31) - 1


def s15f16(v):
    """Signed 15.16 value of `v`: round(v * 65536), exact halves go up.

    `v` is an int, a Fraction or a float (taken as its exact binary value).
    """
    n = ref.round_half_up(Q(v) * 65536)
    if not S15F16_MIN <= n <= S15F16_MAX:
        raise ValueError(f"{v} outside the s15Fixed16 range")
    return n


def pack_s15f16(values):
    return b"".join(struct.pack(">i", s15f16(v)) for v in values)


def pad4(b):
    return b + b"\0" * (-len(b) % 4)


def xyz_type(xyz):
    return b"XYZ \0\0\0\0" + pack_s15f16(xyz)


def sf32_type(values):
    return b"sf32\0\0\0\0" + pack_s15f16(values)


def para_type(function, params):
    """ICC.1:2010 10.16: parametricCurveType. Function 0 has 1 and function 3 has 5 parameters."""
    if len(params) != {0: 1, 3: 5}[function]:
        raise ValueError(f"function {function} with {len(params)} parameters")
    return b"para\0\0\0\0" + struct.pack(">HH", function, 0) + pack_s15f16(params)


def mluc_type(text):
    """ICC.1:2010 10.13: one en-US record."""
    s = text.encode("utf-16-be")
    return b"mluc\0\0\0\0" + struct.pack(">II2s2sII", 1, 12, b"en", b"US", len(s), 28) + s


def mhc2_type(count, min_nits, peak_nits, matrix, luts, *, order=("matrix", "r", "g", "b"), gap=b""):
    """MHC2 tag data and the position of each element relative to the tag start.

    `matrix` is 3 rows of 4 values, or None for offset 0. `luts` is 3 lists
    of `count` values; with count 0 the LUT offsets are 0. `order` places
    the elements after the 36-byte header; `gap` is inserted before each.
    """
    body = {}
    if matrix is not None:
        body["matrix"] = pack_s15f16([v for row in matrix for v in row])
    for ch, values in zip("rgb", luts):
        if len(values) != count:
            raise ValueError("LUT length differs from the count")
        if count:
            body[ch] = sf32_type(values)
    pos, out = {}, bytearray(36)
    for name in order:
        if name in body:
            out += gap
            pos[name] = len(out)
            out += body[name]
    struct.pack_into(
        ">4sIIiiIIII", out, 0, b"MHC2", 0, count, s15f16(min_nits), s15f16(peak_nits),
        pos.get("matrix", 0), pos.get("r", 0), pos.get("g", 0), pos.get("b", 0),
    )
    return bytes(out), pos


# Field positions in the MHC2 header.
MHC2_COUNT = 8
MHC2_MATRIX_OFFSET = 20
MHC2_LUT_OFFSET = {"r": 24, "g": 28, "b": 32}


def header(size, profile_id=b"\0" * 16):
    """Display class, RGB data, XYZ PCS. CMM, platform, flags, device and
    creator fields stay zero (unspecified); the rendering intent is perceptual."""
    h = bytearray(HEADER_LEN)
    struct.pack_into(">II", h, 0, size, 0)
    struct.pack_into(">I", h, 8, VERSION)
    h[12:24] = b"mntrRGB XYZ "
    struct.pack_into(">6H", h, 24, *DATE)
    h[36:40] = b"acsp"
    h[68:80] = pack_s15f16(D50)
    h[84:100] = profile_id
    return h


def profile_id(data):
    """ICC.1:2010 7.2.18: MD5 with the flags, rendering intent and ID fields zeroed."""
    b = bytearray(data)
    b[44:48] = b"\0" * 4
    b[64:68] = b"\0" * 4
    b[84:100] = b"\0" * 16
    return hashlib.md5(b).digest()


def serialize(tags, data_order=None):
    """Profile bytes from `(signature, data)` pairs in tag-table order.

    `data` is bytes, or a function of the tag's file offset that returns
    bytes of a length that does not depend on that offset. `data_order`
    lists the signatures in file order; the default is the table order.
    """
    sigs = [s for s, _ in tags]
    if len(set(sigs)) != len(sigs):
        raise ValueError("duplicate tag signature")
    data = dict(tags)
    order = data_order or sigs
    if sorted(order) != sorted(sigs):
        raise ValueError("data order does not list every tag")
    pos = HEADER_LEN + 4 + 12 * len(tags)
    placed = {}
    for s in order:
        d = data[s]
        b = d(pos) if callable(d) else d
        if callable(d) and len(d(pos + 4)) != len(b):
            raise ValueError(f"{s}: length depends on the offset")
        placed[s] = (pos, b)
        pos += len(pad4(b))
    table = struct.pack(">I", len(tags)) + b"".join(
        struct.pack(">4sII", s.encode("ascii"), placed[s][0], len(placed[s][1])) for s in sigs
    )
    body = b"".join(pad4(placed[s][1]) for s in order)
    out = header(pos) + table + body
    return bytes(header(pos, profile_id(out)) + table + body)


def tag_offset(tags, sig, data_order=None):
    """File offset at which `serialize` places tag `sig`."""
    data = dict(tags)
    pos = HEADER_LEN + 4 + 12 * len(tags)
    for s in data_order or [t for t, _ in tags]:
        if s == sig:
            return pos
        d = data[s]
        pos += len(pad4(d(pos) if callable(d) else d))
    raise KeyError(sig)
