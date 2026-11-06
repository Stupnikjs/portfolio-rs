"""Tests de la page Stratégie (fonctions pures). Lancer depuis dashboard/ : `python -m pytest tests -q`.

Aucun accès réseau ni à data/ : les cours sont fabriqués ici."""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import pandas as pd

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from portfolio import strategy as sv  # noqa: E402


def daily_prices(values: list[float], start: str = "2026-01-01") -> pd.DataFrame:
    dates = pd.date_range(start, periods=len(values), freq="D")
    return pd.DataFrame({"date": dates, "price_eur": values})


def trades_at(times: list[str], kind: str = "Buy", price: float = 100.0) -> pd.DataFrame:
    t = pd.to_datetime(times)
    return pd.DataFrame({
        "time_naive": t, "kind": kind, "unit_price_eur": price, "quantity": 1.0, "value_eur": price,
    })


# --- value_before / ma_at_trades : références causales ---------------------------------

def test_value_before_uses_previous_day_close_and_never_the_same_day():
    s = pd.Series([1.0, 2.0, 3.0], index=pd.to_datetime(["2026-01-01", "2026-01-02", "2026-01-03"]))
    got = sv.value_before(s, pd.to_datetime(["2026-01-03 15:00", "2026-01-02 00:00", "2026-01-01 09:00"]))
    assert got.iloc[0] == 2.0   # trade du 3 -> clôture du 2
    assert got.iloc[1] == 1.0   # trade du 2 (minuit) -> clôture du 1
    assert np.isnan(got.iloc[2])  # rien avant le 1er jour


def test_ma_at_trades_has_no_lookahead():
    # 30 jours à 100, puis un saut à 200 le lendemain du trade : la MA vue par le trade reste 100.
    prices = daily_prices([100.0] * 30 + [200.0] * 10)
    ma = sv.rolling_average(prices, window=2)
    trade_day = prices["date"].iloc[29]
    ref = sv.ma_at_trades(ma, trades_at([str(trade_day + pd.Timedelta(hours=12))]))
    assert ref.iloc[0] == 100.0


# --- rolling_average ---------------------------------------------------------------------

def test_rolling_average_is_a_calendar_window():
    prices = daily_prices([float(i) for i in range(1, 15)])  # 14 jours
    ma = sv.rolling_average(prices, window=1)  # 7 jours : moyenne des 7 derniers prix
    assert ma.iloc[-1] == np.mean(range(8, 15))


def test_rolling_average_works_on_weekly_fallback():
    dates = pd.date_range("2026-01-04", periods=8, freq="7D")
    prices = pd.DataFrame({"date": dates, "price_eur": [10.0 * (i + 1) for i in range(8)]})
    ma = sv.rolling_average(prices, window=4)  # 28 jours = 4 points hebdo
    assert ma.iloc[-1] == np.mean([50.0, 60.0, 70.0, 80.0])


# --- dip_indicators / dip_sufficiency_analysis -------------------------------------------

def test_dip_indicators_peak_bars_and_drawdown():
    values = [100, 101, 99, 102, 100, 103, 101, 104, 102, 110, 108, 105, 99]
    ind = sv.dip_indicators(daily_prices(values), window_weeks=8)
    last = ind.iloc[-1]
    assert last["bars_since_peak"] == 3          # 110 est 3 observations avant
    assert np.isclose(last["drawdown_pct"], (99 / 110 - 1) * 100)
    expected = abs(last["drawdown_pct"]) / (last["volatility_pct"] * np.sqrt(3))
    assert np.isclose(last["dip_score"], expected)


def test_dip_score_is_zero_at_a_peak():
    values = [100, 101, 99, 102, 100, 103, 101, 104, 102, 110]  # dernier point = plus haut
    ind = sv.dip_indicators(daily_prices(values), window_weeks=8)
    assert ind.iloc[-1]["bars_since_peak"] == 0
    assert ind.iloc[-1]["dip_score"] == 0.0


def test_score_scales_with_sqrt_of_time_since_peak():
    # Même drawdown et même σ : plus le pic est ancien, plus le score doit être bas (√n).
    rng = np.random.default_rng(0)
    noise = 100 + np.cumsum(rng.normal(0, 0.5, 40))
    ind = sv.dip_indicators(daily_prices(list(noise)), window_weeks=8)
    valid = ind.dropna()
    manual = valid["drawdown_pct"].abs() / (valid["volatility_pct"] * np.sqrt(valid["bars_since_peak"]))
    both = valid[valid["bars_since_peak"] > 0]
    assert np.allclose(both["dip_score"], manual[both.index])


def test_buy_right_after_a_peak_close_is_labelled_au_sommet():
    values = [100, 101, 99, 102, 100, 103, 101, 104, 102, 110, 110.5, 111]
    prices = daily_prices(values)
    # Achat le jour suivant le plus haut (clôture J-1 = 111 = sommet de la fenêtre).
    buys = trades_at([str(prices["date"].iloc[-1] + pd.Timedelta(days=1, hours=10))], price=111.0)
    out = sv.dip_sufficiency_analysis(buys, prices, window=8)
    assert out["dip_verdict"].iloc[0] == "Au sommet"
    assert out["dip_score"].iloc[0] == 0.0


def test_dip_analysis_without_enough_prices_returns_placeholders():
    buys = trades_at(["2026-01-05 10:00"])
    out = sv.dip_sufficiency_analysis(buys, daily_prices([100.0, 101.0]), window=8)
    assert out["dip_verdict"].iloc[0] == "—"
    assert out["dip_score"].isna().all()


# --- timing_analysis : la référence ne dépend pas du filtre de période ----------------------

def test_timing_uses_full_series_not_the_displayed_window():
    # 60 jours à 100 puis 10 jours à 120 ; achat à 110 le 68e jour.
    prices = daily_prices([100.0] * 60 + [120.0] * 10)
    trade = trades_at([str(prices["date"].iloc[67] + pd.Timedelta(hours=9))], price=110.0)
    full = sv.timing_analysis(trade, prices, ma_window=4)
    # MA 28 j à la clôture de J-1 (jour 67, indice 66) : 21 jours à 100 + 7 jours à 120 = 105.
    assert np.isclose(full["avg_buy_vs_ma_pct"], (110 / 105 - 1) * 100)
    # Sur une fenêtre tronquée à 5 jours, la MA n'a plus assez d'historique (échauffement perdu) :
    # aucun verdict. C'est pourquoi le rendu passe TOUJOURS la série complète à timing_analysis.
    short = sv.timing_analysis(trade, prices.iloc[-5:].reset_index(drop=True), ma_window=4)
    assert short["avg_buy_vs_ma_pct"] is None
