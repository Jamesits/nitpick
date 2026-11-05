"""Case specifications, validation and expected results."""

import math

import reference as ref

# Error kinds of invalid input. The Rust fixture reader uses the same names.
ERRORS = {
    "unknown_encoding": "encoding is not pq-full, pq-narrow or hlg-narrow",
    "unsupported_depth": "depth is not 10 or 12",
    "empty_image": "width or height is zero",
    "code_out_of_range": "a code is above 2^depth - 1 or negative",
    "layout_mismatch": "layout rows do not match the size, or a palette index is missing",
    "rect_out_of_bounds": "a rectangle is empty or outside the image",
    "invalid_metadata": "metadata values are outside their domain",
}


class FixtureError(Exception):
    def __init__(self, kind, detail):
        assert kind in ERRORS, kind
        super().__init__(f"{kind}: {detail}")
        self.kind = kind


def _finite(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def validate(spec):
    """Check one case input. Raise FixtureError with the first failed rule."""
    enc = spec["format"]["encoding"]
    depth = spec["format"]["depth"]
    if enc not in ref.ENCODINGS:
        raise FixtureError("unknown_encoding", enc)
    if depth not in ref.DEPTHS:
        raise FixtureError("unsupported_depth", depth)
    w, h = spec["size"]
    if w < 1 or h < 1:
        raise FixtureError("empty_image", spec["size"])
    for entry in spec["palette"]:
        for c in entry["codes"]:
            if not 0 <= c <= ref.max_code(depth):
                raise FixtureError("code_out_of_range", entry["codes"])
    n = len(spec["palette"])
    layout = spec["layout"]
    if layout["kind"] == "rects":
        if not 0 <= layout["background"] < n:
            raise FixtureError("layout_mismatch", "background index")
        for x, y, rw, rh, idx in layout["rects"]:
            if rw < 1 or rh < 1 or x < 0 or y < 0 or x + rw > w or y + rh > h:
                raise FixtureError("rect_out_of_bounds", [x, y, rw, rh])
            if not 0 <= idx < n:
                raise FixtureError("layout_mismatch", "rect index")
    else:
        rows = layout["rows"]
        if len(rows) != h or any(len(r) != w for r in rows):
            raise FixtureError("layout_mismatch", "row shape")
        if any(not 0 <= i < n for r in rows for i in r):
            raise FixtureError("layout_mismatch", "row index")
    md = spec.get("metadata")
    if md is not None:
        coords = [v for p in md["primaries"] for v in p] + list(md["white"])
        if not all(_finite(v) and 0 < v < 1 for v in coords):
            raise FixtureError("invalid_metadata", "chromaticity")
        lo, hi = md["min_mastering_nits"], md["max_mastering_nits"]
        if not (_finite(lo) and _finite(hi) and 0 <= lo < hi):
            raise FixtureError("invalid_metadata", "mastering luminance")
        ll = md["light_level"]
        if ll["kind"] == "explicit":
            cll, fall = ll["max_cll"], ll["max_fall"]
            if not (_finite(cll) and _finite(fall) and 0 <= fall <= cll):
                raise FixtureError("invalid_metadata", "light level")
        elif ll["kind"] != "from_frame":
            raise FixtureError("invalid_metadata", "light level kind")


def raster(spec):
    """Palette index of every pixel, row-major."""
    w, h = spec["size"]
    layout = spec["layout"]
    if layout["kind"] == "rows":
        return [i for r in layout["rows"] for i in r]
    px = [layout["background"]] * (w * h)
    for x, y, rw, rh, idx in layout["rects"]:
        for yy in range(y, y + rh):
            px[yy * w + x : yy * w + x + rw] = [idx] * rw
    return px


def resolve_metadata(md, level):
    if md is None:
        return None
    if level is None and md["light_level"]["kind"] == "from_frame":
        return {"error": "unsupported"}
    ll = md["light_level"]
    cll, fall = (
        (level["max_cll"], level["max_fall"]) if ll["kind"] == "from_frame" else (ll["max_cll"], ll["max_fall"])
    )
    return {
        "primaries": md["primaries"],
        "white": md["white"],
        "max_mastering_nits": md["max_mastering_nits"],
        "min_mastering_nits": md["min_mastering_nits"],
        "max_cll": cll,
        "max_fall": fall,
    }


def expect(spec):
    """Expected results for each boundary of a valid case."""
    enc = spec["format"]["encoding"]
    depth = spec["format"]["depth"]
    pixels = raster(spec)
    counts = [0] * len(spec["palette"])
    for i in pixels:
        counts[i] += 1
    palette, q_nits, b_nits = [], [], []
    for entry in spec["palette"]:
        q, qn = ref.qrhi_expect(entry["codes"], enc, depth)
        b, bn = ref.browser_expect(entry["codes"], enc, depth)
        palette.append({"qrhi": q, "browser": b})
        q_nits.append(qn)
        b_nits.append(bn)
    q_level = ref.light_level(q_nits, counts)
    b_level = ref.light_level(b_nits, counts) if ref.is_pq(enc) else None
    return {
        "pixel_counts": counts,
        "palette": palette,
        "light_level": {"qrhi": q_level, "browser": b_level or {"error": "unsupported"}},
        "metadata": {
            "qrhi": resolve_metadata(spec.get("metadata"), q_level),
            "browser": resolve_metadata(spec.get("metadata"), b_level),
        }
        if spec.get("metadata") is not None
        else None,
    }


def requires(spec):
    md = spec.get("metadata")
    return {
        "depth": spec["format"]["depth"],
        "encoding": spec["format"]["encoding"],
        "hdr_metadata": md is not None,
        "auto_light_level": md is not None and md["light_level"]["kind"] == "from_frame",
    }


def intended_nominal(codes, encoding, depth):
    """Luminance after code quantization under the codes' own encoding (PQ only)."""
    if not ref.is_pq(encoding):
        return None
    nits = ref.nominal_nits(codes, encoding, depth)
    return {
        "nits_2020": [ref.to_f64(n) for n in nits],
        "xyz": [ref.to_f64(v) for v in ref.mat_vec(_mpf_matrix(ref.rgb_to_xyz("bt2020")), nits)],
    }


def _mpf_matrix(m):
    return tuple(tuple(ref.mpf(c) for c in row) for row in m)


def fmt_id(encoding, depth):
    return f"{encoding}-{depth}"

