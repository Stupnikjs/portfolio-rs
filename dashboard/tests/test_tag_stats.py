"""Tests de portfolio/tag_stats.py (logique pure par tag)."""
import sys
from pathlib import Path

import pandas as pd
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from portfolio import tag_stats as ts  # noqa: E402


def positions() -> pd.DataFrame:
    rows = [
        # symbol, kind, value, cost_basis, pnl latent, realized pnl, realized cost, tags
        ("AIR", "Stock", 600.0, 500.0, 100.0, 0.0, 0.0, ["Tech", "Dividende"]),
        ("BTC", "Crypto", 300.0, 400.0, -100.0, 0.0, 0.0, ["Crypto"]),
        ("OLD", "Stock", 0.0, 0.0, 0.0, 50.0, 100.0, ["Tech"]),   # soldée : garde son P&L réalisé
        ("XYZ", "Stock", 100.0, 100.0, 0.0, 0.0, 0.0, []),        # sans tag
        ("EUR", "Cash", 999.0, 999.0, 0.0, 0.0, 0.0, []),         # cash : ignoré
    ]
    df = pd.DataFrame(rows, columns=["symbol", "kind", "value_eur", "cost_basis_eur", "pnl_eur",
                                     "realized_pnl_eur", "realized_cost_eur", "tags"])
    df["pnl_total_eur"] = df["pnl_eur"] + df["realized_pnl_eur"]
    df["invested_eur"] = df["cost_basis_eur"] + df["realized_cost_eur"]
    return df


def test_summary_counts_multi_tag_assets_in_each_tag_and_keeps_closed_positions():
    s = ts.tag_summary(positions()).set_index("tag")

    assert set(s.index) == {"Tech", "Dividende", "Crypto", ts.UNTAGGED}
    assert s.loc["Tech", "value"] == 600.0 and s.loc["Dividende", "value"] == 600.0
    assert s.loc["Tech", "pnl_total"] == 150.0          # AIR 100 + OLD 50 réalisé
    assert s.loc["Tech", "cost_basis"] == 500.0         # capital engagé : lots ouverts seulement
