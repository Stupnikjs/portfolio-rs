"""Matrice de corrélation (pré-calculée côté Rust, ou recalculée en direct
via Yahoo Finance quand l'utilisateur ajoute des tickers ad hoc), et vue
par actif."""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import plotly.express as px
import streamlit as st
import yfinance as yf
from clustering import assign_clusters, cluster_summary, cluster_table, compute_linkage, format_cluster_summary, render_dendrogram
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

    if correlation_matrices and selected_window in correlation_matrices:
        # Cas nominal : on réutilise la matrice pré-calculée par le Rust pour
        # la fenêtre sélectionnée, en filtrant selon le seuil de valeur.
        # Les paires sans signal suffisant arrivent en `null` -> NaN pandas,
        # pas en 0.0 : ne jamais les re-remplir avec .fillna(0), sous peine
        # de recréer le bug (absence de données affichée comme décorrélation).
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

    n_missing = int(corr_matrix.isna().sum().sum())
    if n_missing:
        st.caption(f"⬜ Cases vides : {n_missing} paire(s) sans assez de données communes pour calculer un coefficient.")

    st.markdown("""
    **Comment lire cette matrice ?**
    - 🔴 **Rouge (proche de 1)** : Les actifs bougent ensemble (mauvaise diversification).
    - ⚪ **Blanc (proche de 0)** : Aucune corrélation (idéal pour stabiliser).
    - 🔵 **Bleu (proche de -1)** : Corrélation négative (vrais couvre-risques).
    - ▪️ **Case vide** : pas assez de données communes pour calculer un coefficient (à ne pas lire comme "décorrélé").
    """)

def _render_clusters(
    correlation_matrices: dict,
    selected_window: str,
    eligible_symbols: set[str],
    df: pd.DataFrame,
    total_portfolio_value: float,
) -> None:
    st.subheader(f"🧩 Groupes automatiques ({selected_window})")

    if not (correlation_matrices and selected_window in correlation_matrices):
        st.info("Données insuffisantes pour calculer les groupes.")
        return

    full_corr = pd.DataFrame(correlation_matrices[selected_window])
    kept = [s for s in full_corr.columns if s in eligible_symbols]
    corr_matrix = full_corr.loc[kept, kept] if len(kept) >= 3 else pd.DataFrame()

    if corr_matrix.empty:
        st.info("Au moins 3 actifs sont nécessaires pour former des groupes.")
        return

    max_clusters = len(corr_matrix.columns) - 1
    n_clusters = st.slider(
        "Nombre de groupes",
        min_value=2,
        max_value=max_clusters,
        value=min(4, max_clusters),
    )

    linkage_matrix = compute_linkage(corr_matrix)
    cluster_labels = assign_clusters(corr_matrix, linkage_matrix, n_clusters)

    st.plotly_chart(render_dendrogram(corr_matrix, linkage_matrix), use_container_width=True)
    st.caption(
        "Plus deux actifs fusionnent bas dans l'arbre, plus leur comportement "
        "historique est proche -- indépendamment de leur type (Action/Crypto/...)."
    )

    # --- Récap par groupe avec métriques financières ---
    summary = cluster_summary(cluster_labels, df, total_portfolio_value)

    if summary.empty:
        st.info("Aucun actif du portefeuille trouvé dans les groupes.")
        return

    # Multiselect pour agréger les groupes souhaités
    group_options = summary["Groupe"].tolist()
    selected_groups = st.multiselect(
        "Sélectionner les groupes à agréger",
        options=group_options,
        default=group_options,
        help="Les KPIs et graphiques ci-dessous ne prennent en compte que les groupes sélectionnés.",
    )

    st.markdown("##### Détail par groupe")
    st.dataframe(
        format_cluster_summary(summary),
        use_container_width=True,
        hide_index=True,
    )

    if not selected_groups:
        st.info("Sélectionne au moins un groupe pour voir l'agrégat.")
        return

    sel = summary[summary["Groupe"].isin(selected_groups)].copy()
    total_sel_value = float(sel["Valeur (€)"].sum())
    total_sel_cost = float(sel["Cost Basis (€)"].sum())
    total_sel_pnl = float(sel["P&L (€)"].sum())
    sel_perf = (total_sel_pnl / total_sel_cost * 100) if total_sel_cost > 0 else 0.0
    sel_weight = (total_sel_value / total_portfolio_value * 100) if total_portfolio_value > 0 else 0.0

    # --- KPIs agrégés ---
    st.markdown("##### 📊 Agrégat des groupes sélectionnés")
    c1, c2, c3, c4, c5 = st.columns(5)
    c1.metric("Valeur agrégée", f"{total_sel_value:,.2f} €")
    c2.metric("Cost Basis agrégé", f"{total_sel_cost:,.2f} €")
    c3.metric("P&L agrégé", f"{total_sel_pnl:+,.2f} €")
    c4.metric("Performance agrégée", f"{sel_perf:+.2f} %")
    c5.metric("% du portefeuille", f"{sel_weight:.2f} %")

    # --- Graphiques : poids dans le portefeuille + performance par groupe ---
    col_left, col_right = st.columns(2)

    with col_left:
        st.markdown("##### 🥧 Poids de chaque groupe dans le portefeuille")
        fig_pie = px.pie(
            sel,
            values="Valeur (€)",
            names="Groupe",
            hole=0.4,
        )
        fig_pie.update_traces(
            textposition="inside",
            textinfo="percent+label",
        )
        fig_pie.update_layout(
            showlegend=False,
            margin=dict(t=0, b=0, l=0, r=0),
        )
        st.plotly_chart(fig_pie, use_container_width=True)

    with col_right:
        st.markdown("##### 📊 Performance par groupe (P&L %)")
        fig_bar = px.bar(
            sel,
            x="Groupe",
            y="P&L (%)",
            color="P&L (%)",
            color_continuous_scale="RdYlGn",
            range_color=[-max(abs(sel["P&L (%)"].min()), abs(sel["P&L (%)"].max())) * 1.1,
                         max(abs(sel["P&L (%)"].min()), abs(sel["P&L (%)"].max())) * 1.1],
            text=sel["P&L (%)"].apply(lambda v: f"{v:+.2f} %"),
        )
        fig_bar.update_traces(textposition="outside")
        fig_bar.update_layout(
            margin=dict(t=0, b=0, l=0, r=0),
            coloraxis_showscale=False,
        )
        st.plotly_chart(fig_bar, use_container_width=True)

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
    full_series = corr_matrix_asset[selected_asset].drop(selected_asset)

    # Avant le patch Rust, les paires sans données arrivaient en 0.0 --
    # indiscernables d'une vraie décorrélation. Elles arrivent maintenant en
    # NaN : on les retire du graphe plutôt que de les afficher comme des 0.
    corr_series = full_series.dropna().sort_values(ascending=True)
    missing = full_series[full_series.isna()].index.tolist()

    if corr_series.empty:
        st.info(f"Aucun coefficient calculable pour {selected_asset} sur cette fenêtre (données insuffisantes pour toutes les autres paires).")
        return

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
    if missing:
        st.caption(f"⚠️ Non calculable (données insuffisantes) : {', '.join(sorted(missing))}")


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
    eligible_symbols = set(df.loc[df["value_eur"] >= min_value_eur, "symbol"]) | BENCHMARK_LABELS
    """
    corr_matrix, source_note = _compute_corr_matrix(
        df, correlation_matrices, selected_window, eligible_symbols
    )
    _render_full_matrix(corr_matrix, source_note, selected_window)
    """
    st.divider()
    _render_per_asset(correlation_matrices, selected_window, eligible_symbols)

    st.divider()
    _render_clusters(
    correlation_matrices,
    selected_window,
    eligible_symbols,
    df,
    data["total_value_eur"],
)