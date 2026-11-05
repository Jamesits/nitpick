"""Independent reference arithmetic for the output fixtures.

Nothing here calls nitpick code or a colour library. Range mappings,
primary matrices and integer quantization use exact rationals. Transfer
functions use mpmath at `DIGITS` significant digits. Results are stored as
the nearest IEEE 754 binary64 value.
"""

from fractions import Fraction as Q

import mpmath

DIGITS = 50
mpmath.mp.dps = DIGITS

ENCODINGS = ("pq-full", "pq-narrow", "hlg-narrow")
DEPTHS = (10, 12)

# SMPTE ST 2084 constants, exact.
PQ_M1 = Q(2610, 16384)
PQ_M2 = Q(2523, 4096) * 128
PQ_C1 = Q(3424, 4096)
PQ_C2 = Q(2413, 4096) * 32
PQ_C3 = Q(2392, 4096) * 32

D65 = (Q("0.3127"), Q("0.3290"))
PRIMARIES = {
    "bt709": ((Q("0.64"), Q("0.33")), (Q("0.30"), Q("0.60")), (Q("0.15"), Q("0.06"))),
    "p3-d65": ((Q("0.680"), Q("0.320")), (Q("0.265"), Q("0.690")), (Q("0.150"), Q("0.060"))),
    "bt2020": ((Q("0.708"), Q("0.292")), (Q("0.170"), Q("0.797")), (Q("0.131"), Q("0.046"))),
}

SCRGB_WHITE_NITS = 80
PNG_MAX = 65535


def mpf(v):
    if isinstance(v, Q):
        return mpmath.mpf(v.numerator) / v.denominator
    return mpmath.mpf(v)


def to_f64(v):
    """Nearest binary64 value; exact zero stays positive zero."""
    return float(mpf(v))


def round_half_up(v):
    """Round to the nearest integer; exact halves go up. `v` is a Fraction or mpf."""
    if isinstance(v, Q):
        return (v + Q(1, 2)).__floor__()
    return int(mpmath.floor(v + mpmath.mpf(1) / 2))


def max_code(depth):
    return (1 << depth) - 1


def is_narrow(encoding):
    return encoding in ("pq-narrow", "hlg-narrow")


def is_pq(encoding):
    return encoding in ("pq-full", "pq-narrow")


def black(encoding, depth):
    return 64 << (depth - 10) if is_narrow(encoding) else 0


def nominal_peak(encoding, depth):
    return 940 << (depth - 10) if is_narrow(encoding) else max_code(depth)


def signal_from_code(code, encoding, depth):
    """BT.2100 Table 9 inverse: E' of a code, exact, without clipping."""
    if is_narrow(encoding):
        s = 1 << (depth - 8)
        return Q(code - 16 * s, 219 * s)
    return Q(code, max_code(depth))


def code_from_signal(signal, encoding, depth):
    """BT.2100 Table 9: code of E' in [0, 1], rounded half up."""
    if is_narrow(encoding):
        s = 1 << (depth - 8)
        return round_half_up((219 * mpf(signal) + 16) * s)
    return round_half_up(max_code(depth) * mpf(signal))


def pq_eotf(signal):
    """ST 2084 EOTF: E' to cd/m². E' is clamped to [0, 1]."""
    e = min(max(mpf(signal), mpmath.mpf(0)), mpmath.mpf(1))
    if e == 0:
        return mpmath.mpf(0)
    p = e ** (1 / mpf(PQ_M2))
    num = max(p - mpf(PQ_C1), mpmath.mpf(0))
    return 10000 * (num / (mpf(PQ_C2) - mpf(PQ_C3) * p)) ** (1 / mpf(PQ_M1))


def pq_inverse_eotf(nits):
    """ST 2084 inverse EOTF: cd/m² in [0, 10000] to E'."""
    y = mpf(nits) / 10000
    if y < 0 or y > 1:
        raise ValueError(f"luminance {nits} outside the PQ range")
    yp = y ** mpf(PQ_M1)
    return ((mpf(PQ_C1) + mpf(PQ_C2) * yp) / (1 + mpf(PQ_C3) * yp)) ** mpf(PQ_M2)


# 3×3 matrices as tuples of rows; column vectors.


def mat_vec(m, v):
    return tuple(sum(m[i][k] * v[k] for k in range(3)) for i in range(3))


def mat_mul(a, b):
    return tuple(tuple(sum(a[i][k] * b[k][j] for k in range(3)) for j in range(3)) for i in range(3))


def mat_inv(m):
    def cof(i, j):
        r0, r1 = [r for r in range(3) if r != i]
        c0, c1 = [c for c in range(3) if c != j]
        return (-1) ** (i + j) * (m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0])

    det = sum(m[0][j] * cof(0, j) for j in range(3))
    if det == 0:
        raise ZeroDivisionError("singular matrix")
    return tuple(tuple(cof(j, i) / det for j in range(3)) for i in range(3))


def xyz_of_xy(xy, big_y=Q(1)):
    x, y = xy
    return (big_y * x / y, big_y, big_y * (1 - x - y) / y)


def rgb_to_xyz(space):
    """Normalized primary matrix (SMPTE RP 177): RGB (1, 1, 1) maps to D65 with Y = 1."""
    cols = [xyz_of_xy(p) for p in PRIMARIES[space]]
    p = tuple(tuple(cols[j][i] for j in range(3)) for i in range(3))
    s = mat_vec(mat_inv(p), xyz_of_xy(D65))
    return tuple(tuple(p[i][j] * s[j] for j in range(3)) for i in range(3))


def rgb_to_rgb(src, dst):
    return mat_mul(mat_inv(rgb_to_xyz(dst)), rgb_to_xyz(src))


M_2020_TO_709 = rgb_to_rgb("bt2020", "bt709")


def scrgb_from_nits_2020(nits):
    """Linear BT.2020 cd/m² to scRGB: BT.709 primaries, 1.0 = 80 cd/m²."""
    m = tuple(tuple(mpf(c) for c in row) for row in M_2020_TO_709)
    return tuple(v / SCRGB_WHITE_NITS for v in mat_vec(m, tuple(mpf(n) for n in nits)))


def r10g10b10a2(codes):
    r, g, b = codes
    return (3 << 30) | (b << 20) | (g << 10) | r


def png_sample(code, encoding, depth):
    """Browser contract: full-range 16-bit sample and its clip state."""
    e = signal_from_code(code, encoding, depth)
    clip = "below" if e < 0 else "above" if e > 1 else "none"
    return round_half_up(PNG_MAX * min(max(e, Q(0)), Q(1))), clip


def qrhi_expect(codes, encoding, depth):
    """QRhi contract: every encoding is sent as full-range PQ codes."""
    signal = [Q(c, max_code(depth)) for c in codes]
    nits = [pq_eotf(s) for s in signal]
    out = {
        "signal": [to_f64(s) for s in signal],
        "nits_2020": [to_f64(n) for n in nits],
        "scrgb": [to_f64(v) for v in scrgb_from_nits_2020(nits)],
        "r10g10b10a2": r10g10b10a2(codes) if depth == 10 else None,
    }
    return out, nits


def browser_expect(codes, encoding, depth):
    samples = [png_sample(c, encoding, depth) for c in codes]
    out = {
        "sample": [v for v, _ in samples],
        "clip": [c for _, c in samples],
        "nits_2020": None,
        "scrgb": None,
    }
    nits = None
    if is_pq(encoding):
        nits = [pq_eotf(Q(v, PNG_MAX)) for v, _ in samples]
        out["nits_2020"] = [to_f64(n) for n in nits]
        out["scrgb"] = [to_f64(v) for v in scrgb_from_nits_2020(nits)]
    return out, nits


def nominal_nits(codes, encoding, depth):
    """Luminance of the codes under their own encoding (PQ only), E' clipped to [0, 1]."""
    return [pq_eotf(signal_from_code(c, encoding, depth)) for c in codes]


def light_level(pixel_nits, counts):
    """CTA-861.3 MaxCLL and MaxFALL from per-palette channel luminance and pixel counts."""
    total = sum(counts)
    peaks = [max(n) for n in pixel_nits]
    max_cll = max(p for p, c in zip(peaks, counts) if c > 0)
    max_fall = sum(p * c for p, c in zip(peaks, counts)) / total
    return {"max_cll": to_f64(max_cll), "max_fall": to_f64(max_fall)}
