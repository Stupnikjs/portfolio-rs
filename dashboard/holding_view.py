"""Page Détention : durée de détention mise en regard de la performance.

Lit `holdings`, `closed_sales` et `assets` dans dashboard.json (pipeline Rust,
lecture seule). Logique pure (DataFrames, figures) séparée du rendu Streamlit,
comme `history_view.py`.

Trois vues :
1. durée de détention vs P&L % de chaque position ouverte (bulle = valeur) ;
2. répartition du capital par tranche d'âge ;
3. durée des ventes gagnantes vs perdantes (biais « je garde mes perdants »),
   + rendement annualisé en colonne du tableau.
"""
from __future__ import annotations

import math

import pandas as pd
import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st

from config import MIN_POSITION_VALUE_EUR
from data import load_data
from theme import BG_COLOR, GAIN_COLOR, GRID_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE, TEXT_COLOR

# Sous ce seuil, une annualisation amplifie du bruit : on affiche « — ».
MIN_DAYS_ANNUALIZED = 30
# Sous ce nombre de ventes par camp, la comparaison gagnants/perdants est anecdotique.
MIN_SALES_PER_SIDE = 5

# (borne basse incluse, borne haute exclue, libellé)
AGE_BUCKETS = [
    (0, 90, "< 3 mois"),
    (90, 365, "3 à 12 mois"),
    (365, 1095, "1 à 3 ans"),
    (1095, math.inf, "> 3 ans"),
]
BUCKET_COLORS = [PASTEL_DARK_SEQUENCE[2], PASTEL_DARK_SEQUENCE[1], PASTEL_DARK_SEQUENCE[0], PASTEL_DARK_SEQUENCE[3]]


# --- Logique pure ---------------------------------------------------------------

def format_days(days: float | None) -> str:
    """373 -> '1 an 8 j' ; 45 -> '1 mois 15 j' ; 6 -> '6 j' ; None/NaN -> '—'."""
    if days is None or (isinstance(days, float) and math.isnan(days)):
        return "—"
    d = int(round(days))
    years, rest = divmod(d, 365)
    months, rest = divmod(rest, 30)
    parts = []
    if years:
        parts.append(f"{years} an{'s' if years > 1 else ''}")
    if months:
        parts.append(f"{months} mois")
    if rest or not parts:
        parts.append(f"{rest} j")
    return " ".join(parts[:2])


def annualized_pct(pnl_pct: float | None, days: float | None) -> float | None:
    """P&L % ramené à un an : (1 + r)^(365/jours) - 1. None si < 30 jours ou perte totale."""
    if pnl_pct is None or days is None or pd.isna(pnl_pct) or pd.isna(days):
        return None
    if days < MIN_DAYS_ANNUALIZED or pnl_pct <= -100:
        return None
    return ((1 + pnl_pct / 100) ** (365 / days) - 1) * 100


def holdings_df(data: dict) -> pd.DataFrame:
    df = pd.DataFrame(data.get("holdings") or [])
    if df.empty:
        return df
    df["first_acquired"] = pd.to_datetime(df["first_acquired"])
    return df.sort_values("symbol").reset_index(drop=True)


def held_symbols(data: dict, min_value_eur: float = MIN_POSITION_VALUE_EUR) -> list[str]:
    """Actifs encore détenus dont la valeur actuelle atteint le seuil (poussière écartée)."""
    holdings = holdings_df(data)
    if holdings.empty:
        return []
    values = {a["symbol"]: a["value_eur"] for a in data.get("assets") or []}
    held = holdings.loc[holdings["open_quantity"] > 0, "symbol"]
    return [s for s in held if values.get(s, 0.0) >= min_value_eur]


def positions_df(data: dict, symbols: list[str] | None = None) -> pd.DataFrame:
    """Positions ouvertes avec leur performance : holdings joints aux assets sur le symbole.

    On écarte les actifs sans cost basis (P&L % indéfini côté Rust : il vaut 0).
    """
    holdings = holdings_df(data)
    assets = pd.DataFrame(data.get("assets") or [])
    if holdings.empty or assets.empty:
        return pd.DataFrame()
    pos = holdings[holdings["open_avg_days"].notna()].merge(
        assets[["symbol", "pnl_pct", "pnl_eur", "value_eur", "cost_basis_eur"]], on="symbol", how="inner"
    )
    pos = pos[pos["cost_basis_eur"] > 0]
    if symbols is not None:
        pos = pos[pos["symbol"].isin(symbols)]
    pos = pos.reset_index(drop=True)
    pos["annualized_pct"] = [annualized_pct(p, d) for p, d in zip(pos["pnl_pct"], pos["open_avg_days"])]
    return pos


def age_bucket_df(holdings: pd.DataFrame, symbols: list[str] | None = None) -> pd.DataFrame:
    """Valeur actuelle du portefeuille répartie par tranche d'âge des lots ouverts."""
    rows = []
    for _, h in holdings.iterrows():
        if symbols is not None and h["symbol"] not in symbols:
            continue
        for lot in h["open_lots"] or []:
            if lot.get("value_eur") is not None:
                rows.append((lot["days"], lot["value_eur"]))
    lots = pd.DataFrame(rows, columns=["days", "value_eur"])
    total = lots["value_eur"].sum()
    out = []
    for low, high, label in AGE_BUCKETS:
        v = lots.loc[(lots["days"] >= low) & (lots["days"] < high), "value_eur"].sum()
        out.append({"bucket": label, "value_eur": v, "share_pct": v / total * 100 if total > 0 else 0.0})
    return pd.DataFrame(out)


def sales_df(data: dict) -> pd.DataFrame:
    df = pd.DataFrame(data.get("closed_sales") or [])
    if df.empty:
        return df
    df["sell_date"] = pd.to_datetime(df["sell_date"])
    return df


def reliable_sales(sales: pd.DataFrame) -> pd.DataFrame:
    """Ventes complètes, avec une durée et un résultat exploitables (l'incomplet surestime le P&L)."""
    if sales.empty:
        return sales
    ok = sales[~sales["incomplete"] & sales["days"].notna()]
    return ok.reset_index(drop=True)


def winners_losers(sales: pd.DataFrame) -> dict:
    """Durée moyenne/médiane de détention des ventes gagnantes vs perdantes (pnl exactement 0 ignoré)."""
    ok = reliable_sales(sales)
    out = {}
    for name, mask in (("winners", ok["pnl_eur"] > 0), ("losers", ok["pnl_eur"] < 0)) if not ok.empty else ():
        d = ok.loc[mask, "days"]
        out[name] = {
            "n": int(len(d)),
            "mean_days": float(d.mean()) if len(d) else None,
            "median_days": float(d.median()) if len(d) else None,
        }
    out.setdefault("winners", {"n": 0, "mean_days": None, "median_days": None})
    out.setdefault("losers", {"n": 0, "mean_days": None, "median_days": None})
    return out


def winners_losers_message(wl: dict) -> tuple[str, str]:
    """(niveau, texte) : 'info' | 'warning'. Prudent sur les petits échantillons."""
    w, l = wl["winners"], wl["losers"]
    if w["n"] == 0 or l["n"] == 0:
        return "info", "Il faut au moins une vente gagnante et une perdante pour comparer."
    if min(w["n"], l["n"]) < MIN_SALES_PER_SIDE:
        return "info", (
            f"Échantillon trop petit ({w['n']} gagnantes, {l['n']} perdantes) pour conclure : "
            f"il faut au moins {MIN_SALES_PER_SIDE} ventes de chaque côté."
        )
    ratio = l["mean_days"] / w["mean_days"] if w["mean_days"] else math.inf
    if ratio >= 1.3:
        return "warning", (
            f"Tu gardes tes perdantes {ratio:.1f}× plus longtemps que tes gagnantes "
            f"({format_days(l['mean_days'])} contre {format_days(w['mean_days'])} en moyenne) : "
            "signe classique de gains coupés trop tôt et de pertes laissées courir."
        )
    if ratio <= 1 / 1.3:
        return "info", (
            f"Tu coupes tes perdantes plus vite que tes gagnantes ({format_days(l['mean_days'])} "
            f"contre {format_days(w['mean_days'])}) : discipline cohérente."
        )
    return "info", (
        f"Durées proches entre gagnantes ({format_days(w['mean_days'])}) et perdantes "
        f"({format_days(l['mean_days'])}) : pas de biais net."
    )


def build_scatter_figure(pos: pd.DataFrame) -> go.Figure:
    """Détention moyenne (x) vs P&L % (y) ; taille de bulle = valeur, couleur = signe du P&L."""
    colors = [GAIN_COLOR if v >= 0 else LOSS_COLOR for v in pos["pnl_pct"]]
    sizeref = 2.0 * pos["value_eur"].max() / (46.0**2) if len(pos) else 1.0
    fig = go.Figure(go.Scatter(
        x=pos["open_avg_days"], y=pos["pnl_pct"], mode="markers+text", text=pos["symbol"],
        textposition="top center",
        marker=dict(size=pos["value_eur"], sizemode="area", sizeref=sizeref, sizemin=8,
                    color=colors, line=dict(width=1, color=BG_COLOR)),
        customdata=list(zip(
            pos["symbol"], [format_days(v) for v in pos["open_avg_days"]],
            pos["value_eur"], pos["pnl_eur"],
            ["—" if pd.isna(a) else f"{a:+.1f} %" for a in pos["annualized_pct"]],
        )),
        hovertemplate=(
            "%{customdata[0]}<br>Détention moy. %{customdata[1]}<br>P&L %{y:+.1f} %"
            " (%{customdata[3]:+,.0f} €)<br>Valeur %{customdata[2]:,.0f} €"
            "<br>Annualisé %{customdata[4]}<extra></extra>"
        ),
    ))
    fig.add_hline(y=0, line_color=GRID_COLOR)
    fig.update_layout(
        height=460, margin=dict(l=10, r=10, t=30, b=10), showlegend=False,
        xaxis=dict(title="Détention moyenne (jours, pondérée par le capital)"),
        yaxis=dict(title="P&L latent", ticksuffix=" %", zeroline=False),
    )
    return fig


def build_age_figure(buckets: pd.DataFrame) -> go.Figure:
    """Une seule barre empilée : part de la valeur du portefeuille par tranche d'âge."""
    fig = go.Figure()
    for (_, b), color in zip(buckets.iterrows(), BUCKET_COLORS):
        if b["value_eur"] <= 0:
            continue
        fig.add_trace(go.Bar(
            y=["Portefeuille"], x=[b["share_pct"]], name=b["bucket"], orientation="h", marker_color=color,
            text=[f"{b['share_pct']:.0f} %"], textposition="inside", insidetextanchor="middle",
            customdata=[[b["value_eur"]]],
            hovertemplate=f"{b['bucket']} : %{{x:.1f}} % (%{{customdata[0]:,.0f}} €)<extra></extra>",
        ))
    fig.update_layout(
        barmode="stack", height=170, margin=dict(l=10, r=10, t=10, b=10),
        xaxis=dict(range=[0, 100], ticksuffix=" %"), yaxis=dict(showticklabels=False),
        legend=dict(orientation="h", y=-0.35, x=0),
    )
    return fig


def build_winners_losers_figure(wl: dict) -> go.Figure:
    labels = [f"Gagnantes (n={wl['winners']['n']})", f"Perdantes (n={wl['losers']['n']})"]
    fig = go.Figure()
    for key, name, color in (("mean_days", "Moyenne", PASTEL_DARK_SEQUENCE[0]),
                             ("median_days", "Médiane", PASTEL_DARK_SEQUENCE[2])):
        vals = [wl["winners"][key], wl["losers"][key]]
        fig.add_trace(go.Bar(
            x=labels, y=vals, name=name, marker_color=color,
            text=[format_days(v) for v in vals], textposition="outside",
        ))
    fig.update_layout(
        barmode="group", height=340, margin=dict(l=10, r=10, t=30, b=10),
        yaxis=dict(title="Jours de détention"), legend=dict(orientation="h", y=1.1, x=0),
    )
    return fig


def display_table(pos: pd.DataFrame) -> pd.DataFrame:
    out = pd.DataFrame({
        "Actif": pos["symbol"],
        "Détention moyenne": pos["open_avg_days"].map(format_days),
        "Plus vieux lot": pos["open_oldest_days"].map(format_days),
        "P&L latent (%)": pos["pnl_pct"],
        "Annualisé (%)": pos["annualized_pct"],
        "Valeur (€)": pos["value_eur"],
        "_days": pos["open_avg_days"],
    })
    return out.sort_values("_days", ascending=False).drop(columns="_days").reset_index(drop=True)


# --- Rendu Streamlit ---------------------------------------------------------------

def _ensure_dark_template() -> None:
    if "portfolio_dark" not in pio.templates:
        pio.templates["portfolio_dark"] = dict(layout=dict(
            paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
            xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR),
        ))
    pio.templates.default = "portfolio_dark"


def render_holding_page() -> None:
    _ensure_dark_template()
    st.title("⏳ Durée de détention")

    data = load_data()
    holdings = holdings_df(data)
    if holdings.empty:
        st.info("Aucune donnée `holdings` dans dashboard.json : relance `cargo run`.")
        st.stop()

    held = held_symbols(data)
    if not held:
        st.info(f"Aucune position détenue d'au moins {MIN_POSITION_VALUE_EUR:.0f} €.")
        st.stop()
    symbols = st.multiselect(
        f"Actifs d'au moins {MIN_POSITION_VALUE_EUR:.0f} € (vues 1 et 2 et tableau)", held, default=held
    )
    if not symbols:
        st.info("Sélectionne au moins un actif.")
        st.stop()

    # --- 1. Détention vs performance ---
    st.subheader("1. Détention vs performance")
    pos = positions_df(data, symbols)
    if pos.empty:
        st.info("Aucune position ouverte avec un cost basis connu.")
    else:
        st.plotly_chart(build_scatter_figure(pos), use_container_width=True)
        st.caption(
            "En haut à droite : gagnants tenus longtemps. En bas à droite : perdants que tu gardes (à "
            "réexaminer). À gauche : positions trop jeunes pour un verdict. Taille de bulle = valeur."
        )

    # --- 2. Capital par tranche d'âge ---
    st.subheader("2. Capital par tranche d'âge")
    buckets = age_bucket_df(holdings, symbols)
    if buckets["value_eur"].sum() > 0:
        st.plotly_chart(build_age_figure(buckets), use_container_width=True)
        young = buckets.loc[buckets["bucket"] == "< 3 mois", "share_pct"].iloc[0]
        st.caption(f"{young:.0f} % de la valeur sélectionnée est détenue depuis moins de 3 mois (non éprouvée). "
                   "Valeur actuelle des lots encore ouverts, calcul FIFO.")
    else:
        st.info("Pas de lot valorisé pour la sélection.")

    # --- 3. Gagnantes vs perdantes ---
    st.subheader("3. Durée des ventes gagnantes vs perdantes")
    sales = sales_df(data)
    wl = winners_losers(sales)
    level, message = winners_losers_message(wl)
    if wl["winners"]["n"] and wl["losers"]["n"]:
        st.plotly_chart(build_winners_losers_figure(wl), use_container_width=True)
    (st.warning if level == "warning" else st.info)(message)
    st.caption("Toutes les ventes de titres complètes, indépendamment du filtre (c'est un comportement "
               "global). Les ventes dépassant les lots connus sont exclues.")

    # --- Tableau ---
    if not pos.empty:
        st.subheader("Détail par position")
        st.dataframe(
            display_table(pos), hide_index=True, use_container_width=True,
            column_config={
                "P&L latent (%)": st.column_config.NumberColumn(format="%+.1f %%"),
                "Annualisé (%)": st.column_config.NumberColumn(format="%+.1f %%"),
                "Valeur (€)": st.column_config.NumberColumn(format="%.0f €"),
            },
        )
        st.caption(f"Annualisé = P&L ramené à un an sur la détention moyenne ; « — » sous {MIN_DAYS_ANNUALIZED} "
                   "jours. Approximation pour les positions construites en plusieurs achats.")