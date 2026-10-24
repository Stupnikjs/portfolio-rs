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
    assert s.loc["Tech", "latent_pct"] == pytest.approx(20.0)
    assert s.loc["Tech", "invested"] == 600.0           # total investi cumulé : 500 + 100
    assert s.loc["Tech", "perf_pct"] == pytest.approx(25.0)
    assert s.loc["Tech", "positions"] == 1              # OLD est soldée
    assert s.loc["Crypto", "pnl_color"] == "Perte"


def test_summary_weight_uses_real_total_without_double_counting_or_cash():
    s = ts.tag_summary(positions()).set_index("tag")
    assert s.loc["Tech", "weight_pct"] == pytest.approx(60.0)  # 600 / (600+300+100)


def test_positions_of_tag_handles_untagged_bucket():
    df = positions()
    assert list(ts.positions_of_tag(df, ts.UNTAGGED)["symbol"]) == ["XYZ"]
    assert set(ts.positions_of_tag(df, "Tech")["symbol"]) == {"AIR", "OLD"}


def test_tag_history_sums_assets_per_date_and_tag():
    history = pd.DataFrame({
        "date": pd.to_datetime(["2026-03-01", "2026-03-08"]),
        "assets": [
            [{"symbol": "AIR", "kind": "Stock", "value_eur": 100.0, "cost_basis_eur": 80.0},
             {"symbol": "BTC", "kind": "Crypto", "value_eur": 50.0, "cost_basis_eur": 60.0}],
            [{"symbol": "AIR", "kind": "Stock", "value_eur": 120.0, "cost_basis_eur": 80.0}],
        ],
    })
    h = ts.tag_history(history, {"AIR": ["Tech", "Dividende"], "BTC": ["Crypto"]})
    tech = h[h["tag"] == "Tech"].reset_index(drop=True)

    assert list(tech["value_eur"]) == [100.0, 120.0]
    assert tech.loc[1, "pnl_pct"] == pytest.approx(50.0)
    assert len(h[h["tag"] == "Crypto"]) == 1


def test_tag_history_without_detail_is_empty():
    assert ts.tag_history(None, {}).empty
    legacy = pd.DataFrame({"date": pd.to_datetime(["2026-03-01"]), "assets": [None]})
    assert ts.tag_history(legacy, {}).empty


def test_trades_by_month_per_tag():
    trades = [
        {"time": "2026-03-02T10:00:00Z", "symbol": "AIR", "kind": "Buy", "value_eur": 100.0},
        {"time": "2026-03-20T10:00:00Z", "symbol": "AIR", "kind": "Sell", "value_eur": 40.0},
        {"time": "2026-04-01T10:00:00Z", "symbol": "BTC", "kind": "Buy", "value_eur": 70.0},
    ]
    t = ts.tag_trades(trades, {"AIR": ["Tech"], "BTC": ["Crypto"]})
    m = ts.trades_by_month(t[t["tag"] == "Tech"])

    assert list(m["month"]) == ["2026-03"]
    assert m.loc[0, "bought"] == 100.0 and m.loc[0, "sold"] == 40.0 and m.loc[0, "n_trades"] == 2
    assert ts.trades_by_month(ts.tag_trades(None, {})).empty