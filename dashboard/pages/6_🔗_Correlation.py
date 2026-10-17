"""Page Corrélation : Matrice, Clustering et Variation sur les fenêtres temporelles."""
from __future__ import annotations

import streamlit as st
import pandas as pd
import plotly.graph_objects as go
import plotly.io as pio

from config import MIN_POSITION_VALUE_EUR
from data import load_data, build_assets_df, filter_small_positions, CASH_KIND
from correlation import render_correlation_section
from theme import BG_COLOR, TEXT_COLOR, GRID_COLOR

# Applique le template sombre pour cette page
if "portfolio_dark" not in pio.templates:
    pio.templates["portfolio_dark"] = dict(
        layout=dict(
            paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
            xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR),
        )
    )
pio.templates.default = "portfolio_dark"

st.title("🔗 Corrélations")

# --- Chargement des données (similaire à app.py) ---
data = load_data()
df = build_assets_df(data)
df = df[~((df["kind"] == CASH_KIND) & (df["symbol"] == "EUR"))].copy()
df = filter_small_positions(df, MIN_POSITION_VALUE_EUR)

if df.empty:
    st.info(f"Aucune position d'au moins {MIN_POSITION_VALUE_EUR:.0f} €.")
    st.stop()

# --- 1. Section classique (Matrice, par actif, clustering) ---
render_correlation_section(df, data)

st.divider()

# --- 2. NOUVEAU : Variation de la corrélation (90j, 6m, 1y) ---
st.subheader("📈 Variation de corrélation entre deux actifs")
st.caption("Compare le coefficient de corrélation de deux actifs sur les différentes fenêtres temporelles pour voir si leur relation se renforce ou s'affaiblit avec le temps.")

correlation_matrices = data.get("correlation_matrices", {})

# Le pipeline Rust calcule "90d", "6m" (180j) et "1y" (365j)
available_windows = [w for w in ["90d", "6m", "1y"] if w in correlation_matrices]

if not available_windows:
    st.info("Données de corrélation indisponibles. Lance le pipeline Rust (`cargo run`).")
else:
    # Récupérer tous les actifs présents dans les matrices
    all_assets = set()
    for w in available_windows:
        all_assets.update(correlation_matrices[w].keys())
    all_assets = sorted(list(all_assets))

    if len(all_assets) < 2:
        st.info("Pas assez d'actifs pour comparer.")
    else:
        col1, col2 = st.columns(2)
        asset_a = col1.selectbox("Actif A", all_assets, index=0)
        # Pour l'actif B, on met un autre actif par défaut si possible
        default_idx_b = 1 if len(all_assets) > 1 else 0
        asset_b = col2.selectbox("Actif B", all_assets, index=default_idx_b)

        if asset_a == asset_b:
            st.warning("Sélectionne deux actifs différents pour voir leur corrélation.")
        else:
            variations = []
            for w in available_windows:
                # La matrice est structurée comme { "90d": { "BTC": { "ETH": 0.8 } } }
                val = correlation_matrices[w].get(asset_a, {}).get(asset_b)
                if val is not None:
                    variations.append({"window": w, "correlation": val})
            
            if not variations:
                st.info(f"Corrélation non calculable entre {asset_a} et {asset_b} (données historiques insuffisantes).")
            else:
                df_var = pd.DataFrame(variations)
                
                # Ordre chronologique explicite pour l'axe X
                window_order = ["90d", "6m", "1y"]
                df_var['sort'] = df_var['window'].apply(lambda x: window_order.index(x))
                df_var = df_var.sort_values('sort')
                
                # Mapping pour un affichage plus clair
                labels_map = {"90d": "3 mois (90j)", "6m": "6 mois (180j)", "1y": "1 an (365j)"}
                df_var["label"] = df_var["window"].map(labels_map)

                fig_var = go.Figure(go.Scatter(
                    x=df_var["label"], 
                    y=df_var["correlation"],
                    mode="lines+markers+text",
                    text=[f"{v:.2f}" for v in df_var["correlation"]],
                    textposition="top center",
                    line=dict(color="#4C9AFF", width=3, shape="spline"),
                    marker=dict(size=10, color="#F5B942", line=dict(width=2, color=BG_COLOR))
                ))
                
                fig_var.update_layout(
                    title=f"Évolution de la corrélation : {asset_a} vs {asset_b}",
                    xaxis_title="Fenêtre temporelle",
                    yaxis_title="Coefficient de corrélation",
                    yaxis=dict(range=[-1.1, 1.1], dtick=0.5),
                    height=450,
                    margin=dict(l=20, r=20, t=60, b=20)
                )
                
                # Ligne horizontale à 0 pour bien voir les décorélations
                fig_var.add_hline(y=0, line_color=GRID_COLOR, line_width=1, line_dash="dash")
                
                st.plotly_chart(fig_var, use_container_width=True)
                
                st.markdown("""
                **Comment l'interpréter ?**
                - **Corrélation qui monte sur le long terme (1 an)** : Les actifs sont de plus en plus liés structurellement (ex: deux actions tech qui suivent le même secteur).
                - **Corrélation qui baisse sur le long terme** : Les actifs se décorrelent, c'est excellent pour la diversification de ton portefeuille.
                - **Corrélation négative** : L'actif B a tendance à monter quand l'actif A descend (couvre-risque).
                """)