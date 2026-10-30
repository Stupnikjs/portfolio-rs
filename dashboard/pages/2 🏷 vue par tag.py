"""Page Vue par Tag : statistiques et graphiques par tag (la gestion des tags est dans Gestion Tags)."""
from __future__ import annotations

import streamlit as st

from portfolio.assets import attach_tags, load_synced_assets
from portfolio.config import MIN_POSITION_VALUE_EUR
from portfolio.data import load_portfolio, small_positions_summary
from portfolio.history import load_history
from portfolio.tag_stats import (
    UNTAGGED, symbol_tags_map, tag_history, tag_overlap, tag_returns, tag_summary, tag_trades,
)
from portfolio.tag_view import render_tag_detail, render_tag_overview, render_tags_history
from portfolio.theme import apply_theme

apply_theme()  # doit être appelé avant tout autre st.* ou px.*
st.title("🏷️ Vue par Tag")

data, df_all = load_portfolio()
assets_data, _ = load_synced_assets(df_all)  # ajoute les nouveaux actifs détenus pour le taggage
df_all = attach_tags(df_all, assets_data)

# `df_all` garde toutes les positions pour les totaux (une position soldée porte du P&L réalisé).
# Le seuil de poussière ne sert qu'à l'affichage des graphiques d'allocation.
n_dust, dust_value = small_positions_summary(df_all, MIN_POSITION_VALUE_EUR)
if n_dust:
    st.sidebar.caption(
        f"{n_dust} position(s) < {MIN_POSITION_VALUE_EUR:.0f} € masquée(s) des graphiques "
        f"({dust_value:,.2f} € au total). Les totaux les incluent."
    )

summary = tag_summary(df_all)
if summary.empty:
    st.info("Aucune position à afficher.")
    st.stop()

# --- Données par tag ---
sym_tags = symbol_tags_map(df_all)
try:
    history = load_history()
except ValueError as e:
    st.warning(str(e))
    history = None
hist = tag_history(history, sym_tags)
returns = tag_returns(history, sym_tags)
trades = tag_trades(data.get("trades"), sym_tags)

# --- 1. Vue d'ensemble : tous les tags ---
render_tag_overview(summary, tag_overlap(df_all))
if UNTAGGED in set(summary["tag"]):
    st.caption(f"« {UNTAGGED} » regroupe les actifs non taggés : à compléter dans la page Gestion Tags.")

st.divider()

# --- 2. Évolution : tous les tags ---
render_tags_history(hist, returns)

st.divider()

# --- 3. Détail d'un tag ---
tag = st.selectbox("Tag à détailler", list(summary["tag"]), help="Triés par valeur décroissante.")
render_tag_detail(tag, df_all, summary, hist, trades, MIN_POSITION_VALUE_EUR)