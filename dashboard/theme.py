"""Thème sombre du dashboard.

Le thème Streamlit lui-même est piloté par `.streamlit/config.toml`
(base = "dark") -- c'est la manière officielle de faire, ça évite le CSS
fragile qui casse à chaque update de Streamlit.

Ce module gère la partie que `config.toml` ne couvre pas : les graphiques
Plotly, qui ont leur propre thème et restent blancs par défaut même si la
page Streamlit est sombre.
"""
from __future__ import annotations

import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st

# Palette cohérente avec .streamlit/config.toml
BG_COLOR = "#0E1117"
CARD_BG_COLOR = "#161B22"
TEXT_COLOR = "#E6E6E6"
GRID_COLOR = "#2A2E37"
GAIN_COLOR = "#26C281"
LOSS_COLOR = "#E74C3C"

PASTEL_DARK_SEQUENCE = [
    "#4C9AFF", "#26C281", "#F5B942", "#E74C3C",
    "#9B59B6", "#1ABC9C", "#F39C12", "#5DADE2",
]


def _build_dark_template() -> go.layout.Template:
    template = go.layout.Template()
    template.layout = go.Layout(
        paper_bgcolor=BG_COLOR,
        plot_bgcolor=BG_COLOR,
        font=dict(color=TEXT_COLOR),
        xaxis=dict(gridcolor=GRID_COLOR, zerolinecolor=GRID_COLOR),
        yaxis=dict(gridcolor=GRID_COLOR, zerolinecolor=GRID_COLOR),
        colorway=PASTEL_DARK_SEQUENCE,
        legend=dict(bgcolor="rgba(0,0,0,0)"),
    )
    return template


def apply_theme() -> None:
    """À appeler une fois, tout en haut de app.py, avant tout st.* ou px.*.

    Configure la page Streamlit et enregistre + active un template Plotly
    sombre nommé "portfolio_dark" pour tous les graphiques du dashboard.
    """
    st.set_page_config(page_title="Portfolio Dashboard", page_icon="📈", layout="wide")

    pio.templates["portfolio_dark"] = _build_dark_template()
    pio.templates.default = "portfolio_dark"
