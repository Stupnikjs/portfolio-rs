"""Matrice de corrélation (pré-calculée côté Rust, ou recalculée en direct
via Yahoo Finance quand l'utilisateur ajoute des tickers ad hoc), et vue
par actif."""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import plotly.express as px
import streamlit as st
import yfinance as yf

from config import BENCHMARK_LABELS, BENCHMARK_TICKERS, DEFAULT_WINDOW_ORDER, WINDOW_TO_YF_PERIOD
from data import yfinance_ticker_for


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


def _select_window(correlation_matrices: dict) -> str:
    if correlation_matrices:
        available_windows = (
            [w for w in DEFAULT_WINDOW_ORDER if w in correlation_matrices]
            + [w for w in correlation_matrices if w not in DEFAULT_WINDOW_ORDER]
        )
        return st.selectbox("Fenêtre temporelle", options=available_windows, index=0)
    return "90d"


def _compute_corr_matrix(
    df: pd.DataFrame,
    correlation_matrices: dict,
    selected_window: str,
    eligible_symbols: set[str],
    
) -> tuple[pd.DataFrame, str | None]:
    yf_period = WINDOW_TO_YF_PERIOD.get(selected_window, "3mo")

    """
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
        return corr_matrix, f"Corrélation recalculée en direct (Yahoo Finance, fenêtre={selected_window})"
    """
    if correlation_matrices and selected_window in correlation_matrices:
        # Cas nominal : on réutilise la matrice pré-calculée par le Rust pour
        # la fenêtre sélectionnée, en filtrant selon le seuil de valeur.
        full_corr = pd.DataFrame(correlation_matrices[selected_window])
        kept = [s for s in full_corr.columns if s in eligible_symbols]
        corr_matrix = full_corr.loc[kept, kept] if len(kept) >= 2 else pd.DataFrame()
        return corr_matrix, f"Corrélation pré-calculée (pipeline Rust, fenêtre={selected_window})"

    return pd.DataFrame(), None


def _render_full_matrix(corr_matrix: pd.DataFrame, source_note: str | None, selected_window: str) -> None:
    if corr_matrix.empty or corr_matrix.shape[1] < 2:
        st.info("Données insuffisantes pour calculer la corrélation (moins de 2 actifs après filtrage).")
        return

    st.caption(source_note)

    fig_corr = px.imshow(
        corr_matrix,
        text_auto=".2f",
        color_continuous_scale="RdBu_r",
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


def _render_per_asset(correlation_matrices: dict, selected_window: str, eligible_symbols: set[str]) -> None:
    st.subheader(f"🔗 Corrélations par actif ({selected_window})")

    if not (correlation_matrices and selected_window in correlation_matrices):
        st.info("Données insuffisantes pour calculer la corrélation.")
        return

    full_corr = pd.DataFrame(correlation_matrices[selected_window])
    kept = [s for s in full_corr.columns if s in eligible_symbols]
    corr_matrix_asset = full_corr.loc[kept, kept] if len(kept) >= 2 else pd.DataFrame()

    if corr_matrix_asset.empty:
        st.info("Données insuffisantes après filtrage.")
        return

    selected_asset = st.selectbox("Choisir un actif", options=corr_matrix_asset.columns.tolist())
    corr_series = corr_matrix_asset[selected_asset].drop(selected_asset).sort_values(ascending=True)

    fig_corr_asset = go.Figure(go.Bar(
        x=corr_series.values,
        y=corr_series.index,
        orientation="h",
        marker=dict(
            color=corr_series.values,
            colorscale="RdBu_r",
            cmin=-1, cmax=1,
            colorbar=dict(title="Corrélation"),
        ),
        text=[f"{v:+.2f}" for v in corr_series.values],
        textposition="outside",
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


def render_correlation_section(df: pd.DataFrame, data: dict) -> None:
    """Point d'entrée appelé par app.py pour tout le bloc corrélation."""
    st.subheader("🔥 Matrice de corrélation")

    correlation_matrices = data.get("correlation_matrices", {})
    selected_window = _select_window(correlation_matrices)

    min_value_eur = st.number_input(
        "Valeur minimale par actif du portefeuille pour la corrélation (EUR)",
        min_value=0.0, value=10.0, step=5.0,
        help="Filtre uniquement tes positions -- les indices/matières premières de référence restent toujours affichés.",
    )
    """
    extra_tickers_input = st.text_input(
        "Ajouter d'autres tickers Yahoo Finance à la demande (séparés par des virgules)",
        placeholder="ex: ^IXIC, TLT, AAPL",
    )
    
    extra_tickers = [t.strip().upper() for t in extra_tickers_input.split(",") if t.strip()]
    """
    eligible_symbols = set(df.loc[df["value_eur"] >= min_value_eur, "symbol"]) | BENCHMARK_LABELS

    corr_matrix, source_note = _compute_corr_matrix(
        df, correlation_matrices, selected_window, eligible_symbols
    )

    st.divider()
    _render_per_asset(correlation_matrices, selected_window, eligible_symbols)
