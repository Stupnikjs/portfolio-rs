"""Page Historique : plus-value latente dans le temps (valeur totale - cost basis).

Lit `data/history.json` (pipeline Rust, lecture seule). La logique pure
(filtrage, stats, figure) est séparée du rendu Streamlit pour être testable.
"""
from __future__ import annotations

import json
from pathlib import Path

import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from portfolio.config import DATA_PATH, HISTORY_PATH
from portfolio.data import build_assets_df, compute_kpis, load_data
from portfolio.theme import GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

LINE_COLOR = PASTEL_DARK_SEQUENCE[0]  # bleu : courbe de la plus-value latente

# Libellé -> nombre de jours affichés (None = tout l'historique).
PERIODS: dict[str, int | None] = {
    "1 mois": 31,
    "3 mois": 92,
    "6 mois": 183,
    "1 an": 366,
    "Tout": None,
}


# --- Chargement de history.json -------------------------------------------------

HISTORY_REQUIRED_COLUMNS = {"date", "total_value_eur", "total_cost_basis_eur"}


def history_to_df(raw: list[dict]) -> pd.DataFrame:
    """Transforme le contenu de history.json en DataFrame trié par date.

    - accepte l'ancien nom de colonne `week_end` (renommé en `date`) ;
    - `complete` absent (ancien format) -> True ;
    - un doublon de date garde la dernière entrée ;
    - lève ValueError si une colonne indispensable manque.
    """
    df = pd.DataFrame(raw)
    if df.empty:
        return pd.DataFrame(columns=sorted(HISTORY_REQUIRED_COLUMNS | {"total_pnl_eur", "complete", "assets"}))

    if "date" not in df.columns and "week_end" in df.columns:
        df = df.rename(columns={"week_end": "date"})

    missing = HISTORY_REQUIRED_COLUMNS - set(df.columns)
    if missing:
        raise ValueError(f"history.json : colonne(s) manquante(s) : {', '.join(sorted(missing))}")

    df["date"] = pd.to_datetime(df["date"])

    df["complete"] = df["complete"].fillna(True).astype(bool) if "complete" in df.columns else True
    if "total_pnl_eur" not in df.columns:
        df["total_pnl_eur"] = df["total_value_eur"] - df["total_cost_basis_eur"]
    if "assets" not in df.columns:
        df["assets"] = None

    return (
        df.sort_values("date")
        .drop_duplicates(subset="date", keep="last")
        .reset_index(drop=True)
    )


def load_history(path: Path = HISTORY_PATH) -> pd.DataFrame | None:
    """Charge history.json (pipeline Rust). Renvoie None si le fichier n'existe pas."""
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as f:
        return history_to_df(json.load(f))


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


def unrealized_gain(df: pd.DataFrame) -> pd.Series:
    """Écart valeur totale - cost basis (plus-value latente), en EUR, pour chaque point."""
    return df["total_value_eur"] - df["total_cost_basis_eur"]


def split_at_zero(x: pd.Series, y: pd.Series) -> tuple[list, list[float]]:
    """Série (x, y) avec un point interpolé à y = 0 à chaque changement de signe.

    Permet de remplir proprement la partie positive et la partie négative d'une courbe
    sans qu'un segment qui traverse zéro déborde de l'autre côté.
    """
    xs, ys = list(x), [float(v) for v in y]
    out_x, out_y = [], []
    for i, (xi, yi) in enumerate(zip(xs, ys)):
        if i > 0 and ys[i - 1] * yi < 0:
            share = ys[i - 1] / (ys[i - 1] - yi)
            out_x.append(xs[i - 1] + (xi - xs[i - 1]) * share)
            out_y.append(0.0)
        out_x.append(xi)
        out_y.append(yi)
    return out_x, out_y


def build_history_figure(df: pd.DataFrame) -> go.Figure:
    """Plus-value latente (valeur - cost basis) : courbe + remplissage vert (gain) / rouge (perte).

    La première trace est la courbe elle-même, un point par ligne de `df` ; viennent ensuite
    les remplissages, puis les croix rouges (points incomplets) et le point actuel.
    """
    few_points = len(df) <= 40
    mode = "lines+markers" if few_points else "lines"
    gain = unrealized_gain(df)

    fig = go.Figure()
    fig.add_trace(go.Scatter(
        x=df["date"], y=gain, name="Plus-value latente", mode=mode,
        line=dict(color=LINE_COLOR, width=2.5), marker=dict(size=5),
        hovertemplate="%{y:+,.2f} €",
    ))

    fill_x, fill_y = split_at_zero(df["date"], gain)
    for name, color, values in (
        ("Gain", GAIN_COLOR, [max(v, 0.0) for v in fill_y]),
        ("Perte", LOSS_COLOR, [min(v, 0.0) for v in fill_y]),
    ):
        fig.add_trace(go.Scatter(
            x=fill_x, y=values, name=name, mode="lines", line=dict(width=0),
            fill="tozeroy", fillcolor=_with_alpha(color, 0.25),
            showlegend=False, hoverinfo="skip",
        ))
    fig.add_hline(y=0, line=dict(color="rgba(150,150,150,0.6)", width=1))

    incomplete = df[~df["complete"]]
    if not incomplete.empty:
        fig.add_trace(go.Scatter(
            x=incomplete["date"], y=unrealized_gain(incomplete),
            name="Prix manquants/reportés", mode="markers",
            marker=dict(symbol="x", size=11, color=LOSS_COLOR, line=dict(width=2)),
            hovertemplate="Prix incomplet<extra></extra>",
        ))

    live = df[df["live"]] if "live" in df.columns else df.iloc[0:0]
    if not live.empty:
        fig.add_trace(go.Scatter(
            x=live["date"], y=unrealized_gain(live), name="Aujourd'hui", mode="markers",
            marker=dict(symbol="circle-open", size=12, color=GAIN_COLOR, line=dict(width=2)),
            hovertemplate="%{y:+,.2f} € (actuel)<extra></extra>",
        ))

    fig.update_layout(
        height=480, hovermode="x unified",
        margin=dict(l=10, r=10, t=30, b=10),
        yaxis=dict(ticksuffix=" €", zeroline=False),
        legend=dict(orientation="h", y=1.08, x=0),
    )
    return fig


def _with_alpha(color: str, alpha: float) -> str:
    """'#RRGGBB' -> 'rgba(r, g, b, alpha)' (plotly n'accepte pas l'alpha en hexadécimal 6 chiffres)."""
    h = color.lstrip("#")
    r, g, b = (int(h[i:i + 2], 16) for i in (0, 2, 4))
    return f"rgba({r}, {g}, {b}, {alpha})"


def asset_snapshot_df(assets: list[dict]) -> pd.DataFrame:
    """Détail par actif d'un snapshot (pour le debug), trié par valeur décroissante."""
    cols = ["symbol", "kind", "quantity", "price_eur", "value_eur", "cost_basis_eur", "price_status"]
    df = pd.DataFrame(assets)
    for col in cols:
        if col not in df.columns:
            df[col] = None
    return df[cols].sort_values("value_eur", ascending=False).reset_index(drop=True)


# --- Rendu Streamlit ---------------------------------------------------------------

def _live_kpis() -> dict | None:
    """KPIs du jour (même convention que la page d'accueil : cash exclu)."""
    if not DATA_PATH.exists():
        return None
    return compute_kpis(build_assets_df(load_data()))


def render_history_page() -> None:
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
        "Courbe = valeur totale - cost basis (plus-value latente), cash exclu. Un achat ne la modifie pas "
        "(valeur et cost basis montent ensemble) ; elle bouge avec les prix, et redescend quand tu "
        "vends des positions gagnantes (la plus-value devient réalisée)."
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