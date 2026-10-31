"""Tests de portfolio/fmt.py (formats français)."""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from portfolio.fmt import fmt_eur, fmt_number, fmt_pct  # noqa: E402

NB = "\u00a0"


def test_fmt_eur_groups_thousands_and_uses_decimal_comma():
    assert fmt_eur(1234.5) == f"1{NB}234,50{NB}€"
    assert fmt_eur(1234567.891, decimals=0) == f"1{NB}234{NB}568{NB}€"


def test_fmt_eur_signed():
    assert fmt_eur(12.3, signed=True) == f"+12,30{NB}€"
    assert fmt_eur(-1234.5, signed=True) == f"-1{NB}234,50{NB}€"


def test_tiny_negative_value_has_no_negative_zero():
    assert fmt_eur(-0.001) == f"0,00{NB}€"
    assert fmt_pct(-0.001, decimals=1, signed=False) == f"0,0{NB}%"


def test_fmt_pct():
    assert fmt_pct(3.456) == f"+3,5{NB}%"
    assert fmt_pct(-12.04) == f"-12,0{NB}%"
    assert fmt_pct(3.456, decimals=2, signed=False) == f"3,46{NB}%"


def test_fmt_number():
    assert fmt_number(0.00123456, decimals=4) == "0,0012"
    assert fmt_number(1234.5, decimals=1) == f"1{NB}234,5"


@pytest.mark.parametrize("fn", [fmt_eur, fmt_pct, fmt_number])
def test_missing_values_show_a_dash(fn):
    assert fn(None) == "—"
    assert fn(float("nan")) == "—"
