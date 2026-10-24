"""Page Tags : gestion et assignation des Tags aux actifs du portefeuille."""
from __future__ import annotations

import streamlit as st

from portfolio.assets import load_synced_assets, save_assets_data, update_asset
from portfolio.data import build_assets_df, load_data
from portfolio.theme import apply_theme

apply_theme()
st.title("🏷️ Gestion des Tags")

# Positions (dashboard.json) + actifs suivis (où sont stockés les tags), nouveaux actifs ajoutés
df_portfolio = build_assets_df(load_data())
assets_data, added = load_synced_assets(df_portfolio)
if added > 0:
    st.toast(f"{added} nouvel(s) actif(s) ajouté(s) pour taggage.", icon="✅")

# --- Récupérer tous les tags existants ---
all_tags = sorted(set(tag for a in assets_data["assets"] for tag in a.get("tags", [])))

# ==========================================
# NOUVEAU : GESTION GLOBALE (SUPPRESSION)
# ==========================================
st.subheader("🧹 Gestion globale des tags")
if not all_tags:
    st.info("Aucun tag n'existe encore dans le système.")
else:
    col1, col2 = st.columns([3, 1])
    tag_to_delete = col1.selectbox("Sélectionner un tag à supprimer de TOUS les actifs", all_tags)
    if col2.button("🗑️ Supprimer définitivement", type="primary"):
        if tag_to_delete:
            count = 0
            # On boucle sur tous les actifs pour retirer ce tag s'il est présent
            for a in assets_data["assets"]:
                if tag_to_delete in a.get("tags", []):
                    a["tags"].remove(tag_to_delete)
                    count += 1
            
            save_assets_data(assets_data)
            st.success(f"Le tag '{tag_to_delete}' a été retiré de {count} actif(s).")
            st.rerun() # Rafraîchit la page pour mettre à jour la liste des tags

st.divider()

# ==========================================
# GESTION PAR ACTIF (INCHANGÉE + CORRIGÉE)
# ==========================================
st.subheader("✏️ Tags par actif")

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

st.markdown(f"##### Configuration pour {selected_asset['name']}")
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
new_tags_list = list(dict.fromkeys(chosen_existing))  # sans doublons, ordre conservé
if new_tag.strip():
    new_tags_list.append(new_tag.strip())

st.write("**Tags actuels pour cet actif :**", ", ".join(new_tags_list) if new_tags_list else "Aucun")

if st.button("💾 Sauvegarder les tags", type="primary"):
    # Mettre à jour
    update_asset(assets_data, selected_asset["ticker"], tags=new_tags_list)
    save_assets_data(assets_data)
    st.success("Tags sauvegardés avec succès !")
    st.rerun()
