"""Page Comparer : un tag face à un actif (action, ETF ou crypto) du portefeuille.

Même méthode que la page « Vue par tag » : rendement hebdomadaire entre deux snapshots de
history.json, hors achats/ventes (quantité du snapshot précédent x variation de prix). Seules les
semaines où le tag ET l'actif sont détenus sont comparées, pour que les deux courbes reposent sur
exactement les mêmes dates. Logique pure (pandas) et figures séparées du rendu `st.*`.
"""
from __future__ import annotations

import numpy as np
import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from portfolio.assets import attach_tags, load_synced_assets
from portfolio.data import CASH_KIND, load_portfolio
from portfolio.fmt import fmt_number, fmt_pct
from portfolio.history import PERIODS, load_history
from portfolio.tag_stats import (
    positions_of_tag, rebase_returns, symbol_tags_map, tag_perf_stats, tag_returns, tag_summary,
)
from portfolio.theme import GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

TAG_COLOR = PASTEL_DARK_SEQUENCE[0]    # bleu
ASSET_COLOR = PASTEL_DARK_SEQUENCE[2]  # orange
MARGIN = dict(l=10, r=10, t=40, b=10)
MIN_PERIODS = 8  # en dessous, corrélation et bêta ne sont pas significatifs

SERIES_COLUMNS = ["date", "tag", "ret"]  # `tag` = libellé de la série : réutilise rebase_returns / tag_perf_stats


# --- Logique pure -------------------------------------------------------------------

def asset_returns(history: pd.DataFrame | None) -> pd.DataFrame:
    """Rendement par (date, symbole) entre deux snapshots consécutifs, hors achats/ventes.

    Même règle que `tag_returns` : l'actif doit être détenu au snapshot précédent, avec un prix
    valide aux deux dates. Colonnes : prev_date, date, symbol, kind, ret (fraction).
    """
    cols = ["prev_date", "date", "symbol", "kind", "ret"]
    if history is None or history.empty:
        return pd.DataFrame(columns=cols)

    rows, prev, prev_date = [], None, None
    for date, assets in zip(history["date"], history["assets"]):
        if not isinstance(assets, list):
            prev, prev_date = None, None
            continue
        cur = {a["symbol"]: (float(a.get("quantity") or 0.0), float(a.get("price_eur") or 0.0), a.get("kind"))
               for a in assets if a.get("kind") != CASH_KIND and a.get("symbol")}
        if prev is not None:
            for sym, (q0, p0, kind) in prev.items():
                p1 = cur.get(sym, (0.0, 0.0, None))[1]
                if q0 <= 0 or p0 <= 0 or p1 <= 0:
                    continue
                rows.append((prev_date, date, sym, kind, p1 / p0 - 1))
        prev, prev_date = cur, date
    return pd.DataFrame(rows, columns=cols)


def asset_catalog(a_ret: pd.DataFrame, df_all: pd.DataFrame) -> pd.DataFrame:
    """Actifs comparables (au moins une semaine de rendement) : symbol, kind, weeks, held.

    `held` = encore détenu aujourd'hui. Les actifs soldés restent comparables tant qu'ils sont
    dans l'historique.
    """
    cols = ["symbol", "kind", "weeks", "held"]
    if a_ret.empty:
        return pd.DataFrame(columns=cols)
    cat = (
        a_ret.groupby("symbol")
        .agg(kind=("kind", "last"), weeks=("ret", "size"))
        .reset_index()
    )
    cat["kind"] = cat["kind"].fillna("Autre")
    held = set(df_all.loc[(df_all["kind"] != CASH_KIND) & (df_all["value_eur"] > 0), "symbol"])
    cat["held"] = cat["symbol"].isin(held)
    return cat[cols].sort_values(["kind", "symbol"]).reset_index(drop=True)


def compare_series(
    history_dates, tag_ret: pd.DataFrame, a_ret: pd.DataFrame,
    tag: str, symbol: str, tag_label: str, asset_label: str,
) -> pd.DataFrame:
    """Séries du tag et de l'actif sur les semaines communes, prêtes pour `rebase_returns`.

    Colonnes : date, tag (libellé de la série), ret. Chaque série commence par un point d'ancrage
    (ret = 0) à la date du snapshot précédant la première semaine commune. Vide s'il n'y a aucune
    semaine commune.
    """
    empty = pd.DataFrame(columns=SERIES_COLUMNS)
    if tag_ret.empty or a_ret.empty:
        return empty

    t = tag_ret[(tag_ret["tag"] == tag) & (tag_ret["base_eur"] > 0)].drop_duplicates("date").set_index("date")["ret"]
    a = a_ret[a_ret["symbol"] == symbol].drop_duplicates("date").set_index("date")["ret"]
    common = t.index.intersection(a.index).sort_values()
    if common.empty:
        return empty

    dates = pd.DatetimeIndex(sorted(pd.to_datetime(pd.Series(list(history_dates))).unique()))
    pos = int(dates.searchsorted(common[0]))
    anchor = dates[pos - 1] if pos > 0 else common[0] - pd.Timedelta(days=7)

    frames = []
    for label, series in ((tag_label, t), (asset_label, a)):
        frames.append(pd.DataFrame({
            "date": [anchor, *common],
            "tag": label,
            "ret": [0.0, *series.loc[common].astype(float).tolist()],
        }))
    return pd.concat(frames, ignore_index=True)[SERIES_COLUMNS]


def relative_series(rebased: pd.DataFrame, tag_label: str, asset_label: str) -> pd.Series:
    """Écart d'indice (tag - actif), en points, par date. Les deux séries partagent les mêmes dates."""
    wide = rebased.pivot(index="date", columns="tag", values="index")
    return (wide[tag_label] - wide[asset_label]).dropna()


def relative_stats(rebased: pd.DataFrame, tag_label: str, asset_label: str, min_periods: int = MIN_PERIODS) -> dict:
    """Performance, écart, corrélation, bêta (tag sur actif) et part de semaines gagnées par le tag.

    Corrélation et bêta : rendements hebdomadaires, NaN sous `min_periods` semaines.
    Bêta = cov(tag, actif) / var(actif) : si l'actif varie de 1 %, le tag varie en moyenne de bêta %.
    """
    nan = float("nan")
    out = {"n_periods": 0, "perf_tag": nan, "perf_asset": nan, "diff": nan, "corr": nan, "beta": nan, "hit_rate": nan}
    if rebased.empty:
        return out

    last = rebased.sort_values("date").groupby("tag")["index"].last()
    out["perf_tag"] = float(last[tag_label] - 100)
    out["perf_asset"] = float(last[asset_label] - 100)
    out["diff"] = out["perf_tag"] - out["perf_asset"]

    d = rebased[rebased.duplicated("tag")]  # retire le 1er point de chaque série (ret forcé à 0)
    wide = d.pivot(index="date", columns="tag", values="ret").dropna()
    n = len(wide)
    out["n_periods"] = n
    if n:
        out["hit_rate"] = float((wide[tag_label] > wide[asset_label]).mean() * 100)
    if n >= min_periods:
        out["corr"] = float(wide[tag_label].corr(wide[asset_label]))
        var = float(wide[asset_label].var())
        if var > 0:
            out["beta"] = float(wide[tag_label].cov(wide[asset_label]) / var)
    return out


# --- Figures ------------------------------------------------------------------------

def comparison_figure(rebased: pd.DataFrame, tag_label: str, asset_label: str) -> go.Figure:
    """Les deux courbes en base 100 au début de la fenêtre."""
    fig = go.Figure()
    for label, color in ((tag_label, TAG_COLOR), (asset_label, ASSET_COLOR)):
        g = rebased[rebased["tag"] == label]
        fig.add_scatter(x=g["date"], y=g["index"], name=label, mode="lines",
                        line=dict(color=color, width=2.5), hovertemplate="%{y:.1f}")
    fig.add_hline(y=100, line_dash="dot", line_color="#555B66")
    fig.update_layout(
        title="Performance comparée (base 100)", height=420, margin=MARGIN, hovermode="x unified",
        legend=dict(orientation="h", y=1.12, x=0),
    )
    return fig


def difference_figure(diff: pd.Series, tag_label: str, asset_label: str) -> go.Figure:
    """Écart d'indice tag - actif : vert quand le tag mène, rouge quand il est derrière."""
    colors = [GAIN_COLOR if v >= 0 else LOSS_COLOR for v in diff]
    fig = go.Figure(go.Bar(x=diff.index, y=diff.values, marker_color=colors, hovertemplate="%{y:+.1f} pts"))
    fig.update_layout(
        title=f"Écart : {tag_label} − {asset_label} (points d'indice)", height=280, margin=MARGIN,
        showlegend=False, yaxis=dict(ticksuffix=" pts"),
    )
    return fig


# --- Rendu Streamlit -----------------------------------------------------------------

def render_compare_page() -> None:
    st.title("⚖️ Comparer un tag à un actif")

    data, df_all = load_portfolio()
    assets_data, _ = load_synced_assets(df_all)
    df_all = attach_tags(df_all, assets_data)

    try:
        history = load_history()
    except ValueError as e:
        st.warning(str(e))
        return
    if history is None or history.empty:
        st.info("history.json introuvable ou vide : lance d'abord le pipeline Rust (`cargo run`).")
        return

    summary = tag_summary(df_all)
    sym_tags = symbol_tags_map(df_all)
    tag_ret = tag_returns(history, sym_tags)
    a_ret = asset_returns(history)
    catalog = asset_catalog(a_ret, df_all)
    if summary.empty or catalog.empty:
        st.info("Pas assez d'historique par actif pour comparer (il faut au moins deux snapshots).")
        return

    # --- Choix ---
    with st.container(border=True):
        c1, c2, c3 = st.columns([2, 1, 2])
        tag = c1.selectbox("Tag", list(summary["tag"]), help="Triés par valeur décroissante.")
        kinds = ["Tous"] + sorted(catalog["kind"].unique())
        kind = c2.selectbox("Type d'actif", kinds)
        pool = catalog if kind == "Tous" else catalog[catalog["kind"] == kind]
        info = pool.set_index("symbol")
        symbol = c3.selectbox(
            "Actif", list(pool["symbol"]),
            format_func=lambda s: f"{s} · {info.loc[s, 'kind']}" + ("" if info.loc[s, "held"] else " (soldé)"),
        )
        period = st.radio("Période", list(PERIODS), index=len(PERIODS) - 1, horizontal=True)

    tag_label, asset_label = f"Tag · {tag}", f"Actif · {symbol}"
    combined = compare_series(history["date"], tag_ret, a_ret, tag, symbol, tag_label, asset_label)
    if combined.empty:
        st.warning("Aucune semaine commune : le tag et l'actif n'ont jamais été détenus en même temps.")
        return

    days = PERIODS[period]
    start = None if days is None else combined["date"].max() - pd.Timedelta(days=days)
    rebased = rebase_returns(combined, start)
    stats = relative_stats(rebased, tag_label, asset_label)
    if stats["n_periods"] < 1:
        st.warning("Pas assez de semaines communes sur cette période : élargis la période.")
        return

    if symbol in set(positions_of_tag(df_all, tag)["symbol"]):
        st.warning("Cet actif fait partie du tag : la corrélation et le bêta sont mécaniquement élevés.")

    # --- KPIs ---
    perf = tag_perf_stats(rebased).set_index("tag")
    vol_t, vol_a = perf.loc[tag_label, "vol_pct"], perf.loc[asset_label, "vol_pct"]
    dd_t, dd_a = perf.loc[tag_label, "max_dd_pct"], perf.loc[asset_label, "max_dd_pct"]

    with st.container(border=True):
        k1, k2, k3, k4 = st.columns(4)
        k1.metric(f"Performance · {tag}", fmt_pct(stats["perf_tag"], 2),
                  delta=f"{fmt_number(stats['diff'], 1, signed=True)} pts vs l'actif",
                  help="Rendement cumulé de la période, hors achats/ventes.")
        k2.metric(f"Performance · {symbol}", fmt_pct(stats["perf_asset"], 2))
        k3.metric("Corrélation (hebdo)", fmt_number(stats["corr"], 2),
                  help=f"Corrélation des rendements hebdomadaires ; « — » sous {MIN_PERIODS} semaines communes.")
        k4.metric("Bêta du tag / actif", fmt_number(stats["beta"], 2),
                  help="Si l'actif varie de 1 %, le tag varie en moyenne de bêta %. Sous 1 : le tag amortit l'actif.")

        k5, k6, k7, k8 = st.columns(4)
        k5.metric("Volatilité annualisée (tag / actif)",
                  f"{fmt_pct(vol_t, 1, signed=False)} / {fmt_pct(vol_a, 1, signed=False)}")
        k6.metric("Drawdown max (tag / actif)",
                  f"{fmt_pct(dd_t, 1, signed=False)} / {fmt_pct(dd_a, 1, signed=False)}")
        k7.metric("Semaines où le tag fait mieux", fmt_pct(stats["hit_rate"], 0, signed=False))
        k8.metric("Semaines comparées", fmt_number(stats["n_periods"]))

    # --- Graphiques ---
    st.plotly_chart(comparison_figure(rebased, tag_label, asset_label), use_container_width=True)
    st.plotly_chart(difference_figure(relative_series(rebased, tag_label, asset_label), tag_label, asset_label),
                    use_container_width=True)
    st.caption(
        "Rendements hebdomadaires hors achats/ventes (même méthode que « Vue par tag »). Seules les semaines "
        "où le tag et l'actif sont tous deux détenus sont comparées ; les deux courbes partent de 100 à la même date."
    )
