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
