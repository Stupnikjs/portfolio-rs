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