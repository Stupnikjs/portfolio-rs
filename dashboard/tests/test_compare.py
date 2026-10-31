"""Tests de portfolio/compare.py (logique pure : rendements par actif, séries communes, bêta)."""
import sys
from pathlib import Path

import numpy as np
import pandas as pd

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from portfolio import compare as cp  # noqa: E402
from portfolio import tag_stats as ts  # noqa: E402


def _history(rows: list[tuple[str, list[dict]]]) -> pd.DataFrame:
    """rows : (date, [actifs]). Colonnes minimales de history_to_df."""
    return pd.DataFrame({"date": pd.to_datetime([d for d, _ in rows]), "assets": [a for _, a in rows]})


def _a(symbol, qty, price, kind="Stock"):
    return {"symbol": symbol, "kind": kind, "quantity": qty, "price_eur": price}


HIST = _history([
    ("2026-01-04", [_a("A", 10, 100.0)]),
    ("2026-01-11", [_a("A", 10, 110.0)]),                       # A : +10 %
    ("2026-01-18", [_a("A", 20, 99.0), _a("B", 1, 50.0, "Crypto")]),   # A : -10 %, achat de 10 titres ; B achetée
    ("2026-01-25", [_a("A", 20, 99.0), _a("B", 1, 55.0, "Crypto")]),   # A : 0 %, B : +10 %
])


def test_asset_returns_ignore_trades_and_need_a_previous_holding():
    r = cp.asset_returns(HIST).set_index(["symbol", "date"])["ret"]
    assert abs(r[("A", pd.Timestamp("2026-01-11"))] - 0.10) < 1e-9
    assert abs(r[("A", pd.Timestamp("2026-01-18"))] - (-0.10)) < 1e-9   # l'achat n'a aucun effet
    assert ("B", pd.Timestamp("2026-01-18")) not in r.index             # pas détenue au snapshot précédent
    assert abs(r[("B", pd.Timestamp("2026-01-25"))] - 0.10) < 1e-9


def test_single_asset_tag_equals_the_asset_series():
    sym_tags = {"A": ["T"], "B": ["Autre"]}
    t_ret, a_ret = ts.tag_returns(HIST, sym_tags), cp.asset_returns(HIST)
    s = cp.compare_series(HIST["date"], t_ret, a_ret, "T", "A", "Tag · T", "Actif · A")
    tag_part = s[s["tag"] == "Tag · T"].reset_index(drop=True)
    asset_part = s[s["tag"] == "Actif · A"].reset_index(drop=True)
    assert list(tag_part["date"]) == list(asset_part["date"])
    assert np.allclose(tag_part["ret"], asset_part["ret"])
    assert tag_part["ret"].iloc[0] == 0.0                               # point d'ancrage
    assert tag_part["date"].iloc[0] == pd.Timestamp("2026-01-04")       # snapshot précédent la 1re semaine


def test_only_common_weeks_are_compared():
    sym_tags = {"A": ["T"], "B": ["Autre"]}
    t_ret, a_ret = ts.tag_returns(HIST, sym_tags), cp.asset_returns(HIST)
    s = cp.compare_series(HIST["date"], t_ret, a_ret, "T", "B", "Tag · T", "Actif · B")
    assert list(s[s["tag"] == "Actif · B"]["date"]) == [pd.Timestamp("2026-01-18"), pd.Timestamp("2026-01-25")]
    # B n'a de rendement que le 25/01 : une seule semaine comparée + l'ancrage (18/01)
    assert len(s[s["tag"] == "Tag · T"]) == 2


def test_no_common_week_gives_empty_frame():
    t_ret = ts.tag_returns(HIST, {"A": ["T"], "B": ["Autre"]})
    a_ret = cp.asset_returns(HIST[HIST["date"] <= "2026-01-11"])         # B inconnue
    assert cp.compare_series(HIST["date"], t_ret, a_ret, "T", "B", "x", "y").empty


def test_catalog_lists_assets_with_returns_and_held_flag():
    a_ret = cp.asset_returns(HIST)
    df_all = pd.DataFrame({"symbol": ["A"], "kind": ["Stock"], "value_eur": [1980.0]})
    cat = cp.asset_catalog(a_ret, df_all).set_index("symbol")
    assert set(cat.index) == {"A", "B"}
    assert bool(cat.loc["A", "held"]) and not bool(cat.loc["B", "held"])
    assert cat.loc["B", "kind"] == "Crypto"


def _rebased(tag_rets, asset_rets):
    dates = pd.date_range("2026-01-04", periods=len(tag_rets) + 1, freq="7D")
    frames = [
        pd.DataFrame({"date": dates, "tag": label, "ret": [0.0, *rets]})
        for label, rets in (("T", tag_rets), ("A", asset_rets))
    ]
    return ts.rebase_returns(pd.concat(frames, ignore_index=True))


def test_beta_and_correlation_of_a_leveraged_series():
    asset = [0.01, -0.02, 0.015, 0.03, -0.01, 0.02, -0.015, 0.005, 0.01, -0.005]
    rb = _rebased([2 * r for r in asset], asset)
    st_ = cp.relative_stats(rb, "T", "A")
    assert st_["n_periods"] == 10
    assert abs(st_["beta"] - 2.0) < 1e-9 and abs(st_["corr"] - 1.0) < 1e-9
    assert st_["hit_rate"] == 60.0      # le tag (x2) bat l'actif quand le rendement est positif : 6 semaines sur 10
    assert st_["diff"] == st_["perf_tag"] - st_["perf_asset"]


def test_beta_and_correlation_are_nan_below_min_periods():
    rb = _rebased([0.01, 0.02, -0.01], [0.02, 0.01, -0.02])
    st_ = cp.relative_stats(rb, "T", "A")
    assert st_["n_periods"] == 3
    assert np.isnan(st_["beta"]) and np.isnan(st_["corr"])


def test_relative_series_is_index_difference():
    rb = _rebased([0.10, 0.10], [0.0, 0.0])
    d = cp.relative_series(rb, "T", "A")
    assert abs(d.iloc[-1] - 21.0) < 1e-9 and d.iloc[0] == 0.0
