"""Page 4 : Gestion et assignation des Tags aux actifs du portefeuille."""
from __future__ import annotations

import pandas as pd
import streamlit as st

from data import load_data, build_assets_df, yfinance_ticker_for
from assets_data import load_assets_data, save_assets_data, sync_watchlist_with_portfolio, update_asset

st.title("🏷️ Gestion des Tags")

# 1. Charger les données du portefeuille (dashboard.json)
data = load_data()
df_portfolio = build_assets_df(data)

# 2. Charger les données des actifs (où sont stockés les tags)
assets_data = load_assets_data()

# 3. Synchroniser : ajouter automatiquement les nouveaux actifs achetés
def resolver(symbol):
    row = df_portfolio[df_portfolio["symbol"] == symbol]
    if not row.empty:
        return yfinance_ticker_for(row.iloc[0])
    return symbol

added = sync_watchlist_with_portfolio(assets_data, df_portfolio, ticker_resolver=resolver)
if added > 0:
    save_assets_data(assets_data)
    st.toast(f"{added} nouvel(s) actif(s) ajouté(s) pour taggage.", icon="✅")

# --- Récupérer tous les tags existants ---
all_tags = sorted(set(tag for a in assets_data["assets"] for tag in a.get("tags", [])))

# --- Interface de sélection d'actif ---
# On priorise les actifs réellement détenus
current_tickers = set()
for _, row in df_portfolio.iterrows():
    if row["value_eur"] > 0:
        tk = row.get("ticker") or row["symbol"]
        current_tickers.add(tk)

assets_list = assets_data["assets"]
# Trier : détenus en premier, puis par ordre alphabétique
assets_list_sorted = sorted(assets_list, key=lambda x: (x["ticker"] not in current_tickers, x["name"]))

# Créer une liste de labels pour le selectbox
asset_options = [f"{a['name']} ({a['ticker']})" for a in assets_list_sorted]

selected_label = st.selectbox("Choisir un actif à tagger", options=asset_options)
selected_index = asset_options.index(selected_label)
selected_asset = assets_list_sorted[selected_index]

st.divider()

# --- Éditeur de tags pour l'actif sélectionné ---
st.subheader(f"Tags pour {selected_asset['name']}")
current_tags = selected_asset.get("tags", [])

# 1. Multiselect avec les tags existants (SUGGESTION)
chosen_existing = st.multiselect(
    "Sélectionner des tags existants", 
    options=all_tags, 
    default=[t for t in current_tags if t in all_tags]
)

# 2. Option pour créer un nouveau tag (AUTRE)
st.markdown("**Ou créer un nouveau tag :**")
new_tag = st.text_input("Nouveau tag (laisser vide pour ignorer)", placeholder="Ex: Tech, Dividende...")

# Combiner les tags existants choisis et le nouveau tag
new_tags_list = list(set(chosen_existing)) # On enlève les doublons
if new_tag.strip():
    new_tags_list.append(new_tag.strip())

st.write("**Tags actuels pour cet actif :**", ", ".join(new_tags_list) if new_tags_list else "Aucun")

if st.button("💾 Sauvegarder les tags", type="primary"):
    # Mettre à jour
    update_asset(assets_data, selected_asset["ticker"], tags=new_tags_list)
    save_assets_data(assets_data)
    st.success("Tags sauvegardés avec succès !")
    st.rerun()