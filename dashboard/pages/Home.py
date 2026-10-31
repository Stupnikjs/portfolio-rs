"""Page d'accueil : vue globale du portefeuille, sans tags.

La vue par tag est dans la page « Vue par tag », leur gestion dans « Gestion Tags ».
"""
from __future__ import annotations

import streamlit as st

from portfolio.config import MIN_POSITION_VALUE_EUR
from portfolio.data import (
    CASH_KIND, compute_global_kpis, compute_kpis, load_portfolio, small_positions_summary,
)
from portfolio.home import render_allocation, render_kpis, render_pnl_by_asset, render_positions_table
from portfolio.theme import apply_theme

apply_theme()  # doit être appelé avant tout autre st.* ou px.*

data, df_all = load_portfolio()

# --- En-tête : KPIs du portefeuille complet ---
render_kpis(compute_kpis(df_all), compute_global_kpis(df_all, data))

# `df_all` garde toutes les positions pour les totaux (une position soldée porte du P&L réalisé).
# Le seuil de poussière ne sert qu'à l'affichage.
n_dust, dust_value = small_positions_summary(df_all, MIN_POSITION_VALUE_EUR)
if n_dust:
    st.sidebar.caption(
        f"{n_dust} position(s) < {MIN_POSITION_VALUE_EUR:.0f} € masquée(s) des graphiques "
        f"({dust_value:,.2f} € au total). Les totaux les incluent."
    )

scope = df_all[df_all["kind"] != CASH_KIND]
df_view = scope[scope["value_eur"] >= MIN_POSITION_VALUE_EUR]  # allocation : sans poussière
# P&L et tableau : on garde aussi les positions soldées qui ont du P&L réalisé.
df_perf = scope[(scope["value_eur"] >= MIN_POSITION_VALUE_EUR) | (scope["realized_pnl_eur"].abs() > 0.01)]

if df_view.empty:
    st.info("Aucune position à afficher.")
    st.stop()

tab_alloc, tab_perf, tab_positions = st.tabs(["🥧 Allocation", "📊 Performance", "📋 Positions"])

with tab_alloc:
    render_allocation(df_view)

with tab_perf:
    render_pnl_by_asset(df_perf)

with tab_positions:
    # Le poids est calculé sur tout le portefeuille (poussière comprise), pas sur les seules lignes affichées.
    render_positions_table(df_perf, weight_base=float(scope["value_eur"].sum()))
