"""Expected values of the explicit HDR display model.

Independent of nitpick code and of colour libraries. Stage order, with
column vectors:
1. Decode the source signal to linear RGB in cd/m².
2. $XYZ = M_{src}\\,RGB$, $M$ the RP 177 normalized primary matrix at D65,
   without D50 adaptation.
3. $XYZ' = A\\,XYZ$, $A$ the left 3×3 of the stored MHC2 matrix.
4. $RGB_{wire} = M_{wire}^{-1}\\,XYZ'$.
5. Encode the wire transfer function and clip the signal to [0, 1]. PQ:
   luminance at or below 0 gives signal 0, above 10000 cd/m² gives 1.
   SDR: the value relative to the wire white, clipped to [0, 1].
6. Apply the stored MHC2 LUT of each channel: linear interpolation
   between uniformly spaced entries. Clamp the result to [0, 1].
7. Full-range code: $\\lfloor (2^n - 1)\\,v + 1/2 \\rfloor$.

The calibration uses the stored 15.16 values, decoded exactly. Matrices
are exact rationals; transfer functions use mpmath. Inputs are stored as
binary64, and each expectation is computed from that stored value.
"""

from fractions import Fraction as Q

import mpmath

import icc
import reference as ref

IDENTITY = tuple(tuple(Q(int(i == j)) for j in range(3)) for i in range(3))

# IEC 61966-2-1. The two thresholds are the published values; they do not
# map exactly to each other.
SRGB_DECODE_KNEE = Q("0.04045")
SRGB_ENCODE_KNEE = Q("0.0031308")
GAMMA22 = Q("2.2")

# A code within this distance of a rounding midpoint, or a wire component
# this small relative to the largest one of a non-black input, makes the
# generator stop: a binary64 implementation cannot decide the result.
TIE_MARGIN = mpmath.mpf("1e-6")
ZERO_MARGIN = mpmath.mpf("1e-9")


def stored(v):
    return Q(icc.s15f16(v), 65536)


class Calibration:
    """The XYZ matrix (3×3) and the channel LUTs, or None for identity."""

    def __init__(self, matrix, luts):
        self.matrix, self.luts = matrix, luts

    @classmethod
    def from_mhc2(cls, m):
        """The stored values of a `profiles.Mhc2`; the fourth column is ignored."""
        matrix = IDENTITY if m.matrix is None else tuple(tuple(stored(v) for v in row[:3]) for row in m.matrix)
        luts = tuple(tuple(stored(v) for v in lut) for lut in m.luts) if m.count else None
        return cls(matrix, luts)


def npm(space):
    """Linear RGB to XYZ: RGB (1, 1, 1) maps to D65 with Y = 1."""
    return ref.rgb_to_xyz(space)


def srgb_decode(s):
    s = ref.mpf(s)
    return s / mpmath.mpf("12.92") if s <= ref.mpf(SRGB_DECODE_KNEE) else ((s + mpmath.mpf("0.055")) / mpmath.mpf("1.055")) ** mpmath.mpf("2.4")


def srgb_encode(v):
    v = ref.mpf(v)
    return mpmath.mpf("12.92") * v if v <= ref.mpf(SRGB_ENCODE_KNEE) else mpmath.mpf("1.055") * v ** (1 / mpmath.mpf("2.4")) - mpmath.mpf("0.055")


SDR = {
    "srgb": (srgb_decode, srgb_encode),
    "gamma22": (lambda s: ref.mpf(s) ** ref.mpf(GAMMA22), lambda v: ref.mpf(v) ** (1 / ref.mpf(GAMMA22))),
}


def decode(encoding, s):
    """Source signal to cd/m²; exact for a linear source."""
    match encoding["kind"]:
        case "linear":
            return s * Q(encoding["nits_per_unit"])
        case "pq":
            if not 0 <= s <= 1:
                raise ValueError(f"PQ signal {s}")
            return ref.pq_eotf(s)
        case "sdr":
            if not 0 <= s <= 1:
                raise ValueError(f"SDR signal {s}")
            return SDR[encoding["transfer"]][0](s) * ref.mpf(Q(encoding["white_nits"]))
    raise ValueError(encoding)


def encode(encoding, nits):
    """Wire cd/m² to the clipped signal and its clip state."""
    match encoding["kind"]:
        case "pq":
            if nits <= 0:
                return mpmath.mpf(0), "low" if nits < 0 else "none"
            if nits > 10000:
                return mpmath.mpf(1), "high"
            return ref.pq_inverse_eotf(nits), "none"
        case "sdr":
            v = ref.mpf(nits) / ref.mpf(Q(encoding["white_nits"]))
            if v < 0:
                return mpmath.mpf(0), "low"
            if v > 1:
                return mpmath.mpf(1), "high"
            return SDR[encoding["transfer"]][1](v), "none"
    raise ValueError(encoding)


def lut_eval(lut, x):
    """Linear interpolation between uniformly spaced entries; `x` in [0, 1]."""
    n = len(lut)
    t = x * (n - 1)
    i = min(int(mpmath.floor(t)), n - 2)
    f = t - i
    return ref.mpf(lut[i]) + f * (ref.mpf(lut[i + 1]) - ref.mpf(lut[i]))


def clamp(v):
    if v < 0:
        return mpmath.mpf(0), "low"
    if v > 1:
        return mpmath.mpf(1), "high"
    return v, "none"


def mpf_mat(m):
    return tuple(tuple(ref.mpf(v) for v in row) for row in m)


def wire_matrix(cal, source, wire):
    """$M_{wire}^{-1} A M_{src}$, exact."""
    return ref.mat_mul(ref.mat_inv(npm(wire)), ref.mat_mul(cal.matrix, npm(source)))


def evaluate(cal, source, wire, depth, rgb, *, matrix=None, check=True):
    """Expected stages of one pixel. `matrix` replaces the joined matrix of
    `wire_matrix`, for the order and basis checks."""
    src_nits = tuple(decode(source["encoding"], Q(c)) for c in rgb)
    m = matrix or wire_matrix(cal, source["primaries"], wire["primaries"])
    # Exact for a linear source, so that a value at a clip boundary stays on it.
    exact = all(isinstance(v, Q) for v in src_nits)
    wire_nits = ref.mat_vec(m if exact else mpf_mat(m), src_nits)
    if check and any(c != 0 for c in rgb):
        top = max(abs(ref.mpf(w)) for w in wire_nits)
        for w in map(ref.mpf, wire_nits):
            if abs(w) <= top * ZERO_MARGIN:
                raise ValueError(f"{rgb}: wire component {w} is too close to 0 to decide its clipping")
    enc = [encode(wire["encoding"], w) for w in wire_nits]
    signal = [s for s, _ in enc]
    lut = [lut_eval(cal.luts[k], s) for k, s in enumerate(signal)] if cal.luts else signal
    out = [clamp(v) for v in lut]
    peak = ref.max_code(depth)
    codes = []
    for v, _ in out:
        x = v * peak
        if check and abs(x - mpmath.floor(x) - mpmath.mpf(1) / 2) < TIE_MARGIN:
            raise ValueError(f"{rgb}: code {x} is too close to a rounding midpoint")
        codes.append(ref.round_half_up(x))
    return {
        "source_nits": [ref.to_f64(v) for v in src_nits],
        "wire_nits": [ref.to_f64(v) for v in wire_nits],
        "signal": [ref.to_f64(v) for v in signal],
        "lut": [ref.to_f64(v) for v in lut],
        "output": [ref.to_f64(v) for v, _ in out],
        "code": codes,
        "clip": {"encode": [c for _, c in enc], "output": [c for _, c in out]},
    }


def space(primaries, kind, **params):
    return {"primaries": primaries, "encoding": {"kind": kind, **params}}


SCRGB = space("bt709", "linear", nits_per_unit=80.0)
PQ_2020 = space("bt2020", "pq")


def sdr(transfer, white, primaries="bt709"):
    return space(primaries, "sdr", transfer=transfer, white_nits=float(white))


def stored_input(v):
    return [ref.to_f64(c) for c in v]


def case(cid, calibration, source, wire, depth, inputs, *, calibrations):
    """`calibration` is a profile ID or `(matrix, luts)` with exact binary64 values."""
    if isinstance(calibration, str):
        cal = Calibration.from_mhc2(calibrations[calibration])
        ref_cal = {"profile": calibration}
    else:
        matrix, luts = calibration
        cal = Calibration(matrix, luts)
        for v in [*[c for row in matrix for c in row], *[c for lut in luts or () for c in lut]]:
            if Q(float(v)) != v:
                raise ValueError(f"{v} is not a binary64 value")
        ref_cal = {"matrix": [[float(c) for c in row] for row in matrix],
                   "lut": None if luts is None else [[float(c) for c in lut] for lut in luts]}
    stored_in = [stored_input(v) for v in inputs]
    return {
        "id": cid,
        "calibration": ref_cal,
        "source": source,
        "wire": {**wire, "depth": depth},
        "input": stored_in,
        "expected": [evaluate(cal, source, wire, depth, v) for v in stored_in],
    }


def neutral(*values):
    return [[Q(v)] * 3 for v in values]


# scRGB: black, 1 cd/m², 80, 203, 1000, the PQ peak (boundary), above the
# peak; the BT.709 primaries; colours inside BT.709 and outside BT.2020.
SCRGB_IN = (
    neutral("0", "0.0125", "1", "2.5375", "12.5", "125", "150")
    + [[1, 0, 0], [0, 1, 0], [0, 0, 1]]
    + [[Q("0.2"), Q("0.5"), Q("0.8")], [Q("6"), Q("1.5"), Q("0.25")], [-1, Q("1.5"), Q("-0.2")]]
)
# PQ signals at the knots k/255 of the 256-entry LUTs (k = 0, 1, 64, 128,
# 254, 255), at midpoints between knots and at one interior value.
LUT_256_IN = (
    neutral(*[Q(k, 255) for k in (0, 1, 64, 128, 254, 255)])
    + neutral(*[Q(2 * k + 1, 510) for k in (0, 100, 254)])
    + [[Q("0.3"), Q("0.6"), Q("0.9")]]
)
# Knots of the 3-entry LUTs (0, 1/2, 1) and their midpoints.
LUT_3_IN = neutral("0", "0.25", "0.5", "0.75", "1") + [[Q("0.3"), Q("0.6"), Q("0.9")]]
LUT_4096_IN = neutral(0, Q(1, 4095), Q(1000, 4095), Q(2001, 8190), 1)
# Linear values relative to SDR white near black: on and above the linear
# segment of the sRGB encoding (below 0.0031308), then mid and high
# values, and above white.
SDR_IN = (
    neutral("0", "0.0002", "0.001", "0.003", "0.0032", "0.01", "0.18", "0.5", "1", "1.25")
    + [[Q("0.2"), Q("0.5"), Q("0.8")], [Q("1.1"), Q("0.5"), Q("-0.05")]]
)
# SDR source signals: near black on the linear segment of the sRGB
# decoding (below 0.04045), mid values, white and the BT.709 primaries.
SDR_SOURCE_IN = neutral("0", "0.01", "0.02", "0.2", "0.5", "1") + [[1, 0, 0], [0, 1, 0], [0, 0, 1]]
SWAP_IN = [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 1], [Q("0.2"), Q("0.5"), Q("0.8")]]

HALF = Q(1, 2)
GAIN = (tuple(tuple(HALF if i == j else Q(0) for j in range(3)) for i in range(3)), None)


def cases(calibrations):
    """Model cases in manifest order. `calibrations` maps profile IDs to `profiles.Mhc2`."""
    c = lambda *a: case(*a, calibrations=calibrations)  # noqa: E731
    hdr10 = PQ_2020
    return [
        c("pq_identity", "mhc2_identity", SCRGB, hdr10, 10, SCRGB_IN),
        c("pq_xyz_gain", GAIN, SCRGB, hdr10, 10, SCRGB_IN),
        c("pq_matrix", "mhc2_matrix", SCRGB, hdr10, 10, SCRGB_IN),
        c("pq_lut", "mhc2_lut", PQ_2020, hdr10, 10, LUT_256_IN),
        c("pq_combined", "mhc2_combined", SCRGB, hdr10, 12, SCRGB_IN),
        c("pq_signed", "mhc2_signed", PQ_2020, hdr10, 10, LUT_3_IN),
        c("pq_lut_4096", "mhc2_lut_4096", PQ_2020, hdr10, 12, LUT_4096_IN),
        c("sdr_srgb_identity", "mhc2_identity", SCRGB, sdr("srgb", 80), 12, SDR_IN),
        c("sdr_gamma22_identity", "mhc2_identity", SCRGB, sdr("gamma22", 80), 12, SDR_IN),
        c("sdr_srgb_white_200", "mhc2_identity", SCRGB, sdr("srgb", 200), 12, SDR_IN),
        c("sdr_srgb_combined", "mhc2_combined", sdr("srgb", 80), sdr("srgb", 80), 8, SDR_SOURCE_IN),
        c("sdr_srgb_swap_rg", "mhc2_swap_rg", sdr("srgb", 80), sdr("srgb", 80), 8, SWAP_IN),
        c("srgb_source_to_pq", "mhc2_identity", sdr("srgb", 203), hdr10, 10, SDR_SOURCE_IN),
        c("gamma22_source_to_pq", "mhc2_identity", sdr("gamma22", 203), hdr10, 10, SDR_SOURCE_IN),
    ]
