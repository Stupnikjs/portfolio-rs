"""Thème sombre du dashboard.

Le thème Streamlit lui-même est piloté par `.streamlit/config.toml` (base = "dark").
Ce module gère ce que `config.toml` ne couvre pas : les graphiques Plotly, qui ont leur propre
thème et restent blancs par défaut même si la page Streamlit est sombre.
"""
from __future__ import annotations

import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st
from streamlit.errors import StreamlitAPIException

# Palette cohérente avec .streamlit/config.toml
BG_COLOR = "#0E1117"
SURFACE_COLOR = "#161A23"  # cartes, infobulles (= secondaryBackgroundColor dans config.toml)
TEXT_COLOR = "#E6E6E6"
GRID_COLOR = "#2A2E37"
GAIN_COLOR = "#26C281"
LOSS_COLOR = "#E74C3C"

PASTEL_DARK_SEQUENCE = [
    "#4C9AFF", "#26C281", "#F5B942", "#E74C3C",
    "#9B59B6", "#1ABC9C", "#F39C12", "#5DADE2",
]

FONT_FAMILY = '"Source Sans Pro", "Source Sans 3", sans-serif'
# Séparateurs français pour tous les graphiques : virgule décimale, espace insécable pour les milliers
# (même format que portfolio.fmt, utilisé pour les KPIs et les tableaux).
PLOTLY_SEPARATORS = ",\u00a0"


def _build_dark_template() -> go.layout.Template:
    template = go.layout.Template()
    template.layout = go.Layout(
        paper_bgcolor=BG_COLOR,
        plot_bgcolor=BG_COLOR,
        font=dict(color=TEXT_COLOR, family=FONT_FAMILY),
        title=dict(x=0, xanchor="left", font=dict(size=16)),
        xaxis=dict(gridcolor=GRID_COLOR, zerolinecolor=GRID_COLOR, automargin=True),
        yaxis=dict(gridcolor=GRID_COLOR, zerolinecolor=GRID_COLOR, automargin=True),
        colorway=PASTEL_DARK_SEQUENCE,
        legend=dict(bgcolor="rgba(0,0,0,0)"),
        hoverlabel=dict(
            bgcolor=SURFACE_COLOR, bordercolor=GRID_COLOR,
            font=dict(color=TEXT_COLOR, family=FONT_FAMILY),
        ),
        separators=PLOTLY_SEPARATORS,
    )
    return template


def apply_theme() -> None:
    """À appeler une fois, tout en haut de CHAQUE page, avant tout autre st.* ou px.*.

    Configure la page Streamlit et active le template Plotly sombre "portfolio_dark".
    Le routeur (app.py) l'appelle aussi avant `st.navigation` : un second appel de
    `st.set_page_config` depuis une page peut être refusé selon la version de Streamlit,
    on l'ignore alors (la configuration du routeur reste valable).
    """
    try:
        st.set_page_config(page_title="Portfolio Dashboard", page_icon="📈", layout="wide")
    except StreamlitAPIException:
        pass

    pio.templates["portfolio_dark"] = _build_dark_template()
    pio.templates.default = "portfolio_dark"
