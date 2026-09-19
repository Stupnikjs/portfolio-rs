"""Regroupement automatique des actifs par similarité de comportement
(corrélation), sans classification a priori -- deux actifs très corrélés
finissent dans le même groupe peu importe leur type (Action/Crypto/...)."""
from __future__ import annotations

import numpy as np
import pandas as pd
from scipy.cluster.hierarchy import dendrogram, fcluster, linkage
from scipy.spatial.distance import squareform

import plotly.graph_objects as go

from theme import PASTEL_DARK_SEQUENCE


def distance_matrix(corr_matrix: pd.DataFrame) -> np.ndarray:
    """d = sqrt(2 * (1 - corr)) -- vraie métrique, contrairement à 1 - corr
    qui ne respecte pas l'inégalité triangulaire."""
    corr = corr_matrix.clip(-1, 1)
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


def cluster_table(cluster_labels: pd.Series) -> pd.DataFrame:
    """Une ligne par groupe, avec la liste des actifs qu'il contient."""
    grouped = cluster_labels.groupby(cluster_labels).apply(lambda s: ", ".join(sorted(s.index)))
    return pd.DataFrame({"Groupe": grouped.index, "Actifs": grouped.values})


def render_dendrogram(corr_matrix: pd.DataFrame, linkage_matrix: np.ndarray) -> go.Figure:
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


def cluster_summary(
    cluster_labels: pd.Series,
    df: pd.DataFrame,
    total_portfolio_value: float,
) -> pd.DataFrame:
    """Une ligne par groupe contenant des actifs du portefeuille, avec :
    - la liste des actifs détenus,
    - la valeur totale, le cost basis, le P&L (€), le P&L (%),
    - le poids du groupe dans le portefeuille global.

    Les actifs non détenus (benchmarks comme les indices ou matières premières)
    sont totalement ignorés. Les groupes ne contenant aucun actif réel
    n'apparaissent pas dans le résumé."""
    df_indexed = df.set_index("symbol")
    rows = []

    for cluster_id in sorted(cluster_labels.unique()):
        symbols_in_cluster = cluster_labels[cluster_labels == cluster_id].index.tolist()
        
        # On ne garde QUE les actifs qui sont réellement dans le portefeuille
        portfolio_assets = [s for s in symbols_in_cluster if s in df_indexed.index]

        if not portfolio_assets:
            # Si le groupe ne contient que des benchmarks (hors portefeuille), on l'ignore
            continue

        sub = df_indexed.loc[portfolio_assets]
        value = float(sub["value_eur"].sum())
        cost = float(sub["cost_basis_eur"].sum())
        pnl = float(sub["pnl_eur"].sum())

        pnl_pct = (pnl / cost * 100) if cost > 0 else 0.0
        weight = (value / total_portfolio_value * 100) if total_portfolio_value > 0 else 0.0

        rows.append({
            "Groupe": int(cluster_id),
            "Actifs": ", ".join(sorted(portfolio_assets)),
            "Nb actifs": len(portfolio_assets),
            "Valeur (€)": value,
            "Cost Basis (€)": cost,
            "P&L (€)": pnl,
            "P&L (%)": pnl_pct,
            "% Portefeuille": weight,
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