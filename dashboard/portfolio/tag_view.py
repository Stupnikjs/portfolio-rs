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