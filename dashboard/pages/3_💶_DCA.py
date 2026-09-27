################################################################################
# Dossier : dashboard/pages
# Fichier : 3_💶_DCA.py
# Chemin relatif : pages/3_💶_DCA.py
################################################################################

"""Page 3 : DCA actif et Watchlist -- persistant dans data/assets.json,
totalement indépendant du pipeline Rust (dashboard.json) et de cash.json.

Réutilise telle quelle la logique déjà écrite dans assets_data.py /
assets_view.py (montants DCA modifiables, promotion/rétrogradation
watchlist <-> DCA, ajout d'un nouvel actif), et propose en plus un
préchargement en un clic des actifs détenus (dashboard.json, pipeline Rust)
absents de la watchlist."""
from __future__ import annotations

import plotly.io as pio
import streamlit as st

from assets_data import load_assets_data, save_assets_data, sync_watchlist_with_portfolio
from assets_view import render_assets_section
from data import CASH_KIND, build_assets_df, load_data, yfinance_ticker_for
from theme import BG_COLOR, TEXT_COLOR, GRID_COLOR

# Applique uniquement le template Plotly (set_page_config est déjà géré par app.py)
if "portfolio_dark" not in pio.templates:
    pio.templates["portfolio_dark"] = dict(
        layout=dict(
            paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
            xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR)
        )
    )
pio.templates.default = "portfolio_dark"

st.title("💶 DCA & Watchlist")

assets_data_ = load_assets_data()

# --- Préchargement des actifs détenus (portefeuille Rust) dans la watchlist ---
with st.expander("🔄 Précharger les actifs du portefeuille dans la watchlist", expanded=False):
    st.caption(
        "Ajoute en watchlist tout actif détenu dans dashboard.json mais absent "
        "de assets.json. N'écrase jamais un actif déjà présent (statut DCA, "
        "montant, analyse restent inchangés)."
    )
    if st.button("Précharger maintenant"):
        portfolio_data = load_data()
        portfolio_df = build_assets_df(portfolio_data)
        portfolio_df = portfolio_df[portfolio_df["kind"] != CASH_KIND]

        def _resolve(symbol: str) -> str:
            row = portfolio_df.loc[portfolio_df["symbol"] == symbol].iloc[0]
            return yfinance_ticker_for(row)

        added = sync_watchlist_with_portfolio(assets_data_, portfolio_df, ticker_resolver=_resolve)
        save_assets_data(assets_data_)
        if added:
            st.success(f"{added} actif(s) ajouté(s) à la watchlist.")
        else:
            st.info("Watchlist déjà à jour, rien à ajouter.")
        st.rerun()

st.divider()

render_assets_section(assets_data_)