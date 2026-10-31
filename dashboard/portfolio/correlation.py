"""Page Corrélation : matrices pré-calculées par le pipeline Rust (`correlation_matrices` de
dashboard.json), corrélations par actif, regroupement automatique (clustering) et variation
d'une paire d'actifs selon la fenêtre temporelle.

Le clustering regroupe les actifs par similarité de comportement, sans classification a priori :
deux actifs très corrélés finissent dans le même groupe peu importe leur type (Action/Crypto/...).
"""
from __future__ import annotations

import numpy as np
import pandas as pd
import plotly.express as px
import plotly.graph_objects as go
import streamlit as st
from scipy.cluster.hierarchy import dendrogram, fcluster, linkage
from scipy.spatial.distance import squareform

from portfolio.config import BENCHMARK_LABELS, DEFAULT_WINDOW_ORDER, MIN_POSITION_VALUE_EUR, WINDOW_LABELS
from portfolio.data import filter_small_positions, load_portfolio
from portfolio.theme import BG_COLOR, GRID_COLOR, PASTEL_DARK_SEQUENCE


# --- Clustering (logique pure) ------------------------------------------------------

def distance_matrix(corr_matrix: pd.DataFrame) -> np.ndarray:
    """d = sqrt(2 * (1 - corr)) -- vraie métrique, contrairement à 1 - corr
    qui ne respecte pas l'inégalité triangulaire.

    Un NaN (données insuffisantes) est traité comme « décorrélé » (0) : distance maximale
    sqrt(2) entre les deux actifs.
    """
    corr = corr_matrix.fillna(0).clip(-1, 1)
    dist = np.sqrt(2 * (1 - corr.values))
    np.fill_diagonal(dist, 0.0)
    return (dist + dist.T) / 2  # symétrise (absorbe les arrondis flottants)


def compute_linkage(corr_matrix: pd.DataFrame, method: str = "average") -> np.ndarray:
    """Linkage hiérarchique scipy à partir de la matrice de corrélation."""
    condensed = squareform(distance_matrix(corr_matrix), checks=False)
    return linkage(condensed, method=method)


def assign_clusters(corr_matrix: pd.DataFrame, linkage_matrix: np.ndarray, n_clusters: int) -> pd.Series:
    """Coupe le dendrogramme pour obtenir exactement `n_clusters` groupes."""
    labels = fcluster(linkage_matrix, t=n_clusters, criterion="maxclust")
    return pd.Series(labels, index=corr_matrix.columns, name="cluster")


def dendrogram_figure(corr_matrix: pd.DataFrame, linkage_matrix: np.ndarray) -> go.Figure:
    """Dendrogramme interactif -- scipy calcule la géométrie, plotly l'affiche."""
    dendro = dendrogram(linkage_matrix, labels=corr_matrix.columns.tolist(), no_plot=True)
    icoord = np.array(dendro["icoord"])
    dcoord = np.array(dendro["dcoord"])
    ordered_labels = dendro["ivl"]

    fig = go.Figure()
    for xs, ys in zip(icoord, dcoord):
        fig.add_trace(go.Scatter(
            x=xs, y=ys, mode="lines",
            line=dict(color=PASTEL_DARK_SEQUENCE[0], width=1.5),
            hoverinfo="skip", showlegend=False,
        ))

    tickvals = [5 + 10 * i for i in range(len(ordered_labels))]
    fig.update_layout(
        xaxis=dict(tickmode="array", tickvals=tickvals, ticktext=ordered_labels, tickangle=45),
        yaxis_title="Distance (0 = comportement identique)",
        height=450,
        margin=dict(t=20, b=90, l=40, r=20),
    )
    return fig


def cluster_summary(cluster_labels: pd.Series, df: pd.DataFrame, total_portfolio_value: float) -> pd.DataFrame:
    """Une ligne par groupe contenant des actifs du portefeuille, avec la liste des actifs
    détenus, la valeur, le cost basis, le P&L (€ et %) et le poids dans le portefeuille global.

    Les actifs non détenus (benchmarks : indices, matières premières) sont ignorés ; un groupe
    sans actif réel n'apparaît pas dans le résumé.
    """
    df_indexed = df.set_index("symbol")
    rows = []

    for cluster_id in sorted(cluster_labels.unique()):
        symbols_in_cluster = cluster_labels[cluster_labels == cluster_id].index.tolist()
        portfolio_assets = [s for s in symbols_in_cluster if s in df_indexed.index]
        if not portfolio_assets:
            continue

        sub = df_indexed.loc[portfolio_assets]
        value = float(sub["value_eur"].sum())
        cost = float(sub["cost_basis_eur"].sum())
        pnl = float(sub["pnl_eur"].sum())

        rows.append({
            "Groupe": int(cluster_id),
            "Actifs": ", ".join(sorted(portfolio_assets)),
            "Nb actifs": len(portfolio_assets),
            "Valeur (€)": value,
            "Cost Basis (€)": cost,
            "P&L (€)": pnl,
            "P&L (%)": (pnl / cost * 100) if cost > 0 else 0.0,
            "% Portefeuille": (value / total_portfolio_value * 100) if total_portfolio_value > 0 else 0.0,
        })

    return pd.DataFrame(rows)


def format_cluster_summary(summary: pd.DataFrame) -> pd.DataFrame:
    """Version formatée pour st.dataframe."""
    disp = summary.copy()
    disp["Valeur (€)"] = disp["Valeur (€)"].apply(lambda x: f"{x:,.2f} €")
    disp["Cost Basis (€)"] = disp["Cost Basis (€)"].apply(lambda x: f"{x:,.2f} €")
    disp["P&L (€)"] = disp["P&L (€)"].apply(lambda x: f"{x:+,.2f} €")
    disp["P&L (%)"] = disp["P&L (%)"].apply(lambda x: f"{x:+.2f} %")
    disp["% Portefeuille"] = disp["% Portefeuille"].apply(lambda x: f"{x:.2f} %")
    return disp


# --- Sélection ------------------------------------------------------------------

def _has_window(correlation_matrices: dict, window: str) -> bool:
    return bool(correlation_matrices) and window in correlation_matrices


def _correlation_subset(
    correlation_matrices: dict, window: str, eligible_symbols: set[str], min_assets: int,
) -> pd.DataFrame:
    """Sous-matrice pré-calculée (Rust) restreinte aux symboles éligibles ; vide si < min_assets.

    Les paires sans signal arrivent en `null` -> NaN pandas, pas en 0.0 : ne jamais les re-remplir
    avec .fillna(0), sous peine de présenter une absence de données comme une décorrélation.
    """
    full_corr = pd.DataFrame(correlation_matrices[window])
    kept = [s for s in full_corr.columns if s in eligible_symbols]
    return full_corr.loc[kept, kept] if len(kept) >= min_assets else pd.DataFrame()


def _select_window(correlation_matrices: dict) -> str:
    if correlation_matrices:
        available_windows = (
            [w for w in DEFAULT_WINDOW_ORDER if w in correlation_matrices]
            + [w for w in correlation_matrices if w not in DEFAULT_WINDOW_ORDER]
        )
        return st.selectbox("Fenêtre temporelle", options=available_windows, index=0)
    return DEFAULT_WINDOW_ORDER[0]


# --- Rendu ----------------------------------------------------------------------

def _render_per_asset(correlation_matrices: dict, selected_window: str, eligible_symbols: set[str]) -> None:
    st.subheader(f"🔗 Corrélations par actif ({selected_window})")

    if not _has_window(correlation_matrices, selected_window):
        st.info("Données insuffisantes pour calculer la corrélation.")
        return

    corr_matrix_asset = _correlation_subset(correlation_matrices, selected_window, eligible_symbols, min_assets=2)
    if corr_matrix_asset.empty:
        st.info("Données insuffisantes après filtrage.")
        return

    selected_asset = st.selectbox("Choisir un actif", options=corr_matrix_asset.columns.tolist())
    full_series = corr_matrix_asset[selected_asset].drop(selected_asset)

    # Les paires sans données arrivent en NaN : on les retire du graphe plutôt que de les afficher comme des 0.
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


def _render_clusters(
    correlation_matrices: dict,
    selected_window: str,
    eligible_symbols: set[str],
    df: pd.DataFrame,
    total_portfolio_value: float,
) -> None:
    st.subheader(f"🧩 Groupes automatiques ({selected_window})")

    if not _has_window(correlation_matrices, selected_window):
        st.info("Données insuffisantes pour calculer les groupes.")
        return

    corr_matrix = _correlation_subset(correlation_matrices, selected_window, eligible_symbols, min_assets=3)
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

    st.plotly_chart(dendrogram_figure(corr_matrix, linkage_matrix), use_container_width=True)
    st.caption(
        "Plus deux actifs fusionnent bas dans l'arbre, plus leur comportement "
        "historique est proche -- indépendamment de leur type (Action/Crypto/...)."
    )

    # --- Récap par groupe avec métriques financières ---
    summary = cluster_summary(cluster_labels, df, total_portfolio_value)
    if summary.empty:
        st.info("Aucun actif du portefeuille trouvé dans les groupes.")
        return

    group_options = summary["Groupe"].tolist()
    selected_groups = st.multiselect(
        "Sélectionner les groupes à agréger",
        options=group_options,
        default=group_options,
        help="Les KPIs et graphiques ci-dessous ne prennent en compte que les groupes sélectionnés.",
    )

    st.markdown("##### Détail par groupe")
    st.dataframe(format_cluster_summary(summary), use_container_width=True, hide_index=True)

    if not selected_groups:
        st.info("Sélectionne au moins un groupe pour voir l'agrégat.")
        return

    sel = summary[summary["Groupe"].isin(selected_groups)].copy()
    total_sel_value = float(sel["Valeur (€)"].sum())
    total_sel_cost = float(sel["Cost Basis (€)"].sum())
    total_sel_pnl = float(sel["P&L (€)"].sum())
    sel_perf = (total_sel_pnl / total_sel_cost * 100) if total_sel_cost > 0 else 0.0
    sel_weight = (total_sel_value / total_portfolio_value * 100) if total_portfolio_value > 0 else 0.0

    st.markdown("##### 📊 Agrégat des groupes sélectionnés")
    c1, c2, c3, c4, c5 = st.columns(5)
    c1.metric("Valeur agrégée", f"{total_sel_value:,.2f} €")
    c2.metric("Cost Basis agrégé", f"{total_sel_cost:,.2f} €")
    c3.metric("P&L agrégé", f"{total_sel_pnl:+,.2f} €")
    c4.metric("Performance agrégée", f"{sel_perf:+.2f} %")
    c5.metric("% du portefeuille", f"{sel_weight:.2f} %")

    col_left, col_right = st.columns(2)

    with col_left:
        st.markdown("##### 🥧 Poids de chaque groupe dans le portefeuille")
        fig_pie = px.pie(sel, values="Valeur (€)", names="Groupe", hole=0.4)
        fig_pie.update_traces(textposition="inside", textinfo="percent+label")
        fig_pie.update_layout(showlegend=False, margin=dict(t=0, b=0, l=0, r=0))
        st.plotly_chart(fig_pie, use_container_width=True)

    with col_right:
        st.markdown("##### 📊 Performance par groupe (P&L %)")
        bound = max(abs(sel["P&L (%)"].min()), abs(sel["P&L (%)"].max())) * 1.1
        fig_bar = px.bar(
            sel,
            x="Groupe",
            y="P&L (%)",
            color="P&L (%)",
            color_continuous_scale="RdYlGn",
            range_color=[-bound, bound],
            text=sel["P&L (%)"].apply(lambda v: f"{v:+.2f} %"),
        )
        fig_bar.update_traces(textposition="outside")
        fig_bar.update_layout(margin=dict(t=0, b=0, l=0, r=0), coloraxis_showscale=False)
        st.plotly_chart(fig_bar, use_container_width=True)


def render_correlation_section(df: pd.DataFrame, data: dict) -> None:
    """Corrélations par actif + groupes automatiques, sur les positions `df` et les benchmarks."""
    st.subheader("🔥 Matrice de corrélation")

    correlation_matrices = data.get("correlation_matrices", {})
    selected_window = _select_window(correlation_matrices)

    min_value_eur = st.number_input(
        "Valeur minimale par actif du portefeuille pour la corrélation (EUR)",
        min_value=0.0, value=10.0, step=5.0,
        help="Filtre uniquement tes positions -- les indices/matières premières de référence restent toujours affichés.",
    )
    eligible_symbols = set(df.loc[df["value_eur"] >= min_value_eur, "symbol"]) | BENCHMARK_LABELS

    st.divider()
    _render_per_asset(correlation_matrices, selected_window, eligible_symbols)

    st.divider()
    _render_clusters(
        correlation_matrices,
        selected_window,
        eligible_symbols - BENCHMARK_LABELS,  # clustering sur les positions uniquement
        df,
        data["total_value_eur"],
    )


def render_window_variation(correlation_matrices: dict) -> None:
    """Évolution du coefficient de corrélation d'une paire d'actifs d'une fenêtre à l'autre."""
    st.subheader("📈 Variation de corrélation entre deux actifs")
    st.caption(
        "Compare le coefficient de corrélation de deux actifs sur les différentes fenêtres temporelles "
        "pour voir si leur relation se renforce ou s'affaiblit avec le temps."
    )

    windows = [w for w in DEFAULT_WINDOW_ORDER if w in correlation_matrices]
    if not windows:
        st.info("Données de corrélation indisponibles. Lance le pipeline Rust (`cargo run`).")
        return

    all_assets = sorted({asset for w in windows for asset in correlation_matrices[w]})
    if len(all_assets) < 2:
        st.info("Pas assez d'actifs pour comparer.")
        return

    col1, col2 = st.columns(2)
    asset_a = col1.selectbox("Actif A", all_assets, index=0)
    asset_b = col2.selectbox("Actif B", all_assets, index=1)

    if asset_a == asset_b:
        st.warning("Sélectionne deux actifs différents pour voir leur corrélation.")
        return

    # Structure d'une matrice : { "90d": { "BTC": { "ETH": 0.8 } } }
    points = [
        (WINDOW_LABELS[w], correlation_matrices[w].get(asset_a, {}).get(asset_b))
        for w in windows
    ]
    points = [(label, v) for label, v in points if v is not None]
    if not points:
        st.info(f"Corrélation non calculable entre {asset_a} et {asset_b} (données historiques insuffisantes).")
        return

    labels, values = zip(*points)
    fig = go.Figure(go.Scatter(
        x=list(labels),
        y=list(values),
        mode="lines+markers+text",
        text=[f"{v:.2f}" for v in values],
        textposition="top center",
        line=dict(color=PASTEL_DARK_SEQUENCE[0], width=3, shape="spline"),
        marker=dict(size=10, color=PASTEL_DARK_SEQUENCE[2], line=dict(width=2, color=BG_COLOR)),
    ))
    fig.update_layout(
        title=f"Évolution de la corrélation : {asset_a} vs {asset_b}",
        xaxis_title="Fenêtre temporelle",
        yaxis_title="Coefficient de corrélation",
        yaxis=dict(range=[-1.1, 1.1], dtick=0.5),
        height=450,
        margin=dict(l=20, r=20, t=60, b=20),
    )
    fig.add_hline(y=0, line_color=GRID_COLOR, line_width=1, line_dash="dash")  # voir les décorrélations
    st.plotly_chart(fig, use_container_width=True)

    st.markdown("""
    **Comment l'interpréter ?**
    - **Corrélation qui monte sur le long terme (1 an)** : Les actifs sont de plus en plus liés structurellement (ex: deux actions tech qui suivent le même secteur).
    - **Corrélation qui baisse sur le long terme** : Les actifs se décorrelent, c'est excellent pour la diversification de ton portefeuille.
    - **Corrélation négative** : L'actif B a tendance à monter quand l'actif A descend (couvre-risque).
    """)


def render_correlation_page() -> None:
    st.title("🔗 Corrélations")

    data, df = load_portfolio()
    df = filter_small_positions(df, MIN_POSITION_VALUE_EUR)
    if df.empty:
        st.info(f"Aucune position d'au moins {MIN_POSITION_VALUE_EUR:.0f} €.")
        st.stop()

    render_correlation_section(df, data)
    st.divider()
    render_window_variation(data.get("correlation_matrices", {}))
