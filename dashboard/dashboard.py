import json
from pathlib import Path
import pandas as pd
import streamlit as st
import plotly.express as px
import plotly.graph_objects as go
import datetime
import pandas as pd
import numpy as np
import requests
import yfinance as yf
import plotly.express as px
from datetime import datetime, timedelta

# Configuration de la page
st.set_page_config(page_title="Portfolio Dashboard", page_icon="📈", layout="wide")

# Chargement des données générées par Rust
DATA_PATH = Path("./data/dashboard.json")

if not DATA_PATH.exists():
    st.error("Fichier dashboard.json introuvable. Lance d'abord le binaire Rust (`cargo run`).")
    st.stop()

with open(DATA_PATH, "r", encoding="utf-8") as f:
    data = json.load(f)

# Préparation du DataFrame
df = pd.DataFrame(data["assets"])
df["pnl_color"] = df["pnl_eur"].apply(lambda x: "Gain" if x >= 0 else "Perte")

# --- EN-TÊTE ET KPIs ---
st.title("📈 Mon Portefeuille")

col1, col2, col3, col4 = st.columns(4)
col1.metric("Valeur Totale", f"{data['total_value_eur']:,.2f} €")
col2.metric("Cost Basis", f"{data['total_cost_basis_eur']:,.2f} €")
col3.metric("P&L Latent Total", f"{data['total_pnl_eur']:,.2f} €")

pnl_pct_global = (data['total_pnl_eur'] / data['total_cost_basis_eur'] * 100) if data['total_cost_basis_eur'] > 0 else 0
col4.metric("Performance Globale", f"{pnl_pct_global:+.2f} %")

st.divider()

# --- GRAPHIQUES ---
col_left, col_right = st.columns(2)

with col_left:
    st.subheader("🥧 Allocation du Portefeuille")
    fig_pie = px.pie(
        df, 
        values='value_eur', 
        names='symbol',
        hole=0.4, # Donut chart
        color_discrete_sequence=px.colors.qualitative.Pastel
    )
    fig_pie.update_traces(textposition='inside', textinfo='percent+label')
    fig_pie.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
    st.plotly_chart(fig_pie, use_container_width=True)

with col_right:
    st.subheader("📊 P&L par Actif (EUR)")
    fig_bar = px.bar(
        df,
        x='symbol',
        y='pnl_eur',
        color='pnl_color',
        color_discrete_map={'Gain': '#26C281', 'Perte': '#E74C3C'},
        labels={'pnl_eur': 'P&L (€)', 'symbol': 'Actif'}
    )
    fig_bar.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
    st.plotly_chart(fig_bar, use_container_width=True)

# --- TABLEAU DÉTAILLÉ ---
st.divider()
st.subheader("📋 Détail des positions")

# Formatage du tableau pour un affichage propre
df_display = df.copy()
df_display['quantity'] = df_display['quantity'].apply(lambda x: f"{x:,.4f}")
df_display['price_eur'] = df_display['price_eur'].apply(lambda x: f"{x:,.2f} €")
df_display['value_eur'] = df_display['value_eur'].apply(lambda x: f"{x:,.2f} €")
df_display['cost_basis_eur'] = df_display['cost_basis_eur'].apply(lambda x: f"{x:,.2f} €")
df_display['pnl_eur'] = df_display['pnl_eur'].apply(lambda x: f"{x:+,.2f} €")
df_display['pnl_pct'] = df_display['pnl_pct'].apply(lambda x: f"{x:+.2f} %")

# Renommer les colonnes
df_display = df_display.rename(columns={
    'symbol': 'Symbole',
    'kind': 'Type',
    'quantity': 'Quantité',
    'price_eur': 'Prix',
    'value_eur': 'Valeur',
    'cost_basis_eur': 'Cost Basis',
    'pnl_eur': 'P&L (EUR)',
    'pnl_pct': 'P&L (%)'
})

st.dataframe(
    df_display[['Symbole', 'Type', 'Quantité', 'Prix', 'Valeur', 'Cost Basis', 'P&L (EUR)', 'P&L (%)']],
    use_container_width=True,
    hide_index=True
)

st.divider()
# --- NOUVELLE SECTION : MATRICE DE CORRÉLATION ---
st.divider()
st.subheader("🔥 Matrice de corrélation (90 jours)")

# --- MATRICE DE CORRÉLATION ---
st.divider()
st.subheader("🔥 Matrice de corrélation (90 jours)")

if "correlation_matrix" in data and data["correlation_matrix"]:
    # Pandas peut transformer le dictionnaire de dictionnaires en DataFrame directement
    corr_matrix = pd.DataFrame(data["correlation_matrix"])
    
    # Création de la Heatmap avec Plotly
    fig_corr = px.imshow(
        corr_matrix,
        text_auto=".2f",
        color_continuous_scale='RdBu_r', # Rouge = positif, Bleu = négatif
        zmin=-1, zmax=1,
        title="Corrélation des rendements journaliers (90 jours)"
    )
    
    fig_corr.update_layout(
        height=600,
        margin=dict(t=50, b=0, l=0, r=0),
        xaxis_title="Actifs",
        yaxis_title="Actifs"
    )
    
    st.plotly_chart(fig_corr, use_container_width=True)
    
    st.markdown("""
    **Comment lire cette matrice ?**
    - 🔴 **Rouge (proche de 1)** : Les actifs bougent ensemble. (Mauvaise diversification).
    - ⚪ **Blanc (proche de 0)** : Aucune corrélation. (Idéal pour stabiliser).
    - 🔵 **Bleu (proche de -1)** : Corrélation négative. (Vrais couvre-risques).
    """)
else:
    st.info("Données insuffisantes pour calculer la corrélation.")