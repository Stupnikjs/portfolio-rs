"""KPIs en en-tête + graphiques d'allocation/P&L + tableau détaillé."""
from __future__ import annotations

import pandas as pd
import plotly.express as px
import streamlit as st

from data import format_display_df
from theme import GAIN_COLOR, LOSS_COLOR


def render_kpis(kpis: dict, global_kpis: dict | None = None) -> None:
    st.title("📈 Mon Portefeuille")

    col1, col2, col3, col4, col5 = st.columns(5)
    col1.metric("Valeur Totale", f"{kpis['total_value_eur']:,.2f} €")
    col2.metric("Cost Basis", f"{kpis['total_cost_basis_eur']:,.2f} €")
    col3.metric("P&L Latent", f"{kpis['total_pnl_latent_eur']:,.2f} €")
    col4.metric("P&L Réalisé", f"{kpis['total_pnl_realized_eur']:,.2f} €")
    col5.metric(
        "P&L Total",
        f"{kpis['total_pnl_eur']:,.2f} €",
        help="Latent + réalisé",
    )

    if global_kpis is not None:
        g1, g2, g3 = st.columns(3)
        g1.metric("Titres + crypto + cash", f"{global_kpis['equity_eur']:,.2f} €")
        g2.metric(
            "Cash XTB disponible",
            f"{global_kpis['cash_eur']:,.2f} €",
            f"{global_kpis['cash_share'] * 100:.0f} % du total",
            delta_color="off",
        )
        ret = global_kpis["return_pct"]
        g3.metric(
            "Performance globale (cash inclus)",
            f"{ret:+.2f} %" if ret is not None else "n/a",
            f"{global_kpis['gain_eur']:+,.2f} €",
            help="(P&L latent + réalisé + dividendes nets) / (cost basis des lots ouverts + cash XTB). "
                 "Le cash Binance n'est pas suivi.",
        )
    else:
        invested = kpis.get("total_invested_eur") or kpis["total_cost_basis_eur"]
        pnl_pct_global = kpis["total_pnl_eur"] / invested * 100 if invested > 0 else 0
        st.metric("Performance Globale (hors cash)", f"{pnl_pct_global:+.2f} %")

def render_allocation_and_pnl(df: pd.DataFrame, df_perf: pd.DataFrame | None = None) -> None:
    df_perf = df if df_perf is None else df_perf
    col_left, col_right = st.columns(2)

    with col_left:
        st.subheader("🥧 Allocation du Portefeuille")
        fig_pie = px.pie(
            df,
            values="value_eur",
            names="symbol",
            hole=0.4,
        )
        fig_pie.update_traces(textposition="inside", textinfo="percent+label")
        fig_pie.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
        st.plotly_chart(fig_pie, use_container_width=True)

        # --- Export CSV : ticker + % d'allocation uniquement (pas de prix) ---
        total_value = float(df["value_eur"].sum())
        alloc_df = (
            df[["symbol", "value_eur"]]
            .assign(allocation_pct=lambda d: d["value_eur"] / total_value * 100 if total_value > 0 else 0.0)
            .drop(columns="value_eur")
            .rename(columns={"symbol": "ticker"})
            .sort_values("allocation_pct", ascending=False)
            .reset_index(drop=True)
        )
        # Arrondi pour éviter les bruits flottants
        alloc_df["allocation_pct"] = alloc_df["allocation_pct"].round(4)

        st.download_button(
            label="📥 Télécharger l'allocation (CSV)",
            data=alloc_df.to_csv(index=False).encode("utf-8"),
            file_name="allocation.csv",
            mime="text/csv",
            help="Ticker et pourcentage d'allocation uniquement — aucune donnée de prix.",
        )

    with col_right:
        st.subheader("📊 P&L total par Actif (EUR, latent + réalisé)")
        fig_bar = px.bar(
            df_perf,
            x="symbol",
            y="pnl_total_eur",
            color="pnl_color",
            color_discrete_map={"Gain": GAIN_COLOR, "Perte": LOSS_COLOR},
            labels={"pnl_total_eur": "P&L total (€)", "symbol": "Actif"},
        )
        fig_bar.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
        st.plotly_chart(fig_bar, use_container_width=True)

def render_positions_table(df: pd.DataFrame) -> None:
    st.subheader("📋 Détail des positions")
    df_display = format_display_df(df)
    st.dataframe(
        df_display[["Symbole", "Type", "Quantité", "Prix", "Valeur", "Cost Basis",
                    "P&L latent", "P&L réalisé", "P&L total", "Perf. totale (%)"]],
        use_container_width=True,
        hide_index=True,
    )