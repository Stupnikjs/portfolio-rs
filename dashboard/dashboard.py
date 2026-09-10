################################################################################
# Dossier : dashboard
# Fichier : dashboard.py
# Chemin relatif : dashboard.py
################################################################################
from __future__ import annotations

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
    st.subheader("🥧 Allocation par Groupe")

    # 1. Exclure le Cash (EUR)
    df_no_cash = df[~df['symbol'].str.upper().eq('EUR')].copy()

    # 2. Mapping de tes symboles vers les groupes demandés
    groupe_mapping = {
        'SANTE': ['SAN.FR', 'MDT.US', 'BIM.FR', 'NOV.DE', 'SHL.DE', 'BSX.US'],
        'INDUS': ['SGO.FR', 'AI.FR',  'BAS.DE', ],
        'MAT PREM' : ['4BRZ.DE','EGLN.UK' ],
        'CRYPTO': ['ETH', 'BTC', 'LINK', 'MSTR.US', 'IB1T.DE'],
        'ASIE': ['CEBL.DE', 'NDIA.UK', 'PASI.FR', 'XFVT.DE']
    }

    # Création d'un dictionnaire inversé pour associer chaque symbole à son groupe
    symbol_to_groupe = {symbole: groupe for groupe, symboles in groupe_mapping.items() for symbole in symboles}

    # Application du mapping sur le DataFrame (les actifs non trouvés iraient dans 'Autres')
    df_no_cash['groupe'] = df_no_cash['symbol'].map(symbol_to_groupe).fillna('Autres')

    # 3. Graphique en anneau (Donut) respectant ta charte Pastel
    fig_pie = px.pie(
        df_no_cash, 
        values='value_eur', 
        names='groupe',
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
# --- MATRICE DE CORRÉLATION ---
st.subheader("🔥 Matrice de corrélation")

# Le Rust écrit "correlation_matrices" (pluriel) : dict window -> matrice.
correlation_matrices = data.get("correlation_matrices", {})

# Mapping label de fenêtre (côté Rust) -> période acceptée par yfinance
WINDOW_TO_YF_PERIOD = {"90d": "3mo", "6m": "6mo", "1y": "1y"}

if correlation_matrices:
    # Ordre canonique si toutes les fenêtres sont présentes
    default_order = ["90d", "6m", "1y"]
    available_windows = [w for w in default_order if w in correlation_matrices] \
                      + [w for w in correlation_matrices if w not in default_order]
    selected_window = st.selectbox("Fenêtre temporelle", options=available_windows, index=0)
else:
    selected_window = "90d"

yf_period = WINDOW_TO_YF_PERIOD.get(selected_window, "3mo")


@st.cache_data(ttl=3600, show_spinner=False)
def fetch_returns(tickers: tuple[str, ...], period: str = "3mo") -> pd.DataFrame:
    """Télécharge les clôtures quotidiennes Yahoo Finance pour `tickers` et
    renvoie les rendements journaliers (une colonne par ticker). Les
    tickers introuvables sont simplement absents du résultat plutôt que
    de faire échouer tout le calcul."""
    prices = {}
    for ticker in tickers:
        try:
            hist = yf.Ticker(ticker).history(period=period, interval="1d")
            if not hist.empty:
                prices[ticker] = hist["Close"]
        except Exception:
            continue
    if not prices:
        return pd.DataFrame()
    price_df = pd.concat(prices, axis=1)
    price_df.index = price_df.index.tz_localize(None)
    return price_df.pct_change().dropna(how="all")


def yfinance_ticker_for(row: pd.Series) -> str:
    ticker = row.get("ticker")
    if isinstance(ticker, str) and ticker:
        return ticker
    if row.get("kind") == "Crypto":
        return f"{row['symbol']}-EUR"
    return row["symbol"]


BENCHMARK_LABELS = {"MSCI China", "CAC 40", "S&P 500", "Or", "Argent"}
BENCHMARK_TICKERS = {
    "MSCI China": "MCHI",
    "CAC 40": "^FCHI",
    "S&P 500": "^GSPC",
    "Or": "GC=F",
    "Argent": "SI=F",
}

min_value_eur = st.number_input(
    "Valeur minimale par actif du portefeuille pour la corrélation (EUR)",
    min_value=0.0, value=10.0, step=5.0,
    help="Filtre uniquement tes positions -- les indices/matières premières de référence restent toujours affichés.",
)

extra_tickers_input = st.text_input(
    "Ajouter d'autres tickers Yahoo Finance à la demande (séparés par des virgules)",
    placeholder="ex: ^IXIC, TLT, AAPL",
)
extra_tickers = [t.strip().upper() for t in extra_tickers_input.split(",") if t.strip()]

eligible_symbols = set(df.loc[df["value_eur"] >= min_value_eur, "symbol"]) | BENCHMARK_LABELS

if extra_tickers:
    # Mode "recalcul en direct" : on appelle Yahoo Finance sur la fenêtre choisie.
    eligible_rows = df[df["symbol"].isin(eligible_symbols)]
    portfolio_tickers = {row["symbol"]: yfinance_ticker_for(row) for _, row in eligible_rows.iterrows()}
    label_by_ticker = {
        **{v: k for k, v in portfolio_tickers.items()},
        **{v: k for k, v in BENCHMARK_TICKERS.items()},
        **{t: t for t in extra_tickers},
    }

    with st.spinner("Récupération des historiques de prix (Yahoo Finance)..."):
        returns = fetch_returns(tuple(sorted(set(label_by_ticker.keys()))), period=yf_period)

    missing = set(label_by_ticker.keys()) - set(returns.columns)
    if missing:
        st.caption(f"Tickers non résolus par Yahoo Finance, ignorés : {', '.join(sorted(missing))}")

    returns = returns.rename(columns=label_by_ticker)
    corr_matrix = returns.corr() if returns.shape[1] >= 2 else pd.DataFrame()
    source_note = f"Corrélation recalculée en direct (Yahoo Finance, fenêtre={selected_window})"
elif correlation_matrices and selected_window in correlation_matrices:
    # ✅ Cas nominal : on réutilise la matrice pré-calculée par le Rust pour
    # la fenêtre sélectionnée, en filtrant selon le seuil de valeur.
    full_corr = pd.DataFrame(correlation_matrices[selected_window])
    kept = [s for s in full_corr.columns if s in eligible_symbols]
    corr_matrix = full_corr.loc[kept, kept] if len(kept) >= 2 else pd.DataFrame()
    source_note = f"Corrélation pré-calculée (pipeline Rust, fenêtre={selected_window})"
else:
    corr_matrix = pd.DataFrame()
    source_note = None

if not corr_matrix.empty and corr_matrix.shape[1] >= 2:
    st.caption(source_note)

    fig_corr = px.imshow(
        corr_matrix,
        text_auto=".2f",
        color_continuous_scale='RdBu_r',
        zmin=-1, zmax=1,
        title=f"Corrélation des rendements journaliers ({selected_window})",
    )
    fig_corr.update_layout(
        height=600,
        margin=dict(t=50, b=0, l=0, r=0),
        xaxis_title="Actifs",
        yaxis_title="Actifs",
    )
    st.plotly_chart(fig_corr, use_container_width=True)

    st.markdown("""
    **Comment lire cette matrice ?**
    - 🔴 **Rouge (proche de 1)** : Les actifs bougent ensemble (mauvaise diversification).
    - ⚪ **Blanc (proche de 0)** : Aucune corrélation (idéal pour stabiliser).
    - 🔵 **Bleu (proche de -1)** : Corrélation négative (vrais couvre-risques).
    """)
else:
    st.info("Données insuffisantes pour calculer la corrélation (moins de 2 actifs après filtrage).")


st.divider()
st.subheader(f"🔗 Corrélations par actif ({selected_window})")

if correlation_matrices and selected_window in correlation_matrices:
    full_corr = pd.DataFrame(correlation_matrices[selected_window])
    # Même filtre que pour la matrice complète : cohérence visuelle.
    kept = [s for s in full_corr.columns if s in eligible_symbols]
    corr_matrix_asset = full_corr.loc[kept, kept] if len(kept) >= 2 else pd.DataFrame()

    if not corr_matrix_asset.empty:
        selected_asset = st.selectbox("Choisir un actif", options=corr_matrix_asset.columns.tolist())

        corr_series = corr_matrix_asset[selected_asset].drop(selected_asset).sort_values(ascending=True)

        fig_corr_asset = go.Figure(go.Bar(
            x=corr_series.values,
            y=corr_series.index,
            orientation='h',
            marker=dict(
                color=corr_series.values,
                colorscale='RdBu_r',
                cmin=-1, cmax=1,
                colorbar=dict(title="Corrélation"),
            ),
            text=[f"{v:+.2f}" for v in corr_series.values],
            textposition='outside',
        ))

        fig_corr_asset.update_layout(
            title=f"Corrélation de {selected_asset} avec les autres actifs",
            xaxis_title="Coefficient de corrélation",
            xaxis=dict(range=[-1.15, 1.15]),
            height=max(300, 40 * len(corr_series)),
            margin=dict(t=50, b=0, l=10, r=50),
        )

        st.plotly_chart(fig_corr_asset, use_container_width=True)

        st.caption(
            "🔴 Rouge = corrélation positive (bougent ensemble) · "
            "🔵 Bleu = corrélation négative (couvre-risque) · "
            "Proche de 0 = décorrélé"
        )
    else:
        st.info("Données insuffisantes après filtrage.")
else:
    st.info("Données insuffisantes pour calculer la corrélation.")