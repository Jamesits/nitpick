"""Case definitions for each fixture group.

Each builder returns case inputs without expected results. `generate.py`
validates them and adds the expectations.
"""

from fractions import Fraction as Q

import reference as ref
import spec

PQ_ENCODINGS = ("pq-full", "pq-narrow")

# ITU-R BT.2111-3 (05/2025) Tables 2, 3 and 4, transcribed from the
# publication. The 12-bit columns are copied from the tables, not derived
# from the 10-bit values. Orders: bars "100", "75" and "58" are the (high,
# low) codes of white, yellow, cyan, green, magenta, red and blue; "steps"
# are -7 % (narrow only), 0 %, 10 % ... 100 %, 109 % (narrow only); "bt709"
# are yellow, cyan, green, magenta, red and blue; "pluge" are 0 %, -2 %
# (narrow only), +2 % and +4 %.
BT2111 = {
    ("hlg-narrow", 10): {
        "100": (940, 64), "75": (721, 64), "grey40": 414,
        "steps": [4, 64, 152, 239, 327, 414,
                  502, 590, 677, 765, 852,
                  940, 1019],
        "bt709": [(713, 719, 316), (538, 709, 718), (512, 706, 296),
                  (651, 286, 705), (639, 269, 164), (227, 147, 702)],
        "pluge": [64, 48, 80, 99],
    },
    ("hlg-narrow", 12): {
        "100": (3760, 256), "75": (2884, 256), "grey40": 1656,
        "steps": [16, 256, 608, 956, 1308, 1656,
                  2008, 2360, 2708, 3060, 3408,
                  3760, 4076],
        "bt709": [(2852, 2876, 1264), (2152, 2836, 2872), (2048, 2824, 1184),
                  (2604, 1144, 2820), (2556, 1076, 656), (908, 588, 2808)],
        "pluge": [256, 192, 320, 396],
    },
    ("pq-narrow", 10): {
        "100": (940, 64), "58": (573, 64), "grey40": 414,
        "steps": [4, 64, 152, 239, 327, 414,
                  502, 590, 677, 765, 852,
                  940, 1019],
        "bt709": [(569, 572, 381), (485, 566, 571), (474, 565, 368),
                  (537, 362, 564), (531, 351, 257), (318, 236, 563)],
        "pluge": [64, 48, 80, 99],
    },
    ("pq-narrow", 12): {
        "100": (3760, 256), "58": (2292, 256), "grey40": 1656,
        "steps": [16, 256, 608, 956, 1308, 1656,
                  2008, 2360, 2708, 3060, 3408,
                  3760, 4076],
        "bt709": [(2276, 2288, 1524), (1940, 2264, 2284), (1896, 2260, 1472),
                  (2148, 1448, 2256), (2124, 1404, 1028), (1272, 944, 2252)],
        "pluge": [256, 192, 320, 396],
    },
    ("pq-full", 10): {
        "100": (1023, 0), "58": (594, 0), "grey40": 409,
        "steps": [0, 102, 205, 307, 409, 512,
                  614, 716, 818, 921, 1023],
        "bt709": [(589, 593, 370), (491, 586, 592), (479, 585, 355),
                  (552, 348, 584), (545, 335, 225), (296, 201, 582)],
        "pluge": [0, 19, 41],
    },
    ("pq-full", 12): {
        "100": (4095, 0), "58": (2378, 0), "grey40": 1638,
        "steps": [0, 410, 819, 1229, 1638, 2048,
                  2457, 2867, 3276, 3686, 4095],
        "bt709": [(2359, 2373, 1483), (1967, 2348, 2371), (1918, 2342, 1423),
                  (2209, 1391, 2339), (2181, 1339, 901), (1186, 806, 2331)],
        "pluge": [0, 75, 164],
    },
}


def bars(hi, lo):
    """White, yellow, cyan, green, magenta, red, blue."""
    return [(hi, hi, hi), (hi, hi, lo), (lo, hi, hi), (lo, hi, lo), (hi, lo, hi), (hi, lo, lo), (lo, lo, hi)]


def entry(codes, intended=None):
    e = {"codes": list(codes)}
    if intended is not None:
        e["intended"] = intended
    return e


def patch_grid(palette, cols=16, cell=6, patch=4):
    """Rect layout: palette entry 0 is the background; each other entry is one patch."""
    n = len(palette) - 1
    cols = min(cols, n)
    rows = (n + cols - 1) // cols
    off = (cell - patch) // 2
    rects = [[(i % cols) * cell + off, (i // cols) * cell + off, patch, patch, i + 1] for i in range(n)]
    return [cols * cell, rows * cell], {"kind": "rects", "background": 0, "rects": rects}


def make(case_id, encoding, depth, palette, size=None, layout=None, metadata=None):
    if layout is None:
        size, layout = patch_grid(palette)
    return {
        "id": case_id,
        "format": {"encoding": encoding, "depth": depth},
        "size": size,
        "palette": palette,
        "layout": layout,
        "metadata": metadata,
    }


def bg(encoding, depth):
    return entry([ref.black(encoding, depth)] * 3)


# --- codes ---------------------------------------------------------------


def scalar_tables():
    """Every scalar code at 10 and 12 bits, as columns indexed by code.

    QRhi values depend on the depth only; browser values on encoding and depth.
    """
    qrhi, browser = [], []
    for depth in ref.DEPTHS:
        n = ref.max_code(depth) + 1
        signal, nits, scrgb = [], [], []
        for c in range(n):
            q, _ = ref.qrhi_expect((c, c, c), "pq-full", depth)
            signal.append(q["signal"][0])
            nits.append(q["nits_2020"][0])
            scrgb.append(q["scrgb"])
        table = {"depth": depth, "count": n, "signal": signal, "nits_2020": nits, "scrgb_grey": scrgb}
        if depth == 10:
            table["r10g10b10a2"] = {
                "r": [ref.r10g10b10a2((c, 0, 0)) for c in range(n)],
                "g": [ref.r10g10b10a2((0, c, 0)) for c in range(n)],
                "b": [ref.r10g10b10a2((0, 0, c)) for c in range(n)],
            }
        qrhi.append(table)
        for enc in ref.ENCODINGS:
            samples = [ref.png_sample(c, enc, depth) for c in range(n)]
            b = {"encoding": enc, "depth": depth, "count": n,
                 "sample": [v for v, _ in samples], "clip": [k for _, k in samples], "nits_2020": None}
            if ref.is_pq(enc):
                b["nits_2020"] = [ref.to_f64(ref.pq_eotf(Q(v, ref.PNG_MAX))) for v, _ in samples]
            browser.append(b)
    return {"qrhi": qrhi, "browser": browser}


def endpoint_codes(encoding, depth):
    """Narrow: min permitted, black - 1, black, black + 1, peak - 1, peak,
    peak + 1, max permitted. Full: black, black + 1, peak - 1, peak."""
    top = ref.max_code(depth)
    if ref.is_narrow(encoding):
        lo, hi = ref.black(encoding, depth), ref.nominal_peak(encoding, depth)
        s = depth - 10
        return [4 << s, lo - 1, lo, lo + 1, hi - 1, hi, hi + 1, (1020 << s) - 1]
    return [0, 1, top - 1, top]


def codes_group():
    cases = []
    for enc in ref.ENCODINGS:
        for depth in ref.DEPTHS:
            pal = [bg(enc, depth)] + [entry([c] * 3) for c in endpoint_codes(enc, depth)]
            cases.append(make(f"codes-endpoints-{spec.fmt_id(enc, depth)}", enc, depth, pal))
    # Single-channel ramps in separate rows find channel swaps.
    for depth in ref.DEPTHS:
        top = ref.max_code(depth)
        steps = [top * k // 7 for k in range(8)]
        pal = [entry([0, 0, 0])]
        rects = []
        for ch in range(3):
            for i, v in enumerate(steps):
                codes = [0, 0, 0]
                codes[ch] = v
                pal.append(entry(codes))
                rects.append([i * 4, ch * 4, 4, 4, len(pal) - 1])
        cases.append(make(f"codes-channel-ramps-pq-full-{depth}", "pq-full", depth, pal,
                          size=[32, 12], layout={"kind": "rects", "background": 0, "rects": rects}))
    return cases


# --- luminance -----------------------------------------------------------

LUMINANCE_NITS = ["0", "0.005", "0.05", "1", "80", "100", "203", "1000", "4000", "10000"]


def from_frame_md():
    return {"primaries": [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]], "white": [0.3127, 0.3290],
            "max_mastering_nits": 1000.0, "min_mastering_nits": 0.0001, "light_level": {"kind": "from_frame"}}


def luminance_group():
    cases = []
    # Neutral PQ patches. Codes are quantized from the intended luminance;
    # expected values use the quantized codes.
    for enc in PQ_ENCODINGS:
        for depth in ref.DEPTHS:
            pal = [bg(enc, depth)]
            for nits in LUMINANCE_NITS:
                code = ref.code_from_signal(ref.pq_inverse_eotf(Q(nits)), enc, depth)
                after = spec.intended_nominal([code] * 3, enc, depth)
                pal.append(entry([code] * 3, {"nits": float(Q(nits)), "after_quantization": after}))
            cases.append(make(f"lum-neutral-{spec.fmt_id(enc, depth)}", enc, depth, pal))
    # A uniform frame at the narrow nominal peak separates QRhi (full-range PQ)
    # from browser (range-mapped) light levels.
    for enc in ("pq-narrow", "hlg-narrow"):
        for depth in ref.DEPTHS:
            pal = [entry([ref.nominal_peak(enc, depth)] * 3)]
            cases.append(make(f"lum-uniform-peak-{spec.fmt_id(enc, depth)}", enc, depth, pal, size=[16, 9],
                              layout={"kind": "rects", "background": 0, "rects": []}, metadata=from_frame_md()))
    return cases


# --- gamut ---------------------------------------------------------------

# Red, green, blue, cyan, magenta, yellow in the source space.
GAMUT_COLOURS = [(1, 0, 0), (0, 1, 0), (0, 0, 1), (0, 1, 1), (1, 0, 1), (1, 1, 0)]
GAMUT_Y = ["10", "100"]


def gamut_entry(space, rgb, big_y, enc, depth):
    m = ref.rgb_to_xyz(space)
    xyz = ref.mat_vec(m, rgb)
    k = Q(big_y) / xyz[1]
    xyz = tuple(v * k for v in xyz)
    lin = ref.mat_vec(ref.mat_inv(ref.rgb_to_xyz("bt2020")), xyz)
    if max(lin) > 10000:
        raise ValueError(f"{space} {rgb} above the PQ range")
    # P3-D65 red has a small negative BT.2020 blue component. It is clipped
    # to 0 before encoding; `container_clip` records this.
    codes = [ref.code_from_signal(ref.pq_inverse_eotf(max(v, 0)), enc, depth) for v in lin]
    intended = {
        "space": space,
        "rgb": list(rgb),
        "Y": float(Q(big_y)),
        "xyz": [ref.to_f64(v) for v in xyz],
        "nits_2020": [ref.to_f64(v) for v in lin],
        "scrgb": [ref.to_f64(v) for v in ref.scrgb_from_nits_2020(lin)],
        "container_clip": min(lin) < 0,
        "after_quantization": spec.intended_nominal(codes, enc, depth),
    }
    return entry(codes, intended)


def gamut_group():
    # Primaries and secondaries in the BT.2020 PQ container. Their scRGB
    # values have negative and greater-than-one components.
    cases = []
    for enc in PQ_ENCODINGS:
        for depth in ref.DEPTHS:
            pal = [bg(enc, depth)]
            for space in ("bt709", "p3-d65", "bt2020"):
                for big_y in GAMUT_Y:
                    for rgb in GAMUT_COLOURS:
                        pal.append(gamut_entry(space, rgb, big_y, enc, depth))
            cases.append(make(f"gamut-{spec.fmt_id(enc, depth)}", enc, depth, pal))
    return cases


# --- mixed channels ------------------------------------------------------


def bt2111_palette(enc, depth):
    """All discrete BT.2111-3 levels; the ramp is covered by the scalar tables."""
    t = BT2111[(enc, depth)]
    pal = [bg(enc, depth)]
    for label in ("100", "75", "58"):
        if label in t:
            pal += [entry(c) for c in bars(*t[label])]
    pal.append(entry([t["grey40"]] * 3))
    pal += [entry([c] * 3) for c in t["steps"]]
    pal += [entry(c) for c in t["bt709"]]
    pal += [entry([c] * 3) for c in t["pluge"]]
    return pal


def small_channel_palette(enc, depth):
    """Bright saturated patches with one small nonzero channel above black."""
    zero = ref.black(enc, depth)
    peak = ref.nominal_peak(enc, depth)
    span = peak - zero
    brights = [zero + span * 58 // 100, zero + span * 80 // 100, peak]
    smalls = [1, 2, 4, 16] if depth == 10 else [1, 4, 16, 64]
    pal = [bg(enc, depth)]
    for bright in brights:
        for small in smalls:
            for ch in range(3):
                one = [zero] * 3
                one[ch] = bright
                one[(ch + 1) % 3] = zero + small
                pal.append(entry(one))
                two = [bright] * 3
                two[ch] = zero + small
                pal.append(entry(two))
    return pal


def mixed_group():
    cases = []
    for enc in ref.ENCODINGS:
        for depth in ref.DEPTHS:
            cases.append(make(f"mixed-bt2111-{spec.fmt_id(enc, depth)}", enc, depth, bt2111_palette(enc, depth)))
    for enc in PQ_ENCODINGS:
        for depth in ref.DEPTHS:
            cases.append(make(f"mixed-small-channel-{spec.fmt_id(enc, depth)}", enc, depth,
                              small_channel_palette(enc, depth)))
    return cases


# --- geometry ------------------------------------------------------------


def geometry_group():
    cases = []
    for depth in ref.DEPTHS:
        top = ref.max_code(depth)
        mid = top // 2
        k = 1 << (depth - 10)
        black, red, green, blue, white = [0, 0, 0], [top, 0, 0], [0, top, 0], [0, 0, top], [top] * 3

        def g(name, palette, rows):
            cases.append(make(f"geom-{name}-pq-full-{depth}", "pq-full", depth, [entry(c) for c in palette],
                              size=[len(rows[0]), len(rows)], layout={"kind": "rows", "rows": rows}))

        # Asymmetric corner colours on an odd width find flips and crops.
        rows = [[0] * 7 for _ in range(5)]
        rows[0][0], rows[0][6], rows[4][0], rows[4][6] = 1, 2, 3, 4
        g("corners", [black, red, green, blue, [top, mid, 0]], rows)

        # One-pixel vertical and horizontal lines with a marked crossing.
        rows = [[0] * 9 for _ in range(7)]
        for y in range(7):
            rows[y][2] = 1
        for x in range(9):
            rows[5][x] = 2
        rows[5][2] = 3
        g("lines", [black, red, green, blue], rows)

        # One-pixel checkerboard finds scaling and filtering.
        g("checker", [black, white], [[(x + y) % 2 for x in range(5)] for y in range(3)])

        # One-pixel border with an orientation marker inside the top-left corner.
        rows = [[1 if x in (0, 10) or y in (0, 8) else 0 for x in range(11)] for y in range(9)]
        rows[1][1] = 2
        g("border", [[mid] * 3, white, red], rows)

        # A different code triple at each pixel of an odd width finds row-pitch errors.
        unique = [[(10 * x + 1) * k, (100 * y + 7) * k, (3 * (x + 13 * y) + 2) * k]
                  for y in range(3) for x in range(13)]
        g("unique", unique, [[y * 13 + x for x in range(13)] for y in range(3)])
    return cases


# --- state ---------------------------------------------------------------

MARKERS = {"a": 0xA5, "b": 0x5A}
MARKER_BITS = 8
MARKER_CELL = 4


def marker_frame(name, depth):
    """Binary marker cells (MSB left) above one 8×8 patch."""
    top = ref.max_code(depth)
    patch = [top * 3 // 4, top // 2, top // 4] if name == "a" else [top // 4, top // 2, top * 3 // 4]
    pal = [entry([0, 0, 0]), entry([top] * 3), entry(patch)]
    value = MARKERS[name]
    rects = [[i * MARKER_CELL, 0, MARKER_CELL, MARKER_CELL, 1]
             for i in range(MARKER_BITS) if value >> (MARKER_BITS - 1 - i) & 1]
    rects.append([12, 8, 8, 8, 2])
    c = make(f"state-frame-{name}-pq-full-{depth}", "pq-full", depth, pal, size=[MARKER_BITS * MARKER_CELL, 20],
             layout={"kind": "rects", "background": 0, "rects": rects})
    c["marker"] = {"value": value, "bits": MARKER_BITS, "cell": MARKER_CELL, "origin": [0, 0], "one": 1, "zero": 0}
    return c


def explicit_md(primaries, max_nits, cll, fall):
    return {"primaries": primaries, "white": [0.3127, 0.3290], "max_mastering_nits": max_nits,
            "min_mastering_nits": 0.0001, "light_level": {"kind": "explicit", "max_cll": cll, "max_fall": fall}}


M1 = explicit_md([[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]], 1000.0, 1000.0, 400.0)
M2 = explicit_md([[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]], 4000.0, 2000.0, 100.0)


def sequences(depth, frames):
    a, b = (f"state-frame-{n}-pq-full-{depth}" for n in ("a", "b"))
    ll = spec.expect({**frames[a], "metadata": from_frame_md()})["metadata"]

    def show(frame, wait=True, metadata=None, **expect):
        return {"event": "show", "frame": frame, "wait": wait, "metadata": metadata, "expect": expect}

    def ev(name, **args):
        return {"event": name, **args}

    def explicit(md):
        r = spec.resolve_metadata(md, None)
        return {"qrhi": r, "browser": r}

    seqs = {
        # Two acknowledged frames; the second marker is current.
        "sequential": [ev("open"), show(a, outcome="ready", marker=MARKERS["a"]),
                       show(b, outcome="ready", marker=MARKERS["b"])],
        # A new frame before acknowledgement cancels the old request.
        "replace-pending": [ev("open"), show(a, wait=False, outcome="cancelled"),
                            show(b, outcome="ready", marker=MARKERS["b"])],
        # Metadata explicit, off, from frame, then changed.
        "metadata": [ev("open"), show(a, metadata=M1, outcome="ready", resolved=explicit(M1)),
                     show(a, outcome="ready", resolved=None),
                     show(a, metadata=from_frame_md(), outcome="ready", resolved=ll),
                     show(b, metadata=M2, outcome="ready", resolved=explicit(M2))],
        # Host close drops the pending request; the closed surface rejects new frames.
        "close-pending": [ev("open"), show(a, wait=False, outcome="cancelled"), ev("close"),
                          show(b, outcome="error", error="any")],
        # An external disconnect completes the pending request.
        "disconnect-pending": [ev("open"), show(a, wait=False, outcome="error", error="Disconnected"),
                               ev("disconnect")],
        # A resize invalidates the surface; frames need a new open at the new size.
        "resize-pending": [ev("open"), show(a, wait=False, outcome="error", error="any"), ev("resize", size=[64, 40]),
                           show(b, outcome="error", error="any"), ev("open"),
                           show(b, outcome="ready", marker=MARKERS["b"])],
        # Device loss fails the pending request; no later acknowledgement is accepted.
        "device-loss-pending": [ev("open"), show(a, wait=False, outcome="error", error="any"), ev("device_loss")],
    }
    return [{"id": f"seq-{name}-{depth}", "steps": steps} for name, steps in seqs.items()]


def state_group():
    return [marker_frame(n, d) for d in ref.DEPTHS for n in ("a", "b")]


# --- invalid -------------------------------------------------------------


def invalid_group():
    base = make("x", "pq-full", 10, [entry([0, 0, 0])], size=[2, 2],
                layout={"kind": "rects", "background": 0, "rects": []})

    def bad(case_id, error, **changes):
        return {"id": case_id, "error": error, "input": {**base, "id": case_id, **changes}}

    return [
        bad("invalid-encoding", "unknown_encoding", format={"encoding": "hlg-full", "depth": 10}),
        bad("invalid-depth-8", "unsupported_depth", format={"encoding": "pq-full", "depth": 8}),
        bad("invalid-depth-16", "unsupported_depth", format={"encoding": "pq-full", "depth": 16}),
        bad("invalid-empty", "empty_image", size=[0, 2]),
        bad("invalid-code-10", "code_out_of_range", palette=[entry([1024, 0, 0])]),
        bad("invalid-code-12", "code_out_of_range", format={"encoding": "pq-full", "depth": 12},
            palette=[entry([0, 0, 4096])]),
        bad("invalid-code-negative", "code_out_of_range", palette=[entry([0, -1, 0])]),
        # Past the right edge.
        bad("invalid-rect", "rect_out_of_bounds",
            layout={"kind": "rects", "background": 0, "rects": [[1, 0, 2, 1, 0]]}),
        bad("invalid-rect-empty", "rect_out_of_bounds",
            layout={"kind": "rects", "background": 0, "rects": [[0, 0, 0, 1, 0]]}),
        bad("invalid-rows", "layout_mismatch", layout={"kind": "rows", "rows": [[0, 0], [0]]}),
        # Palette index without an entry.
        bad("invalid-index", "layout_mismatch", layout={"kind": "rows", "rows": [[0, 1], [0, 0]]}),
        # Minimum mastering luminance above maximum.
        bad("invalid-mastering", "invalid_metadata", metadata={**M1, "min_mastering_nits": 2000.0}),
        bad("invalid-chromaticity", "invalid_metadata", metadata={**M1, "white": [0.3127, 1.2]}),
        # MaxFALL above MaxCLL.
        bad("invalid-light-level", "invalid_metadata",
            metadata=explicit_md(M1["primaries"], 1000.0, 100.0, 200.0)),
        bad("invalid-light-level-negative", "invalid_metadata",
            metadata=explicit_md(M1["primaries"], 1000.0, -1.0, 0.0)),
    ]
