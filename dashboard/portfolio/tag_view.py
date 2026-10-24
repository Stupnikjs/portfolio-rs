"""Rendu de la page d'accueil articulée autour des tags : vue d'ensemble puis détail d'un tag."""
from __future__ import annotations

import pandas as pd
import plotly.express as px
import plotly.graph_objects as go
import streamlit as st

from portfolio.home import render_positions_table
from portfolio.tag_stats import positions_of_tag, trades_by_month
from portfolio.theme import GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

BUY_COLOR = PASTEL_DARK_SEQUENCE[0]
SELL_COLOR = PASTEL_DARK_SEQUENCE[2]
COLOR_MAP = {"Gain": GAIN_COLOR, "Perte": LOSS_COLOR}
MARGIN = dict(l=10, r=10, t=40, b=10)


# --- Vue d'ensemble : tous les tags côte à côte --------------------------------------

def render_tag_overview(summary: pd.DataFrame) -> None:
    st.subheader("🏷️ Vue d'ensemble par tag")
    st.caption("Un actif à plusieurs tags compte dans chacun d'eux : les lignes ne s'additionnent pas.")

    c1, c2, c3 = st.columns(3)

    fig_alloc = px.pie(summary, values="value", names="tag", hole=0.4, title="Poids par tag (valeur)")
    fig_alloc.update_traces(textposition="inside", textinfo="percent+label")
    fig_alloc.update_layout(showlegend=False, margin=MARGIN)
    c1.plotly_chart(fig_alloc, use_container_width=True)

    fig_pnl = px.bar(
        summary, x="tag", y="pnl_total", color="pnl_color", color_discrete_map=COLOR_MAP,
        labels={"pnl_total": "P&L total (€)", "tag": "Tag"}, title="P&L total par tag (€)",
    )
    fig_pnl.update_layout(showlegend=False, margin=MARGIN)
    c2.plotly_chart(fig_pnl, use_container_width=True)

    by_perf = summary.sort_values("perf_pct", ascending=False)
    fig_perf = px.bar(
        by_perf, x="tag", y="perf_pct", color="pnl_color", color_discrete_map=COLOR_MAP,
        labels={"perf_pct": "Perf. totale (%)", "tag": "Tag"}, title="Performance par tag (%)",
    )
    fig_perf.update_layout(showlegend=False, margin=MARGIN, yaxis=dict(ticksuffix=" %"))
    c3.plotly_chart(fig_perf, use_container_width=True)

    st.dataframe(
        summary[["tag", "positions", "value", "weight_pct", "cost_basis", "pnl_latent", "latent_pct",
                 "pnl_realized", "pnl_total", "invested", "perf_pct"]],
        hide_index=True, use_container_width=True,
        column_config={
            "tag": "Tag",
            "positions": st.column_config.NumberColumn("Positions", format="%d"),
            "value": st.column_config.NumberColumn("Valeur", format="%.2f €"),
            "weight_pct": st.column_config.NumberColumn("Poids", format="%.1f %%"),
            "cost_basis": st.column_config.NumberColumn(
                "Capital engagé", format="%.2f €", help="Cost basis des lots encore détenus."),
            "pnl_latent": st.column_config.NumberColumn("P&L latent", format="%+.2f €"),
            "latent_pct": st.column_config.NumberColumn(
                "Latent (%)", format="%+.2f %%", help="P&L latent / capital engagé."),
            "pnl_realized": st.column_config.NumberColumn("P&L réalisé", format="%+.2f €"),
            "pnl_total": st.column_config.NumberColumn("P&L total", format="%+.2f €"),
            "invested": st.column_config.NumberColumn(
                "Total investi", format="%.2f €", help="Cumulé : lots détenus + lots déjà vendus."),
            "perf_pct": st.column_config.NumberColumn(
                "Perf. totale", format="%+.2f %%", help="P&L total / total investi (cumulé)."),
        },
    )


# --- Évolution dans le temps : tous les tags ---------------------------------------------

def render_tags_history(hist: pd.DataFrame) -> None:
    st.subheader("📈 Évolution par tag")
    if hist.empty:
        st.info("Pas d'historique par actif (history.json absent ou sans détail : "
                "relance `cargo run -- --rebuild-history`).")
        return

    metric = st.radio(
        "Mesure", ["Valeur (€)", "P&L latent (%)"], horizontal=True, label_visibility="collapsed",
    )
    y, suffix = ("value_eur", " €") if metric == "Valeur (€)" else ("pnl_pct", " %")
    fig = px.line(hist, x="date", y=y, color="tag", labels={y: metric, "date": "", "tag": "Tag"})
    fig.update_layout(height=420, margin=MARGIN, hovermode="x unified",
                      yaxis=dict(ticksuffix=suffix), legend=dict(orientation="h", y=1.1, x=0))
    st.plotly_chart(fig, use_container_width=True)
    st.caption("P&L latent (%) = (valeur − cost basis) / cost basis des lots ouverts à chaque date.")


# --- Détail d'un tag --------------------------------------------------------------------

def render_tag_detail(
    tag: str,
    df_all: pd.DataFrame,
    summary: pd.DataFrame,
    hist: pd.DataFrame,
    trades: pd.DataFrame,
    min_value_eur: float,
) -> None:
    """KPIs, allocation, P&L par actif, historique, activité et positions d'un tag."""
    row = summary[summary["tag"] == tag].iloc[0]
    positions = positions_of_tag(df_all, tag)
    held = positions[positions["value_eur"] >= min_value_eur]            # allocation : sans poussière
    perf = positions[(positions["value_eur"] >= min_value_eur) | (positions["realized_pnl_eur"].abs() > 0.01)]

    st.subheader(f"🔎 Détail : {tag}")

    k1, k2, k3, k4 = st.columns(4)
    k1.metric("Valeur", f"{row['value']:,.2f} €", f"{row['weight_pct']:.1f} % du portefeuille", delta_color="off")
    k2.metric("Capital engagé", f"{row['cost_basis']:,.2f} €",
              help=f"Cost basis des lots détenus. Total investi cumulé (lots vendus inclus) : {row['invested']:,.2f} €.")
    k3.metric("P&L total", f"{row['pnl_total']:+,.2f} €", f"{row['perf_pct']:+.2f} %")
    k4.metric("Positions", f"{int(row['positions'])}")

    k5, k6, k7, _ = st.columns(4)
    k5.metric("P&L latent", f"{row['pnl_latent']:+,.2f} €", f"{row['latent_pct']:+.2f} %")
    k6.metric("P&L réalisé", f"{row['pnl_realized']:+,.2f} €")
    if not perf.empty:
        best = perf.loc[perf["pnl_total_pct"].idxmax()]
        k7.metric("Meilleur actif", best["symbol"], f"{best['pnl_total_pct']:+.1f} %")

    left, right = st.columns(2)
    if not held.empty:
        fig_pie = px.pie(held, values="value_eur", names="symbol", hole=0.4, title=f"Répartition dans « {tag} »")
        fig_pie.update_traces(textposition="inside", textinfo="percent+label")
        fig_pie.update_layout(showlegend=False, margin=MARGIN)
        left.plotly_chart(fig_pie, use_container_width=True)
    if not perf.empty:
        fig_bar = px.bar(
            perf.sort_values("pnl_total_eur", ascending=False), x="symbol", y="pnl_total_eur",
            color="pnl_color", color_discrete_map=COLOR_MAP,
            labels={"pnl_total_eur": "P&L total (€)", "symbol": "Actif"}, title="P&L total par actif (€)",
        )
        fig_bar.update_layout(showlegend=False, margin=MARGIN)
        right.plotly_chart(fig_bar, use_container_width=True)

    # --- Historique du tag ---
    h = hist[hist["tag"] == tag]
    if len(h) >= 2:
        fig_h = go.Figure()
        fig_h.add_scatter(x=h["date"], y=h["value_eur"], name="Valeur", mode="lines",
                          line=dict(color=BUY_COLOR, width=2.5), hovertemplate="%{y:,.2f} €")
        fig_h.add_scatter(x=h["date"], y=h["cost_basis_eur"], name="Cost basis", mode="lines",
                          line=dict(color=SELL_COLOR, width=2, dash="dash"), hovertemplate="%{y:,.2f} €")
        fig_h.update_layout(title="Valeur et cost basis dans le temps", height=380, margin=MARGIN,
                            hovermode="x unified", yaxis=dict(ticksuffix=" €", rangemode="tozero"),
                            legend=dict(orientation="h", y=1.12, x=0))
        st.plotly_chart(fig_h, use_container_width=True)

    # --- Activité du tag ---
    t = trades[trades["tag"] == tag] if not trades.empty else trades
    monthly = trades_by_month(t)
    if not monthly.empty:
        fig_t = go.Figure()
        fig_t.add_bar(x=monthly["month"], y=monthly["bought"], name="Acheté", marker_color=BUY_COLOR)
        fig_t.add_bar(x=monthly["month"], y=monthly["sold"], name="Vendu", marker_color=SELL_COLOR)
        fig_t.update_layout(barmode="group", title=f"Activité mensuelle ({int(monthly['n_trades'].sum())} trades)",
                            height=340, margin=MARGIN, xaxis=dict(type="category"),
                            yaxis=dict(ticksuffix=" €"), legend=dict(orientation="h", y=1.12, x=0))
        st.plotly_chart(fig_t, use_container_width=True)

    render_positions_table(perf)