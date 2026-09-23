"""Point d'entrée du dashboard. Lancer avec : streamlit run app.py"""
from __future__ import annotations

import streamlit as st

from theme import apply_theme
from data import load_data, build_assets_df, compute_kpis
from charts import render_kpis, render_allocation_and_pnl, render_positions_table
from correlation import render_correlation_section

apply_theme()  # doit être appelé avant tout autre st.* ou px.*

data = load_data()
df = build_assets_df(data)
kpis = compute_kpis(df, realized_pnl_eur=data.get("realized_pnl_eur", 0.0))

render_kpis(kpis)

st.divider()

render_allocation_and_pnl(df)

st.divider()
render_positions_table(df)

st.divider()
render_correlation_section(df, data)
