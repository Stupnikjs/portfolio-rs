"""Page Historique : courbe de la valeur totale et courbe du cost basis.

Lit `data/history.json` (pipeline Rust, lecture seule). La logique pure
(filtrage, stats, figure) est séparée du rendu Streamlit pour être testable.
"""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st

from config import DATA_PATH
from data import build_assets_df, compute_kpis, load_data, load_history
from theme import BG_COLOR, GAIN_COLOR, GRID_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE, TEXT_COLOR

VALUE_COLOR = PASTEL_DARK_SEQUENCE[0]  # bleu
COST_COLOR = PASTEL_DARK_SEQUENCE[2]   # jaune

# Libellé -> nombre de jours affichés (None = tout l'historique).
PERIODS: dict[str, int | None] = {
    "1 mois": 31,
    "3 mois": 92,
    "6 mois": 183,
    "1 an": 366,
    "Tout": None,
}


# --- Logique pure ---------------------------------------------------------------

def filter_period(df: pd.DataFrame, days: int | None) -> pd.DataFrame:
    """Garde les `days` derniers jours (par rapport à la dernière date du df)."""
    if days is None or df.empty:
        return df
    cutoff = df["date"].max() - pd.Timedelta(days=days)
    return df[df["date"] >= cutoff].reset_index(drop=True)


def with_live_point(df: pd.DataFrame, kpis: dict, now: pd.Timestamp | None = None) -> pd.DataFrame:
    """Ajoute le point "maintenant" (valeurs de dashboard.json) à la fin du df.

    Sans effet si `now` n'est pas strictement postérieur à la dernière date.
    """
    now = now if now is not None else pd.Timestamp.now()
    out = df.copy()
    out["live"] = False
    if not out.empty and now <= out["date"].max():
        return out

    live_row = {
        "date": now,
        "total_value_eur": kpis["total_value_eur"],
        "total_cost_basis_eur": kpis["total_cost_basis_eur"],
        "total_pnl_eur": kpis["total_value_eur"] - kpis["total_cost_basis_eur"],
        "complete": True,
        "assets": None,
        "live": True,
    }
    return pd.concat([out, pd.DataFrame([live_row])], ignore_index=True)


def period_stats(df: pd.DataFrame) -> dict:
    """Valeurs de fin de période et variations par rapport au premier point."""
    first, last = df.iloc[0], df.iloc[-1]
    value, cost = float(last["total_value_eur"]), float(last["total_cost_basis_eur"])
    pnl = value - cost
    return {
        "value": value,
        "cost": cost,
        "pnl": pnl,
        "pnl_pct": pnl / cost * 100 if cost > 0 else 0.0,
        "value_delta": value - float(first["total_value_eur"]),
        "cost_delta": cost - float(first["total_cost_basis_eur"]),
    }


def build_history_figure(df: pd.DataFrame) -> go.Figure:
    """Deux courbes (valeur totale, cost basis) + croix rouges sur les points incomplets."""
    few_points = len(df) <= 40
    mode = "lines+markers" if few_points else "lines"

    fig = go.Figure()
    fig.add_trace(go.Scatter(
        x=df["date"], y=df["total_value_eur"], name="Valeur totale", mode=mode,
        line=dict(color=VALUE_COLOR, width=2.5), marker=dict(size=5),
        hovertemplate="%{y:,.2f} €",
    ))
    fig.add_trace(go.Scatter(
        x=df["date"], y=df["total_cost_basis_eur"], name="Cost basis", mode=mode,
        line=dict(color=COST_COLOR, width=2, dash="dash"), marker=dict(size=5),
        hovertemplate="%{y:,.2f} €",
    ))

    incomplete = df[~df["complete"]]
    if not incomplete.empty:
        fig.add_trace(go.Scatter(
            x=incomplete["date"], y=incomplete["total_value_eur"],
            name="Prix manquants/reportés", mode="markers",
            marker=dict(symbol="x", size=11, color=LOSS_COLOR, line=dict(width=2)),
            hovertemplate="Prix incomplet<extra></extra>",
        ))

    live = df[df["live"]] if "live" in df.columns else df.iloc[0:0]
    if not live.empty:
        fig.add_trace(go.Scatter(
            x=live["date"], y=live["total_value_eur"], name="Aujourd'hui", mode="markers",
            marker=dict(symbol="circle-open", size=12, color=GAIN_COLOR, line=dict(width=2)),
            hovertemplate="%{y:,.2f} € (valeur actuelle)<extra></extra>",
        ))

    fig.update_layout(
        height=480, hovermode="x unified",
        margin=dict(l=10, r=10, t=30, b=10),
        yaxis=dict(ticksuffix=" €", rangemode="tozero"),
        legend=dict(orientation="h", y=1.08, x=0),
    )
    return fig


def asset_snapshot_df(assets: list[dict]) -> pd.DataFrame:
    """Détail par actif d'un snapshot (pour le debug), trié par valeur décroissante."""
    cols = ["symbol", "kind", "quantity", "price_eur", "value_eur", "cost_basis_eur", "price_status"]
    df = pd.DataFrame(assets)
    for col in cols:
        if col not in df.columns:
            df[col] = None
    return df[cols].sort_values("value_eur", ascending=False).reset_index(drop=True)


# --- Rendu Streamlit ---------------------------------------------------------------

def _ensure_dark_template() -> None:
    if "portfolio_dark" not in pio.templates:
        pio.templates["portfolio_dark"] = dict(
            layout=dict(
                paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
                xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR),
            )
        )
    pio.templates.default = "portfolio_dark"


def _live_kpis() -> dict | None:
    """KPIs du jour (même convention que la page d'accueil : cash exclu)."""
    if not DATA_PATH.exists():
        return None
    return compute_kpis(build_assets_df(load_data()))


def render_history_page() -> None:
    _ensure_dark_template()
    st.title("📈 Historique")

    try:
        history = load_history()
    except ValueError as e:
        st.error(str(e))
        st.stop()

    if history is None:
        st.info("history.json introuvable. Lance d'abord le binaire Rust (`cargo run`).")
        st.stop()
    if history.empty:
        st.info("history.json est vide : pas encore de snapshot complet.")
        st.stop()

    c1, c2 = st.columns([3, 2])
    period = c1.radio("Période", list(PERIODS), index=len(PERIODS) - 1, horizontal=True, label_visibility="collapsed")
    kpis = _live_kpis()
    show_live = c2.checkbox("Ajouter le point actuel", value=kpis is not None, disabled=kpis is None,
                            help="Valeur et cost basis du jour (dashboard.json), cash exclu.")

    df = with_live_point(history, kpis) if (show_live and kpis) else history.assign(live=False)
    view = filter_period(df, PERIODS[period])

    if len(view) < 2:
        st.info("Pas assez de points sur cette période pour tracer une courbe.")
        st.stop()

    stats = period_stats(view)
    k1, k2, k3, k4 = st.columns(4)
    k1.metric("Valeur totale", f"{stats['value']:,.2f} €", f"{stats['value_delta']:+,.2f} €")
    k2.metric("Cost basis", f"{stats['cost']:,.2f} €", f"{stats['cost_delta']:+,.2f} €", delta_color="off")
    k3.metric("P&L", f"{stats['pnl']:+,.2f} €")
    k4.metric("Performance", f"{stats['pnl_pct']:+.2f} %")

    st.plotly_chart(build_history_figure(view), use_container_width=True)
    st.caption(
        "Écart entre les deux courbes = plus-value latente. Le cash est exclu. "
        "Les variations du cost basis correspondent aux achats/ventes."
    )

    n_bad = int((~view["complete"]).sum())
    if n_bad:
        st.warning(
            f"{n_bad} snapshot(s) avec un prix manquant ou reporté (croix rouges). "
            "Ils sont recalculés automatiquement au prochain `cargo run` ; détail par actif ci-dessous."
        )

    with st.expander("🔎 Détail par actif d'un snapshot"):
        with_assets = view[view["assets"].apply(lambda a: isinstance(a, list) and len(a) > 0)]
        if with_assets.empty:
            st.info("Aucun détail par actif (relance `cargo run -- --rebuild-history`).")
        else:
            choice = st.selectbox(
                "Date", list(with_assets.index[::-1]),
                format_func=lambda i: with_assets.loc[i, "date"].strftime("%d/%m/%Y")
                + ("" if with_assets.loc[i, "complete"] else "  ⚠"),
            )
            st.dataframe(
                asset_snapshot_df(with_assets.loc[choice, "assets"]),
                hide_index=True, use_container_width=True,
                column_config={
                    "symbol": "Actif", "kind": "Type",
                    "quantity": st.column_config.NumberColumn("Quantité", format="%.4f"),
                    "price_eur": st.column_config.NumberColumn("Prix", format="%.2f €"),
                    "value_eur": st.column_config.NumberColumn("Valeur", format="%.2f €"),
                    "cost_basis_eur": st.column_config.NumberColumn("Cost basis", format="%.2f €"),
                    "price_status": "Prix",
                },
            )