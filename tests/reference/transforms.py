"""Expected values of conventional ICC transforms between the profile
fixtures and the D50 PCS XYZ endpoint.

Independent of Little CMS. Each expectation uses the stored profile values:
the signed 15.16 integers of the colorants, `chad` and curve parameters,
decoded exactly. Thus tag rounding is not part of the transform error.
Matrices are exact rationals; curves use mpmath. Inputs are stored as
binary64, and the expectation is computed from that stored value.

Three-component matrix-based model of an ICC.1:2010 display profile:
- device to PCS: $XYZ_{PCS} = M\\,TRC(RGB)$, $M$ the colorant columns;
- PCS to device: $TRC^{-1}(M^{-1} XYZ_{PCS})$.
All intents use this model. With the absolute colorimetric intent, the
adaptation state selects how far the `chad` adaptation is undone:
1 keeps the PCS values, 0 maps through the native XYZ of the source,
$XYZ_{PCS,out} = C_{out}\\,C_{in}^{-1} XYZ_{PCS,in}$. The generator supports
only the states 0 and 1, and only profiles with the same stored `wtpt`; it
fails for other cases. Black point compensation has no effect, because
every fixture curve gives 0 for input 0.
"""

from fractions import Fraction as Q

import icc
import reference as ref

PCS = None
INTENTS = ("perceptual", "relative_colorimetric", "saturation", "absolute_colorimetric")


def stored(v):
    return Q(icc.s15f16(v), 65536)


class Stored:
    """The stored colour tags of a display profile."""

    def __init__(self, display):
        self.m = tuple(tuple(stored(display.adapted[j][i]) for j in range(3)) for i in range(3))
        self.chad = tuple(tuple(stored(v) for v in row) for row in display.chad)
        self.wtpt = tuple(stored(v) for v in icc.D50)
        f, params = display.trc
        self.function, self.params = f, tuple(ref.mpf(stored(p)) for p in params)

    def decode(self, x):
        """ICC.1:2010 10.18 parametric curve, device value to linear."""
        x = ref.mpf(x)
        match self.function:
            case 0:
                (g,) = self.params
                if x < 0:
                    raise ValueError(f"gamma curve at {x}")
                return x ** g
            case 3:
                g, a, b, c, d = self.params
                return (a * x + b) ** g if x >= d else c * x
        raise ValueError(f"function {self.function}")

    def encode(self, y):
        """Inverse of `decode`. Fails where the stored curve has no inverse:
        the stored function 3 parameters leave a small gap at `d`."""
        match self.function:
            case 0:
                (g,) = self.params
                if y < 0:
                    raise ValueError(f"gamma curve inverse at {y}")
                return y ** (1 / g)
            case 3:
                g, a, b, c, d = self.params
                if y >= (a * d + b) ** g:
                    return (y ** (1 / g) - b) / a
                x = y / c
                if x >= d:
                    raise ValueError(f"{y} is in the gap of the stored curve")
                return x
        raise ValueError(f"function {self.function}")


def mpf_mat(m):
    return tuple(tuple(ref.mpf(v) for v in row) for row in m)


IDENTITY = tuple(tuple(Q(int(i == j)) for j in range(3)) for i in range(3))


def linear(end, v):
    return tuple(ref.mpf(c) for c in v) if end is PCS else tuple(end.decode(c) for c in v)


def to_pcs(end, v):
    m = IDENTITY if end is PCS else end.m
    return ref.mat_vec(mpf_mat(m), linear(end, v))


def evaluate(src, dst, intent, adaptation, v):
    """The matrices are joined exactly before use, so an identity chain
    returns its input without mpmath rounding."""
    m = IDENTITY if src is PCS else src.m
    if intent == "absolute_colorimetric":
        if PCS in (src, dst) or src.wtpt != dst.wtpt:
            raise ValueError("absolute intent needs two profiles with equal wtpt")
        if adaptation == 0:
            m = ref.mat_mul(ref.mat_mul(dst.chad, ref.mat_inv(src.chad)), m)
        elif adaptation != 1:
            raise ValueError(f"adaptation state {adaptation}")
    if dst is not PCS:
        m = ref.mat_mul(ref.mat_inv(dst.m), m)
    out = ref.mat_vec(mpf_mat(m), linear(src, v))
    return out if dst is PCS else tuple(dst.encode(c) for c in out)


def f64(v):
    return [ref.to_f64(c) for c in v]


def case(cid, src, dst, inputs, *, displays, intent="relative_colorimetric", bpc=False, adaptation=1):
    """One transform case. `src` and `dst` are profile IDs or `PCS`."""
    if intent not in INTENTS:
        raise ValueError(intent)
    ends = [PCS if e is PCS else Stored(displays[e]) for e in (src, dst)]
    stored_in = [f64(v) for v in inputs]
    expected = [f64(evaluate(*ends, intent, adaptation, [Q(c) for c in v])) for v in stored_in]
    return {
        "id": cid,
        "source": src,
        "target": dst,
        "intent": intent,
        "black_point_compensation": bpc,
        "adaptation_state": float(adaptation),
        "input": stored_in,
        "expected": expected,
    }


# Device values: black, white, near black on the linear sRGB segment
# (below 0.04045), mid grey, primaries and mixed colours. No value is
# near a curve knot.
NEUTRALS = [[Q(v)] * 3 for v in ("0", "0.02", "0.18", "0.5", "1")]
PRIMARIES = [[1, 0, 0], [0, 1, 0], [0, 0, 1]]
MIXED = [[Q("0.2"), Q("0.5"), Q("0.8")], [Q("0.03"), Q("0.6"), Q("0.95")], [Q("0.9"), Q("0.1"), Q("0.4")]]
DEVICE = NEUTRALS + PRIMARIES + MIXED
# Into a gamma 2.2 target, only values that give positive linear
# components: sRGB blue has the P3 blue chromaticity, so its other P3
# components are near zero, and tag rounding sets their sign. Primaries
# into `display_d50` have the same problem.
INTO_P3 = NEUTRALS + PRIMARIES[:2] + MIXED
INTERIOR = NEUTRALS + MIXED

# PCS values inside the sRGB and P3 gamuts: the PCS illuminant, 18 %
# of it, and two colours.
XYZ = [
    list(icc.D50),
    [Q("0.18") * c for c in icc.D50],
    [Q("0.3"), Q("0.25"), Q("0.15")],
    [Q("0.2"), Q("0.3"), Q("0.25")],
]


def cases(displays):
    """Transform cases in manifest order."""
    srgb, p3, d50 = "ordinary_srgb", "ordinary_p3_gamma22", "display_d50"
    c = lambda *a, **k: case(*a, **k, displays=displays)  # noqa: E731
    return [
        c("srgb_identity", srgb, srgb, DEVICE),
        c("srgb_to_pcs", srgb, PCS, DEVICE),
        c("p3_gamma22_to_pcs", p3, PCS, DEVICE),
        c("pcs_to_srgb", PCS, srgb, XYZ),
        c("pcs_to_p3_gamma22", PCS, p3, XYZ),
        c("srgb_to_p3_gamma22", srgb, p3, INTO_P3),
        c("srgb_to_p3_gamma22_perceptual", srgb, p3, INTO_P3, intent="perceptual"),
        c("srgb_to_p3_gamma22_bpc", srgb, p3, INTO_P3, bpc=True),
        c("srgb_to_d50_relative", srgb, d50, INTERIOR),
        c("srgb_to_d50_relative_unadapted", srgb, d50, INTERIOR, adaptation=0),
        c("srgb_to_d50_absolute_adapted", srgb, d50, INTERIOR, intent="absolute_colorimetric"),
        c("srgb_to_d50_absolute_unadapted", srgb, d50, INTERIOR, intent="absolute_colorimetric", adaptation=0),
    ]
