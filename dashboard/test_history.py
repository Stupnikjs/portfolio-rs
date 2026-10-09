"""Tests de la page Historique. Lancer depuis dashboard/ : `python -m pytest tests -q`.

Aucun accès à data/ : les chargements sont remplacés par des données de test."""
from __future__ import annotations

import sys
from pathlib import Path

import pandas as pd
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import history_view as hv  # noqa: E402
from data import history_to_df  # noqa: E402

PAGE = str(Path(__file__).resolve().parent.parent / "pages" / "5_📈_Historique.py")


def raw_history(n: int = 6, incomplete_at: int | None = None) -> list[dict]:
    """n snapshots hebdomadaires (dimanches) à partir du 2026-03-01."""
    rows = []
    for i in range(n):
        value, cost = 1000.0 + 100 * i, 900.0 + 50 * i
        rows.append({
            "date": (pd.Timestamp("2026-03-01") + pd.Timedelta(weeks=i)).strftime("%Y-%m-%d"),
            "total_value_eur": value,
            "total_cost_basis_eur": cost,
            "total_pnl_eur": value - cost,
            "version": 2,
            "complete": i != incomplete_at,
            "assets": [
                {"symbol": "BTC", "kind": "Crypto", "quantity": 0.01, "price_eur": value - 100,
                 "value_eur": 0.01 * (value - 100), "cost_basis_eur": cost - 100, "price_status": "ok"},
                {"symbol": "AIR", "kind": "Stock", "quantity": 1.0, "price_eur": 100.0,
                 "value_eur": 100.0, "cost_basis_eur": 100.0,
                 "price_status": "ok" if i != incomplete_at else "carried"},
            ],
        })
    return rows


# --- history_to_df -------------------------------------------------------------------

def test_history_to_df_sorts_dedups_and_parses_dates():
    raw = raw_history(3)
    raw = [raw[2], raw[0], raw[1], {**raw[1], "total_value_eur": 999.0}]  # désordre + doublon
    df = history_to_df(raw)

    assert list(df["date"]) == sorted(df["date"])
    assert len(df) == 3
    assert pd.api.types.is_datetime64_any_dtype(df["date"])
    assert df.loc[1, "total_value_eur"] == 999.0  # le doublon garde la dernière entrée


def test_history_to_df_reads_legacy_format():
    legacy = [{"week_end": "2026-01-04", "total_value_eur": 10.0, "total_cost_basis_eur": 8.0}]
    df = history_to_df(legacy)

    assert df.loc[0, "date"] == pd.Timestamp("2026-01-04")
    assert bool(df.loc[0, "complete"]) is True
    assert df.loc[0, "total_pnl_eur"] == 2.0
    assert df.loc[0, "assets"] is None


def test_history_to_df_rejects_missing_columns_and_accepts_empty():
    with pytest.raises(ValueError, match="total_cost_basis_eur"):
        history_to_df([{"date": "2026-01-05", "total_value_eur": 1.0}])
    assert history_to_df([]).empty


# --- filtres, stats, point actuel ----------------------------------------------------

def test_filter_period_is_relative_to_the_last_date():
    df = history_to_df(raw_history(10))  # 10 semaines
    last = df["date"].max()

    view = hv.filter_period(df, 31)
    assert view["date"].min() >= last - pd.Timedelta(days=31)
    assert view["date"].max() == last
    assert len(view) == 5  # 0, 7, 14, 21, 28 jours avant la fin
    assert len(hv.filter_period(df, None)) == 10
    assert hv.filter_period(df.iloc[0:0], 31).empty


def test_with_live_point_appends_a_live_row_without_mutating_input():
    df = history_to_df(raw_history(3))
    kpis = {"total_value_eur": 2000.0, "total_cost_basis_eur": 1500.0}
    out = hv.with_live_point(df, kpis, now=pd.Timestamp("2026-06-01"))

    assert len(out) == 4 and out.iloc[-1]["live"]
    assert out.iloc[-1]["total_pnl_eur"] == 500.0
    assert not out.iloc[:-1]["live"].any()
    assert len(df) == 3 and "live" not in df.columns


def test_with_live_point_is_skipped_when_not_after_the_last_snapshot():
    df = history_to_df(raw_history(3))
    kpis = {"total_value_eur": 1.0, "total_cost_basis_eur": 1.0}
    out = hv.with_live_point(df, kpis, now=df["date"].max())

    assert len(out) == 3 and not out["live"].any()


def test_period_stats_values_and_deltas():
    df = history_to_df(raw_history(3))  # valeurs 1000, 1100, 1200 ; cost 900, 950, 1000
    s = hv.period_stats(df)

    assert s["value"] == 1200.0 and s["cost"] == 1000.0
    assert s["pnl"] == 200.0 and s["pnl_pct"] == pytest.approx(20.0)
    assert s["value_delta"] == 200.0 and s["cost_delta"] == 100.0


def test_period_stats_zero_cost_does_not_divide_by_zero():
    df = history_to_df([
        {"date": "2026-01-04", "total_value_eur": 0.0, "total_cost_basis_eur": 0.0},
        {"date": "2026-01-11", "total_value_eur": 5.0, "total_cost_basis_eur": 0.0},
    ])
    assert hv.period_stats(df)["pnl_pct"] == 0.0


# --- figure ---------------------------------------------------------------------------

def test_figure_has_one_curve_for_value_and_one_for_cost_basis():
    df = history_to_df(raw_history(5)).assign(live=False)
    fig = hv.build_history_figure(df)

    names = [t.name for t in fig.data]
    assert names == ["Valeur totale", "Cost basis"]
    assert list(fig.data[0].y) == list(df["total_value_eur"])
    assert list(fig.data[1].y) == list(df["total_cost_basis_eur"])


def test_figure_flags_incomplete_snapshots_and_the_live_point():
    df = history_to_df(raw_history(5, incomplete_at=2))
    df = hv.with_live_point(df, {"total_value_eur": 2000.0, "total_cost_basis_eur": 1500.0}, now=pd.Timestamp("2026-06-01"))
    fig = hv.build_history_figure(df)

    by_name = {t.name: t for t in fig.data}
    assert list(by_name["Prix manquants/reportés"].x) == [pd.Timestamp("2026-03-15")]
    assert list(by_name["Aujourd'hui"].y) == [2000.0]


def test_figure_uses_lines_only_for_long_histories():
    long_df = history_to_df(raw_history(60)).assign(live=False)
    short_df = history_to_df(raw_history(5)).assign(live=False)

    assert hv.build_history_figure(long_df).data[0].mode == "lines"
    assert hv.build_history_figure(short_df).data[0].mode == "lines+markers"


def test_asset_snapshot_df_sorted_by_value_with_all_columns():
    df = hv.asset_snapshot_df([
        {"symbol": "AIR", "value_eur": 100.0},
        {"symbol": "BTC", "value_eur": 900.0, "price_status": "ok"},
    ])
    assert list(df["symbol"]) == ["BTC", "AIR"]
    assert {"kind", "quantity", "price_eur", "cost_basis_eur", "price_status"} <= set(df.columns)


# --- la page elle-même (AppTest) ---------------------------------------------------------

def run_page(monkeypatch, history, kpis=None):
    from streamlit.testing.v1 import AppTest

    monkeypatch.setattr(hv, "load_history", lambda *a, **k: history)
    monkeypatch.setattr(hv, "_live_kpis", lambda: kpis)
    at = AppTest.from_file(PAGE, default_timeout=30)
    at.run()
    return at


def test_page_renders_chart_and_kpis(monkeypatch):
    at = run_page(monkeypatch, history_to_df(raw_history(8)),
                  {"total_value_eur": 2500.0, "total_cost_basis_eur": 1400.0})

    assert not at.exception
    assert len(at.metric) == 4
    assert at.metric[0].value == "2,500.00 €"  # point actuel inclus par défaut
    assert at.metric[1].value == "1,400.00 €"
    assert len(at.get("plotly_chart")) == 1
    assert not at.warning


def test_page_warns_about_incomplete_snapshots(monkeypatch):
    at = run_page(monkeypatch, history_to_df(raw_history(8, incomplete_at=3)))

    assert not at.exception
    assert len(at.warning) == 1 and "1 snapshot" in at.warning[0].value


def test_page_without_history_file_shows_a_message(monkeypatch):
    at = run_page(monkeypatch, None)

    assert not at.exception
    assert "introuvable" in at.info[0].value
    assert not at.get("plotly_chart")


def test_page_period_selection_changes_the_number_of_points(monkeypatch):
    import json

    def n_points(at) -> int:
        spec = json.loads(at.get("plotly_chart")[0].proto.spec)
        return len(spec["data"][0]["x"])

    at = run_page(monkeypatch, history_to_df(raw_history(30)))
    assert n_points(at) == 30  # "Tout" par défaut

    at.radio[0].set_value("1 mois").run()
    assert not at.exception
    assert n_points(at) == 5  # dimanches à 0, 7, 14, 21 et 28 jours de la fin