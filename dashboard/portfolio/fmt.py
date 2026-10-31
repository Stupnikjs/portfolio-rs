"""Formats d'affichage à la française : milliers séparés par une espace insécable, virgule décimale.

Fonctions pures, sans dépendance au reste du package. Elles alignent les KPIs et les tableaux
sur les graphiques Plotly, qui utilisent les mêmes séparateurs (cf. `theme.PLOTLY_SEPARATORS`).
"""
from __future__ import annotations

import pandas as pd

NBSP = "\u00a0"  # espace insécable : évite qu'un montant se coupe en fin de ligne


def _french(text: str) -> str:
    """Convertit un nombre formaté à l'anglo-saxonne (1,234.56) au format français (1 234,56)."""
    return text.replace(",", NBSP).replace(".", ",")


def _number(x: float, decimals: int, signed: bool, grouped: bool) -> str:
    # round(...) + 0.0 : évite l'affichage « -0,00 » pour une valeur négative infime.
    value = round(float(x), decimals) + 0.0
    spec = f"{'+' if signed else ''}{',' if grouped else ''}.{decimals}f"
    return _french(format(value, spec))


def fmt_number(x, decimals: int = 0, signed: bool = False, na: str = "—") -> str:
    """1234.5 -> « 1 234,5 » (decimals=1)."""
    if x is None or pd.isna(x):
        return na
    return _number(x, decimals, signed, grouped=True)


def fmt_eur(x, decimals: int = 2, signed: bool = False, na: str = "—") -> str:
    """1234.5 -> « 1 234,50 € » ; signed=True -> « +1 234,50 € »."""
    if x is None or pd.isna(x):
        return na
    return f"{_number(x, decimals, signed, grouped=True)}{NBSP}€"


def fmt_pct(x, decimals: int = 1, signed: bool = True, na: str = "—") -> str:
    """3.456 -> « +3,5 % » ; signed=False -> « 3,5 % »."""
    if x is None or pd.isna(x):
        return na
    return f"{_number(x, decimals, signed, grouped=False)}{NBSP}%"
