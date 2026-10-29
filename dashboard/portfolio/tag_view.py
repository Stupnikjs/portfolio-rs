"""Rendu de la page d'accueil articulée autour des tags : vue d'ensemble puis détail d'un tag."""
from __future__ import annotations

import pandas as pd
import plotly.express as px
import plotly.graph_objects as go
import streamlit as st

from portfolio.history import PERIODS
from portfolio.home import render_positions_table
from portfolio.tag_stats import (
    positions_of_tag, rebase_returns, tag_contribution, tag_correlation, tag_perf_stats, trades_by_month,
)
from portfolio.theme import GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

BUY_COLOR = PASTEL_DARK_SEQUENCE[0]
SELL_COLOR = PASTEL_DARK_SEQUENCE[2]
COLOR_MAP = {"Gain": GAIN_COLOR, "Perte": LOSS_COLOR}
MARGIN = dict(l=10, r=10, t=40, b=10)


# --- Vue d'ensemble : tous les tags côte à côte --------------------------------------

def render_tag_overview(summary: pd.DataFrame, overlap: pd.DataFrame | None = None) -> None:
    st.subheader("🏷️ Vue d'ensemble par tag")
    st.caption("Un actif à plusieurs tags compte dans chacun d'eux : les lignes ne s'additionnent pas.")

    c1, c2, c3 = st.columns(3)

    by_weight = summary.sort_values("weight_pct")
    fig_alloc = px.bar(
        by_weight, x="weight_pct", y="tag", orientation="h",
        labels={"weight_pct": "% du portefeuille", "tag": ""}, title="Poids par tag (% du portefeuille)",
    )
    fig_alloc.update_traces(texttemplate="%{x:.1f} %", textposition="outside", cliponaxis=False)
    fig_alloc.update_layout(showlegend=False, margin=MARGIN, xaxis=dict(ticksuffix=" %"))
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

    if overlap is not None and not overlap.empty:
        with st.expander("🔀 Chevauchement entre tags"):
            st.caption("Lecture : la cellule (ligne A, colonne B) = part de la valeur de A portée aussi par le tag B.")
            fig_ov = px.imshow(
                overlap, text_auto=".0f", aspect="auto", zmin=0, zmax=100,
                color_continuous_scale="Blues", labels=dict(x="Aussi tagué", y="Tag", color="%"),
            )
            fig_ov.update_layout(margin=MARGIN, coloraxis_showscale=False)
            st.plotly_chart(fig_ov, use_container_width=True)


# --- Évolution dans le temps : tous les tags ---------------------------------------------

def render_tags_history(hist: pd.DataFrame, returns: pd.DataFrame) -> None:
    """Comparaison dans le temps : performance base 100 (hors flux), valeur ou P&L latent, par tag."""
    st.subheader("📈 Évolution par tag")
    if hist.empty:
        st.info("Pas d'historique par actif (history.json absent ou sans détail : "
                "relance `cargo run -- --rebuild-history`).")
        return

    measures = {
        "Performance (base 100)": None,
        "Valeur (€)": ("value_eur", " €"),
        "P&L latent (€)": ("pnl_eur", " €"),
        "P&L latent (%)": ("pnl_pct", " %"),
    }
    if returns.empty:
        measures.pop("Performance (base 100)")

    c1, c2 = st.columns([3, 2])
    metric = c1.radio("Mesure", list(measures), horizontal=True)
    period = c2.radio("Période", list(PERIODS), index=len(PERIODS) - 1, horizontal=True)

    all_tags = list(hist.sort_values("date").groupby("tag")["value_eur"].last().sort_values(ascending=False).index)
    sel = st.multiselect("Tags à comparer", all_tags, default=all_tags[:6],
                         help="Par défaut : les 6 plus gros tags. Un actif à plusieurs tags compte dans chacun.")
    if not sel:
        st.info("Sélectionne au moins un tag.")
        return

    days = PERIODS[period]
    start = None if days is None else hist["date"].max() - pd.Timedelta(days=days)

    if measures[metric] is None:
        rb = rebase_returns(returns[returns["tag"].isin(sel)], start)
        if rb.empty:
            st.info("Pas assez de points sur cette période.")
            return
        fig = px.line(rb, x="date", y="index", color="tag", labels={"index": "Base 100", "date": "", "tag": "Tag"})
        fig.add_hline(y=100, line_dash="dot", line_color="#888")
        fig.update_traces(hovertemplate="%{y:.1f}")
        fig.update_layout(height=420, margin=MARGIN, hovermode="x unified", legend=dict(orientation="h", y=1.1, x=0))
        st.plotly_chart(fig, use_container_width=True)
        st.caption("Performance des positions détenues au début de chaque période : les achats et ventes "
                   "n'y changent rien. Chaque tag démarre à 100 au début de la période choisie.")

        stats = tag_perf_stats(rb)
        st.dataframe(
            stats, hide_index=True, use_container_width=True,
            column_config={
                "tag": "Tag",
                "perf_pct": st.column_config.NumberColumn("Perf. période", format="%+.2f %%"),
                "vol_pct": st.column_config.NumberColumn(
                    "Volatilité (ann.)", format="%.1f %%", help="Affichée à partir de 8 points."),
                "max_dd_pct": st.column_config.NumberColumn(
                    "Drawdown max", format="%.1f %%", help="Pire repli depuis un plus haut, sur la période."),
                "points": st.column_config.NumberColumn("Points", format="%d"),
            },
        )

        # --- Contribution à la performance du portefeuille ---
        contrib, port_perf = tag_contribution(returns, start)
        contrib = contrib[contrib["tag"].isin(sel)]
        if not contrib.empty:
            contrib = contrib.assign(color=contrib["contrib_pts"].map(lambda x: "Gain" if x >= 0 else "Perte"))
            fig_c = px.bar(
                contrib, x="tag", y="contrib_pts", color="color", color_discrete_map=COLOR_MAP,
                labels={"contrib_pts": "Points de perf.", "tag": "Tag"},
                title=f"Contribution à la performance du portefeuille ({port_perf:+.2f} % sur la période)",
            )
            fig_c.update_traces(texttemplate="%{y:+.2f}", textposition="outside", cliponaxis=False)
            fig_c.update_layout(showlegend=False, margin=MARGIN, height=340)
            st.plotly_chart(fig_c, use_container_width=True)
            st.caption("Gain du tag rapporté à la valeur de TOUT le portefeuille, période par période (somme "
                       "approximative). Un actif à plusieurs tags contribue à chacun : les barres ne s'additionnent pas.")

        # --- Corrélation entre tags ---
        with st.expander("🔗 Corrélation entre tags"):
            corr = tag_correlation(rb)
            if corr.empty:
                st.caption("Pas assez de points communs (8 périodes minimum) ou moins de 2 tags sélectionnés : "
                           "choisis une période plus longue.")
            else:
                fig_k = px.imshow(corr, text_auto=".2f", aspect="auto", zmin=-1, zmax=1,
                                  color_continuous_scale="RdBu", labels=dict(color="Corr."))
                fig_k.update_layout(margin=MARGIN, coloraxis_showscale=False)
                st.plotly_chart(fig_k, use_container_width=True)
                st.caption("Corrélation des rendements périodiques. Deux tags qui partagent des actifs sont "
                           "corrélés mécaniquement : à lire avec la matrice de chevauchement.")
        return

    y, suffix = measures[metric]
    h = hist[hist["tag"].isin(sel)]
    if start is not None:
        h = h[h["date"] >= start]
    fig = px.line(h, x="date", y=y, color="tag", labels={y: metric, "date": "", "tag": "Tag"})
    fig.update_layout(height=420, margin=MARGIN, hovermode="x unified",
                      yaxis=dict(ticksuffix=suffix), legend=dict(orientation="h", y=1.1, x=0))
    st.plotly_chart(fig, use_container_width=True)
    st.caption("La valeur en € bouge aussi avec vos achats/ventes ; P&L latent (%) = (valeur − cost basis) / "
               "cost basis des lots ouverts. Pour comparer les tags à armes égales, préfère « Performance (base 100) ».")


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