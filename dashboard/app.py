"""Point d'entrée du dashboard. Lancer avec : streamlit run app.py"""
from __future__ import annotations

import streamlit as st

from theme import apply_theme
from data import load_data, build_assets_df
from charts import render_kpis, render_allocation_and_pnl, render_positions_table
from correlation import render_correlation_section

apply_theme()  # doit être appelé avant tout autre st.* ou px.*

data = load_data()
df = build_assets_df(data)

render_kpis(data)
st.divider()

render_allocation_and_pnl(df)

st.divider()
render_positions_table(df)

st.divider()
render_correlation_section(df, data)
