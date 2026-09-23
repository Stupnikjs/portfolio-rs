"""KPIs en en-tête + graphiques d'allocation/P&L + tableau détaillé."""
from __future__ import annotations

import pandas as pd
import plotly.express as px
import streamlit as st

from data import format_display_df
from theme import GAIN_COLOR, LOSS_COLOR


def render_kpis(kpis: dict) -> None:
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

    pnl_pct_global = (
        kpis["total_pnl_eur"] / kpis["total_cost_basis_eur"] * 100
        if kpis["total_cost_basis_eur"] > 0 else 0
    )
    st.metric("Performance Globale", f"{pnl_pct_global:+.2f} %")

def render_allocation_and_pnl(df: pd.DataFrame) -> None:
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

    with col_right:
        st.subheader("📊 P&L par Actif (EUR)")
        fig_bar = px.bar(
            df,
            x="symbol",
            y="pnl_eur",
            color="pnl_color",
            color_discrete_map={"Gain": GAIN_COLOR, "Perte": LOSS_COLOR},
            labels={"pnl_eur": "P&L (€)", "symbol": "Actif"},
        )
        fig_bar.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
        st.plotly_chart(fig_bar, use_container_width=True)


def render_positions_table(df: pd.DataFrame) -> None:
    st.subheader("📋 Détail des positions")
    df_display = format_display_df(df)
    st.dataframe(
        df_display[["Symbole", "Type", "Quantité", "Prix", "Valeur", "Cost Basis", "P&L (EUR)", "P&L (%)"]],
        use_container_width=True,
        hide_index=True,
    )
