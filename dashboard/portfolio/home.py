"""Page d'accueil (vue globale) : KPIs, allocation, P&L par actif, tableau des positions.

Les figures sont construites par des fonctions pures (`allocation_figure`, `pnl_by_asset_figure`)
que la page « Vue par tag » réutilise pour le détail d'un tag.
"""
from __future__ import annotations

import pandas as pd
import plotly.express as px
import plotly.graph_objects as go
import streamlit as st

from portfolio.fmt import fmt_eur, fmt_number, fmt_pct
from portfolio.theme import GAIN_COLOR, LOSS_COLOR, SURFACE_COLOR

COLOR_MAP = {"Gain": GAIN_COLOR, "Perte": LOSS_COLOR}
MARGIN = dict(l=10, r=10, t=40, b=10)

# Colonnes du tableau des positions : (nom dans le DataFrame, libellé affiché), dans l'ordre d'affichage.
POSITION_COLUMNS = {
    "symbol": "Symbole",
    "kind": "Type",
    "quantity": "Quantité",
    "price_eur": "Prix",
    "value_eur": "Valeur",
    "weight_pct": "Poids (%)",
    "cost_basis_eur": "Cost Basis",
    "pnl_eur": "P&L latent",
    "realized_pnl_eur": "P&L réalisé",
    "pnl_total_eur": "P&L total",
    "pnl_total_pct": "Perf. totale (%)",
}
_PNL_COLUMNS = ["P&L latent", "P&L réalisé", "P&L total", "Perf. totale (%)"]


# --- KPIs -------------------------------------------------------------------------

def _ratio_pct(numerator: float, denominator: float) -> float | None:
    """Pourcentage, ou None si la base n'est pas strictement positive."""
    return numerator / denominator * 100 if denominator > 0 else None


def _signed_delta(amount: float, pct: float | None) -> str:
    """« +1 234 € (+5,2 %) » : le signe pilote la couleur (vert/rouge) du delta de st.metric."""
    text = fmt_eur(amount, decimals=0, signed=True)
    return text if pct is None else f"{text} ({fmt_pct(pct)})"


def render_kpis(kpis: dict, global_kpis: dict | None = None) -> None:
    """Ligne principale (3 chiffres clés) puis ligne secondaire (détail du P&L et cash)."""
    st.title("📈 Mon Portefeuille")

    cost = kpis["total_cost_basis_eur"]
    invested = kpis.get("total_invested_eur") or cost  # capital engagé : lots ouverts + lots vendus
    realized_cost = max(invested - cost, 0.0)
    latent = kpis["total_pnl_latent_eur"]
    realized = kpis["total_pnl_realized_eur"]
    total = kpis["total_pnl_eur"]
    latent_pct = _ratio_pct(latent, cost)
    realized_pct = _ratio_pct(realized, realized_cost)
    total_pct = _ratio_pct(total, invested)

    # --- Ligne principale ---
    h1, h2, h3 = st.columns(3)
    with h1.container(border=True):
        st.metric(
            "Valeur du portefeuille", fmt_eur(kpis["total_value_eur"]),
            delta=_signed_delta(latent, latent_pct),
            help="Titres + crypto, hors cash. Le delta est le P&L latent (valeur - cost basis).",
        )
    with h2.container(border=True):
        if global_kpis is not None:
            ret = global_kpis["return_pct"]
            st.metric(
                "Performance globale (cash inclus)",
                fmt_pct(ret, decimals=2) if ret is not None else "n/a",
                delta=fmt_eur(global_kpis["gain_eur"], signed=True),
                help="(P&L latent + réalisé + dividendes nets) / (cost basis des lots ouverts + cash XTB). "
                     "Le cash Binance n'est pas suivi.",
            )
        else:
            st.metric(
                "Performance globale (hors cash)",
                fmt_pct(total_pct, decimals=2),
                delta=fmt_eur(total, signed=True),
                help="(P&L latent + réalisé) / capital engagé.",
            )
    with h3.container(border=True):
        if global_kpis is not None:
            st.metric("Titres + crypto + cash", fmt_eur(global_kpis["equity_eur"]),
                      help="Valeur du portefeuille + cash XTB disponible.")
        else:
            st.metric("Capital engagé", fmt_eur(invested),
                      help="Cost basis des lots ouverts + coût des lots déjà vendus.")

    # --- Ligne secondaire ---
    with st.container(border=True):
        c1, c2, c3, c4, c5 = st.columns(5)
        c1.metric("Cost basis", fmt_eur(cost))
        c2.metric("P&L latent", fmt_eur(latent, signed=True), delta=fmt_pct(latent_pct) if latent_pct is not None else None,
                  help="Gain ou perte sur les lots encore détenus ; % rapporté au cost basis.")
        c3.metric("P&L réalisé", fmt_eur(realized, signed=True), delta=fmt_pct(realized_pct) if realized_pct is not None else None,
                  help="Gain ou perte sur les lots vendus ; % rapporté au coût de ces lots.")
        c4.metric("P&L total", fmt_eur(total, signed=True), delta=fmt_pct(total_pct) if total_pct is not None else None,
                  help="Latent + réalisé ; % rapporté au capital engagé (lots ouverts + lots vendus).")
        if global_kpis is not None:
            c5.metric("Cash XTB disponible", fmt_eur(global_kpis["cash_eur"]),
                      delta=f"{global_kpis['cash_share'] * 100:.0f} % du total",
                      delta_color="off", delta_arrow="off")


# --- Allocation ---------------------------------------------------------------------

def allocation_figure(df: pd.DataFrame, title: str | None = None) -> go.Figure:
    """Treemap type d'actif > symbole, surface proportionnelle à la valeur.

    Plus lisible qu'un camembert dès qu'il y a une dizaine de lignes. Le pourcentage affiché
    est la part du total de `df`.
    """
    fig = px.treemap(
        df, path=[px.Constant("Portefeuille"), "kind", "symbol"], values="value_eur", color="kind",
        color_discrete_map={"(?)": SURFACE_COLOR},  # tuile racine : discrète
        title=title,
    )
    fig.update_traces(
        textinfo="label+percent root",
        hovertemplate="%{label}<br>%{value:,.2f} €<br>%{percentRoot:.1%} du total<extra></extra>",
    )
    fig.update_layout(margin=MARGIN if title else dict(l=10, r=10, t=10, b=10), height=480)
    return fig


def render_allocation(df: pd.DataFrame) -> None:
    st.plotly_chart(allocation_figure(df), use_container_width=True)

    # --- Export CSV : ticker + % d'allocation uniquement (pas de prix) ---
    total_value = float(df["value_eur"].sum())
    alloc_df = (
        df[["symbol", "value_eur"]]
        .assign(allocation_pct=lambda d: d["value_eur"] / total_value * 100 if total_value > 0 else 0.0)
        .drop(columns="value_eur")
        .rename(columns={"symbol": "ticker"})
        .sort_values("allocation_pct", ascending=False)
        .reset_index(drop=True)
    )
    alloc_df["allocation_pct"] = alloc_df["allocation_pct"].round(4)  # évite les bruits flottants

    st.download_button(
        label="📥 Télécharger l'allocation (CSV)",
        data=alloc_df.to_csv(index=False).encode("utf-8"),
        file_name="allocation.csv",
        mime="text/csv",
        help="Ticker et pourcentage d'allocation uniquement — aucune donnée de prix.",
    )


# --- P&L par actif --------------------------------------------------------------------

def pnl_by_asset_figure(df: pd.DataFrame, title: str | None = None) -> go.Figure:
    """Barres horizontales triées : le plus gros gain en haut, la plus grosse perte en bas."""
    d = df.sort_values("pnl_total_eur")  # ordre croissant : Plotly dessine la 1re ligne en bas
    fig = px.bar(
        d, x="pnl_total_eur", y="symbol", orientation="h", color="pnl_color", color_discrete_map=COLOR_MAP,
        labels={"pnl_total_eur": "P&L total (€)"}, title=title,
    )
    fig.update_traces(texttemplate="%{x:+,.0f} €", textposition="outside", cliponaxis=False)
    fig.update_xaxes(ticksuffix=" €")
    fig.update_yaxes(title_text="")
    fig.update_layout(
        showlegend=False,
        margin=dict(l=10, r=70, t=40 if title else 10, b=10),
        height=max(320, 28 * len(d) + 80),
    )
    return fig


def render_pnl_by_asset(df: pd.DataFrame) -> None:
    st.caption("P&L total par actif (latent + réalisé), en euros.")
    st.plotly_chart(pnl_by_asset_figure(df), use_container_width=True)


# --- Tableau des positions -------------------------------------------------------------

def positions_table(df: pd.DataFrame, weight_base: float | None = None) -> pd.DataFrame:
    """Positions triées par valeur, colonnes renommées en français.

    Les colonnes restent NUMÉRIQUES (le tri du tableau est correct) ; la mise en forme est faite
    par `style_positions`. `weight_base` : valeur totale servant de base au poids (par défaut, la
    somme des lignes du tableau).
    """
    d = df.copy()
    base = float(weight_base) if weight_base else float(d["value_eur"].sum())
    d["weight_pct"] = d["value_eur"] / base * 100 if base > 0 else 0.0
    d = d.sort_values("value_eur", ascending=False)
    return d[list(POSITION_COLUMNS)].rename(columns=POSITION_COLUMNS).reset_index(drop=True)


def _pnl_color(value) -> str:
    if value is None or pd.isna(value) or value == 0:
        return ""
    return f"color: {GAIN_COLOR}" if value > 0 else f"color: {LOSS_COLOR}"


def style_positions(table: pd.DataFrame):
    """Format français + P&L en vert/rouge. Le poids est géré par la ProgressColumn du rendu."""
    styler = table.style.format({
        "Quantité": lambda x: fmt_number(x, 4),
        "Prix": fmt_eur,
        "Valeur": fmt_eur,
        "Cost Basis": fmt_eur,
        "P&L latent": lambda x: fmt_eur(x, signed=True),
        "P&L réalisé": lambda x: fmt_eur(x, signed=True),
        "P&L total": lambda x: fmt_eur(x, signed=True),
        "Perf. totale (%)": lambda x: fmt_pct(x, decimals=2),
    }, na_rep="—")
    # Styler.map remplace applymap à partir de pandas 2.1.
    apply = getattr(styler, "map", None) or styler.applymap
    return apply(_pnl_color, subset=_PNL_COLUMNS)


def render_positions_table(df: pd.DataFrame, weight_base: float | None = None) -> None:
    table = positions_table(df, weight_base)
    st.dataframe(
        style_positions(table),
        hide_index=True,
        use_container_width=True,
        column_config={
            "Poids (%)": st.column_config.ProgressColumn(
                "Poids", format="%.1f %%", min_value=0.0, max_value=float(max(table["Poids (%)"].max(), 1.0)),
                help="Part de la valeur du portefeuille (hors cash)." if weight_base
                else "Part de la valeur de la sélection affichée.",
            ),
        },
    )
