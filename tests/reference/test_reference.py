"""Checks of the reference generator against published values and its own rules."""

from fractions import Fraction as Q

import pytest

import cases
import generate
import reference as ref
import spec


def test_generation_is_deterministic():
    assert generate.build() == generate.build()


@pytest.mark.parametrize("item", cases.invalid_group(), ids=lambda i: i["id"])
def test_invalid_case_is_rejected_with_its_error(item):
    with pytest.raises(spec.FixtureError) as e:
        spec.validate(item["input"])
    assert e.value.kind == item["error"]


def test_pq_endpoints():
    assert ref.pq_eotf(0) == 0
    assert abs(ref.pq_eotf(1) - 10000) < 1e-40
    assert abs(ref.pq_inverse_eotf(10000) - 1) < 1e-45


def test_bt2111_reference_white_codes():
    # BT.2111-3 Tables 3 and 4: 58 % PQ is 203 cd/m2.
    e = ref.pq_inverse_eotf(203)
    assert ref.code_from_signal(e, "pq-full", 10) == 594
    assert ref.code_from_signal(e, "pq-full", 12) == 2378
    assert ref.code_from_signal(e, "pq-narrow", 10) == 573
    # Table 3 has 2292 (4 × 573); the BT.2100 formula gives 2291.
    assert ref.code_from_signal(e, "pq-narrow", 12) == 2291


def test_bt2087_matrix():
    # ITU-R BT.2087-0, linear BT.709 to BT.2020, printed to 4 decimals.
    printed = ((0.6274, 0.3293, 0.0433), (0.0691, 0.9195, 0.0114), (0.0164, 0.0880, 0.8956))
    m = ref.mat_inv(ref.M_2020_TO_709)
    for row, want in zip(m, printed):
        for v, w in zip(row, want):
            assert abs(float(v) - w) <= 0.00005


def test_matrix_rows_preserve_white():
    for row in ref.M_2020_TO_709:
        assert sum(row) == 1


def test_png_sample_exact_half_goes_up():
    # 12-bit narrow code 840: 65535 * 584 / 3504 = 10922.5 exactly.
    assert ref.signal_from_code(840, "pq-narrow", 12) * 65535 == Q(21845, 2)
    assert ref.png_sample(840, "pq-narrow", 12) == (10923, "none")


def test_png_endpoints():
    assert ref.png_sample(64, "pq-narrow", 10) == (0, "none")
    assert ref.png_sample(940, "pq-narrow", 10) == (65535, "none")
    assert ref.png_sample(63, "pq-narrow", 10) == (0, "below")
    assert ref.png_sample(941, "hlg-narrow", 10) == (65535, "above")
    assert ref.png_sample(3760, "pq-narrow", 12) == (65535, "none")


def test_code_940_light_levels():
    # QRhi reads 940 as full-range PQ; the browser maps it to the PNG peak.
    qrhi = ref.pq_eotf(Q(940, 1023))
    assert abs(qrhi - 4654.397) < 0.001
    _, nits = ref.browser_expect((940, 940, 940), "pq-narrow", 10)
    assert nits == [10000, 10000, 10000]


def test_r10g10b10a2_layout():
    assert ref.r10g10b10a2((1, 2, 3)) == 0xC000_0000 | 3 << 20 | 2 << 10 | 1


def test_f16_bracket_and_nearest():
    one_step = Q(1, 1024)
    assert ref.f16_bracket(Q(1)) == (1, 1)
    assert ref.f16_bracket(1 + one_step / 3) == (1, 1 + one_step)
    assert ref.f16_bracket(-(1 + one_step / 3)) == (-1 - one_step, -1)
    # Subnormal spacing below 2^-14.
    assert ref.f16_bracket(Q(3, 2**25)) == (Q(1, 2**24), Q(2, 2**24))
    # Ties go to the even significand, also across a binade.
    assert ref.f16_nearest(1 + one_step / 2) == 1
    assert ref.f16_nearest(1 + 3 * one_step / 2) == 1 + 2 * one_step
    assert ref.f16_nearest(2 - one_step / 4) == 2
    assert ref.f16_nearest(Q(65504)) == 65504
    with pytest.raises(ValueError):
        ref.f16_bracket(Q(65505))


def test_f16_steps_cross_zero_and_binades():
    tiny = Q(1, 2**24)
    assert ref.f16_step(Q(0), 1) == tiny
    assert ref.f16_step(Q(0), -2) == -2 * tiny
    assert ref.f16_step(tiny, -2) == -tiny
    assert ref.f16_step(Q(1), -1) == 1 - Q(1, 2048)
    assert ref.f16_step(Q(1), 1) == 1 + Q(1, 1024)


def test_capture_bound_is_the_farther_neighbour():
    # 80 cd/m² neutral is scRGB 1.0, a binary16 value: no capture error.
    assert ref.scrgb_bound([80, 80, 80], "capture") == [0.0, 0.0, 0.0]
    # Black and peak white are exact too.
    assert ref.scrgb_bound([0, 0, 0], "capture") == [0.0, 0.0, 0.0]
    assert ref.scrgb_bound([10000] * 3, "capture") == [0.0, 0.0, 0.0]
    # scRGB 1 + 1/3 step: the far neighbour is 2/3 step away.
    nits = 80 * (1 + Q(1, 3072))
    b = ref.scrgb_bound([nits] * 3, "capture")[0]
    assert Q(2, 3072) <= Q(b) <= Q(2, 3072) * (1 + Q(1, 2**40))


def test_fp16_bound_counts_the_search_once():
    # Nearest binary16 is 1; the search reaches 1 + 2 steps above and
    # 1 - 2 half steps below. No second rounding term is added.
    nits = 80 * (1 + Q(1, 3072))
    b = Q(ref.scrgb_bound([nits] * 3, "fp16")[0])
    want = 2 * Q(1, 1024) - Q(1, 3072)
    assert want <= b <= want * (1 + Q(1, 2**40))
    # An exact value still allows the search range around it.
    assert ref.scrgb_bound([80, 80, 80], "fp16") == [2 / 1024] * 3
    assert ref.scrgb_bound([0, 0, 0], "fp16") == [2 / 2**24] * 3


def test_fp16_bound_rejects_a_value_at_a_rounding_midpoint():
    nits = 80 * (1 + Q(1, 2048))
    with pytest.raises(ValueError):
        ref.scrgb_bound([nits] * 3, "fp16")
    # The capture bound does not depend on the rounding direction.
    ref.scrgb_bound([nits] * 3, "capture")
