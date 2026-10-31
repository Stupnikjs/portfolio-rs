"""Page Détention : durée de détention mise en regard de la performance.

Lit `holdings`, `closed_sales` et `assets` dans dashboard.json (pipeline Rust,
lecture seule). Logique pure (DataFrames, figures) séparée du rendu Streamlit,
comme `history.py`.

Vues :
0. bandeau de KPI (âge moyen, plus vieux lot, capital jeune, lots, annualisé) ;
1. durée de détention vs P&L % ou annualisé de chaque position ouverte
   (bulle = valeur, couleur = classe d'actif) ;
2. répartition du capital par tranche d'âge ;
3. ventes : statistiques de trading (taux de réussite, gain/perte moyens,
   espérance) + durée des ventes gagnantes vs perdantes (biais « je garde mes
   perdants »), moyenne simple, médiane et moyenne pondérée par le capital ;
4. positions soldées et ventes par actif ;
+ tableau détaillé des positions ouvertes (poids, lots, dernier achat, P&L €).
"""
from __future__ import annotations

import math

import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from portfolio.config import MIN_POSITION_VALUE_EUR
from portfolio.data import load_data
from portfolio.theme import BG_COLOR, GAIN_COLOR, GRID_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

# Sous ce seuil, une annualisation amplifie du bruit : on affiche « — ».
MIN_DAYS_ANNUALIZED = 30
# Sous ce nombre de ventes par camp, la comparaison gagnants/perdants est anecdotique.
MIN_SALES_PER_SIDE = 5
# Sous ce nombre de ventes décisives (gagnantes + perdantes), les stats de trading sont indicatives.
MIN_SALES_FOR_STATS = 10
# Une position dont le reliquat ouvert est sous ce seuil (poussière de quantité) est considérée soldée.
CLOSED_QUANTITY_EPS = 1e-9
# Nombre d'étiquettes affichées sur le scatter (les plus grosses positions) : évite le chevauchement.
LABEL_TOP_N = 10

# (borne basse incluse, borne haute exclue, libellé)
AGE_BUCKETS = [
    (0, 90, "< 3 mois"),
    (90, 365, "3 à 12 mois"),
    (365, 1095, "1 à 3 ans"),
    (1095, math.inf, "> 3 ans"),
]
BUCKET_COLORS = [PASTEL_DARK_SEQUENCE[2], PASTEL_DARK_SEQUENCE[1], PASTEL_DARK_SEQUENCE[0], PASTEL_DARK_SEQUENCE[3]]
YOUNG_DAYS = AGE_BUCKETS[0][1]

# Couleurs par classe d'actif : on évite le vert/rouge, réservés au signe du P&L.
KIND_SEQUENCE = [PASTEL_DARK_SEQUENCE[i] for i in (0, 4, 2, 5, 7, 6)]

# Axe vertical du scatter : clé de colonne -> libellé.
Y_METRICS = {"pnl_pct": "P&L latent", "annualized_pct": "P&L annualisé"}
# Graduations de l'axe de détention (jours, libellé).
X_TICKS = [(7, "1 sem"), (30, "1 mois"), (90, "3 mois"), (365, "1 an"), (1095, "3 ans")]


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


def _none_if_nan(value) -> float | None:
    return None if value is None or pd.isna(value) else float(value)


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
    Colonnes ajoutées : annualized_pct, n_lots, youngest_days (âge du lot le plus récent)
    et weight_pct (poids de la position dans la sélection, en valeur).
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
    pos["n_lots"] = [len(lots or []) for lots in pos["open_lots"]]
    pos["youngest_days"] = [
        min((lot["days"] for lot in lots), default=None) if lots else None for lots in pos["open_lots"]
    ]
    total_value = pos["value_eur"].sum()
    pos["weight_pct"] = pos["value_eur"] / total_value * 100 if total_value > 0 else 0.0
    return pos


def open_lots_df(holdings: pd.DataFrame, symbols: list[str] | None = None) -> pd.DataFrame:
    """Un lot ouvert par ligne : symbol, days, cost_eur, value_eur (NaN si prix inconnu)."""
    rows = []
    for _, h in holdings.iterrows():
        if symbols is not None and h["symbol"] not in symbols:
            continue
        for lot in h["open_lots"] or []:
            value = lot.get("value_eur")
            rows.append((h["symbol"], lot["days"], lot["cost_eur"], float("nan") if value is None else value))
    return pd.DataFrame(rows, columns=["symbol", "days", "cost_eur", "value_eur"])


def age_bucket_df(holdings: pd.DataFrame, symbols: list[str] | None = None) -> pd.DataFrame:
    """Valeur actuelle du portefeuille répartie par tranche d'âge des lots ouverts."""
    lots = open_lots_df(holdings, symbols).dropna(subset=["value_eur"])
    total = lots["value_eur"].sum()
    out = []
    for low, high, label in AGE_BUCKETS:
        v = lots.loc[(lots["days"] >= low) & (lots["days"] < high), "value_eur"].sum()
        out.append({"bucket": label, "value_eur": v, "share_pct": v / total * 100 if total > 0 else 0.0})
    return pd.DataFrame(out)


def portfolio_kpis(lots: pd.DataFrame, pos: pd.DataFrame) -> dict:
    """Résumé de la sélection : âge moyen (pondéré capital), plus vieux lot, part de capital jeune,
    nombre de lots et rendement annualisé du portefeuille (approximation : P&L agrégé ramené à un an
    sur la détention moyenne pondérée, comme l'annualisé par position)."""
    kpis = {"avg_age_days": None, "oldest_days": None, "n_lots": 0, "young_share_pct": None, "annualized_pct": None}
    if not lots.empty:
        kpis["n_lots"] = int(len(lots))
        kpis["oldest_days"] = float(lots["days"].max())
        cost = lots["cost_eur"].sum()
        if cost > 0:
            kpis["avg_age_days"] = float((lots["days"] * lots["cost_eur"]).sum() / cost)
        valued = lots.dropna(subset=["value_eur"])
        total_value = valued["value_eur"].sum()
        if total_value > 0:
            young = valued.loc[valued["days"] < YOUNG_DAYS, "value_eur"].sum()
            kpis["young_share_pct"] = float(young / total_value * 100)
    if not pos.empty:
        cost = pos["cost_basis_eur"].sum()
        if cost > 0:
            pnl_pct = pos["pnl_eur"].sum() / cost * 100
            weighted_days = (pos["open_avg_days"] * pos["cost_basis_eur"]).sum() / cost
            kpis["annualized_pct"] = annualized_pct(pnl_pct, weighted_days)
    return kpis


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


def _side_stats(side: pd.DataFrame) -> dict:
    """Durée moyenne, médiane et moyenne pondérée par le capital (cost_eur) d'un camp de ventes."""
    n = int(len(side))
    if n == 0:
        return {"n": 0, "mean_days": None, "median_days": None, "wmean_days": None}
    days, cost = side["days"], side["cost_eur"]
    return {
        "n": n,
        "mean_days": float(days.mean()),
        "median_days": float(days.median()),
        "wmean_days": float((days * cost).sum() / cost.sum()) if cost.sum() > 0 else None,
    }


def winners_losers(sales: pd.DataFrame) -> dict:
    """Durée moyenne/médiane/pondérée de détention des ventes gagnantes vs perdantes (pnl exactement 0 ignoré)."""
    ok = reliable_sales(sales)
    if ok.empty:
        return {"winners": _side_stats(ok), "losers": _side_stats(ok)}
    return {
        "winners": _side_stats(ok[ok["pnl_eur"] > 0]),
        "losers": _side_stats(ok[ok["pnl_eur"] < 0]),
    }


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


def trade_stats(sales: pd.DataFrame) -> dict:
    """Statistiques de trading sur les ventes fiables (pnl exactement 0 ignoré).

    - win_rate_pct : part de ventes gagnantes parmi les ventes décisives ;
    - avg_win_* / avg_loss_* : gain et perte moyens (€ et %) ; la perte est négative ;
    - payoff_ratio : gain moyen / |perte moyenne| (€) ;
    - expectancy_eur : résultat moyen par vente décisive ;
    - profit_factor : somme des gains / |somme des pertes|.
    """
    out = {
        "n": 0, "n_wins": 0, "n_losses": 0, "win_rate_pct": None,
        "avg_win_eur": None, "avg_loss_eur": None, "avg_win_pct": None, "avg_loss_pct": None,
        "payoff_ratio": None, "expectancy_eur": None, "profit_factor": None,
    }
    ok = reliable_sales(sales)
    if ok.empty:
        return out
    decisive = ok[ok["pnl_eur"] != 0]
    if decisive.empty:
        return out
    wins, losses = decisive[decisive["pnl_eur"] > 0], decisive[decisive["pnl_eur"] < 0]
    out.update(n=int(len(decisive)), n_wins=int(len(wins)), n_losses=int(len(losses)))
    out["win_rate_pct"] = out["n_wins"] / out["n"] * 100
    out["expectancy_eur"] = float(decisive["pnl_eur"].mean())
    if len(wins):
        out["avg_win_eur"] = float(wins["pnl_eur"].mean())
        out["avg_win_pct"] = _none_if_nan(wins["pnl_pct"].mean())
    if len(losses):
        out["avg_loss_eur"] = float(losses["pnl_eur"].mean())
        out["avg_loss_pct"] = _none_if_nan(losses["pnl_pct"].mean())
    if out["avg_win_eur"] is not None and out["avg_loss_eur"] is not None:
        out["payoff_ratio"] = out["avg_win_eur"] / abs(out["avg_loss_eur"])
        out["profit_factor"] = float(wins["pnl_eur"].sum() / abs(losses["pnl_eur"].sum()))
    return out


def trade_stats_message(stats: dict) -> tuple[str, str]:
    """(niveau, texte) : 'info' | 'warning'. Lecture croisée taux de réussite / ratio gain-perte."""
    if stats["n"] == 0:
        return "info", "Aucune vente décisive (gagnante ou perdante) à analyser."
    if stats["n"] < MIN_SALES_FOR_STATS:
        return "info", (
            f"Échantillon réduit ({stats['n']} ventes décisives) : indicateurs à lire avec prudence, "
            f"il en faut au moins {MIN_SALES_FOR_STATS}."
        )
    win_rate, expectancy, payoff = stats["win_rate_pct"], stats["expectancy_eur"], stats["payoff_ratio"]
    payoff_txt = f"{payoff:.1f}×" if payoff is not None else "—"
    if expectancy > 0 and win_rate < 50:
        return "info", (
            f"Moins d'une vente sur deux est gagnante ({win_rate:.0f} %), mais le gain moyen vaut "
            f"{payoff_txt} la perte moyenne : espérance positive ({expectancy:+.0f} € par vente)."
        )
    if expectancy > 0:
        return "info", (
            f"Espérance positive ({expectancy:+.0f} € par vente) : {win_rate:.0f} % de ventes gagnantes, "
            f"gain moyen {payoff_txt} la perte moyenne."
        )
    if win_rate >= 50:
        return "warning", (
            f"Tu gagnes souvent ({win_rate:.0f} % des ventes) mais tes pertes pèsent plus que tes gains "
            f"(gain moyen {payoff_txt} la perte moyenne) : espérance négative ({expectancy:+.0f} € par vente)."
        )
    return "warning", (
        f"Espérance négative ({expectancy:+.0f} € par vente) : {win_rate:.0f} % de ventes gagnantes "
        f"et un gain moyen de seulement {payoff_txt} la perte moyenne."
    )


def closed_positions_df(data: dict) -> pd.DataFrame:
    """Une ligne par actif vendu : ventes fiables agrégées + durées de détention des lots vendus.

    Les ventes incomplètes (au-delà des lots connus) sont exclues des montants et comptées
    dans `n_incomplete`. `fully_closed` = plus aucune quantité ouverte.
    """
    sales = sales_df(data)
    if sales.empty:
        return pd.DataFrame()
    ok = reliable_sales(sales)
    base = sales.groupby("symbol").agg(n_incomplete=("incomplete", "sum"))
    if ok.empty:
        agg = pd.DataFrame(columns=["n_sales", "cost_eur", "proceeds_eur", "pnl_eur", "last_sale"])
    else:
        agg = ok.groupby("symbol").agg(
            n_sales=("pnl_eur", "size"), cost_eur=("cost_eur", "sum"), proceeds_eur=("proceeds_eur", "sum"),
            pnl_eur=("pnl_eur", "sum"), last_sale=("sell_date", "max"),
        )
    out = base.join(agg, how="left").reset_index()
    for col in ("cost_eur", "proceeds_eur", "pnl_eur"):
        out[col] = out[col].astype(float)
    out["last_sale"] = pd.to_datetime(out["last_sale"])
    out["n_sales"] = out["n_sales"].fillna(0).astype(int)
    out["n_incomplete"] = out["n_incomplete"].astype(int)
    out["pnl_pct"] = (out["pnl_eur"] / out["cost_eur"] * 100).where(out["cost_eur"] > 0)
    holdings = holdings_df(data)
    cols = ["symbol", "open_quantity", "closed_avg_days", "closed_min_days", "closed_max_days"]
    if not holdings.empty:
        out = out.merge(holdings[cols], on="symbol", how="left")
    else:
        for c in cols[1:]:
            out[c] = float("nan")
    out["fully_closed"] = out["open_quantity"].fillna(0) <= CLOSED_QUANTITY_EPS
    return out.sort_values("last_sale", ascending=False, na_position="last").reset_index(drop=True)


def build_scatter_figure(
    pos: pd.DataFrame, y_metric: str = "pnl_pct", log_x: bool = True, label_top: int = LABEL_TOP_N,
) -> go.Figure:
    """Détention moyenne (x) vs P&L % ou annualisé (y) ; taille de bulle = valeur, couleur = classe d'actif.

    En mode annualisé, les positions sans annualisation (< 30 jours) sont absentes.
    """
    data = pos.dropna(subset=[y_metric]).copy()
    fig = go.Figure()
    if data.empty:
        return fig
    data["x_days"] = data["open_avg_days"].clip(lower=1.0) if log_x else data["open_avg_days"]
    sizeref = 2.0 * data["value_eur"].max() / (46.0**2)
    labelled = set(data.nlargest(label_top, "value_eur")["symbol"])
    data["kind"] = data["kind"].fillna("")
    for i, kind in enumerate(sorted(data["kind"].unique())):
        sub = data[data["kind"] == kind]
        fig.add_trace(go.Scatter(
            x=sub["x_days"], y=sub[y_metric], mode="markers+text", name=kind or "Autre",
            text=[s if s in labelled else "" for s in sub["symbol"]], textposition="top center",
            marker=dict(size=sub["value_eur"], sizemode="area", sizeref=sizeref, sizemin=8,
                        color=KIND_SEQUENCE[i % len(KIND_SEQUENCE)], line=dict(width=1, color=BG_COLOR)),
            customdata=list(zip(
                sub["symbol"], [format_days(v) for v in sub["open_avg_days"]],
                sub["value_eur"], sub["pnl_eur"],
                ["—" if pd.isna(a) else f"{a:+.1f} %" for a in sub["annualized_pct"]],
                [f"{p:+.1f} %" for p in sub["pnl_pct"]],
            )),
            hovertemplate=(
                "%{customdata[0]}<br>Détention moy. %{customdata[1]}<br>P&L %{customdata[5]}"
                " (%{customdata[3]:+,.0f} €)<br>Valeur %{customdata[2]:,.0f} €"
                "<br>Annualisé %{customdata[4]}<extra></extra>"
            ),
        ))
    fig.add_hline(y=0, line_color=GRID_COLOR)
    if data["x_days"].max() >= 180:
        # Repère « 1 an » tracé comme une série (coordonnées en jours, sûr sur un axe log).
        y_lo, y_hi = min(data[y_metric].min(), 0.0), max(data[y_metric].max(), 0.0)
        pad = (y_hi - y_lo) * 0.08 or 1.0
        fig.add_trace(go.Scatter(
            x=[365, 365], y=[y_lo - pad, y_hi + pad], mode="lines", showlegend=False, hoverinfo="skip",
            line=dict(color=GRID_COLOR, dash="dot"),
        ))
    fig.update_layout(
        height=460, margin=dict(l=10, r=10, t=30, b=10),
        legend=dict(orientation="h", y=1.08, x=0, title_text=""),
        xaxis=dict(
            title="Détention moyenne (pondérée par le capital)", type="log" if log_x else "linear",
            tickmode="array", tickvals=[d for d, _ in X_TICKS], ticktext=[t for _, t in X_TICKS],
        ),
        yaxis=dict(title=Y_METRICS[y_metric], ticksuffix=" %", zeroline=False),
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
    for key, name, color in (
        ("mean_days", "Moyenne", PASTEL_DARK_SEQUENCE[0]),
        ("median_days", "Médiane", PASTEL_DARK_SEQUENCE[2]),
        ("wmean_days", "Moyenne pondérée (capital)", PASTEL_DARK_SEQUENCE[4]),
    ):
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
    """Tableau des positions ouvertes. Les colonnes restent numériques (tri correct au clic) ;
    l'affichage « 1 an 8 j » est appliqué par `style_positions_table`."""
    out = pd.DataFrame({
        "Actif": pos["symbol"],
        "Poids (%)": pos["weight_pct"],
        "Lots": pos["n_lots"],
        "Détention moyenne": pos["open_avg_days"],
        "Plus vieux lot": pos["open_oldest_days"],
        "Dernier achat": pos["youngest_days"],
        "P&L latent (%)": pos["pnl_pct"],
        "P&L latent (€)": pos["pnl_eur"],
        "Annualisé (%)": pos["annualized_pct"],
        "Valeur (€)": pos["value_eur"],
    })
    return out.sort_values("Détention moyenne", ascending=False).reset_index(drop=True)


def closed_display_table(closed: pd.DataFrame) -> pd.DataFrame:
    """Tableau des ventes par actif (colonnes numériques, voir `style_closed_table`)."""
    return pd.DataFrame({
        "Actif": closed["symbol"],
        "Statut": closed["fully_closed"].map({True: "Soldé", False: "Encore détenu"}),
        "Ventes": closed["n_sales"],
        "Dernière vente": closed["last_sale"].dt.strftime("%Y-%m-%d").fillna("—"),
        "Détention moyenne": closed["closed_avg_days"],
        "Plus courte": closed["closed_min_days"],
        "Plus longue": closed["closed_max_days"],
        "Capital vendu (€)": closed["cost_eur"],
        "Produit (€)": closed["proceeds_eur"],
        "P&L réalisé (€)": closed["pnl_eur"],
        "P&L réalisé (%)": closed["pnl_pct"],
        "Incomplètes exclues": closed["n_incomplete"],
    })


def _pnl_color(value) -> str:
    if value is None or pd.isna(value) or value == 0:
        return ""
    return f"color: {GAIN_COLOR}" if value > 0 else f"color: {LOSS_COLOR}"


def _color_cells(styler, subset: list[str]):
    # Styler.map remplace applymap à partir de pandas 2.1.
    apply = getattr(styler, "map", None) or styler.applymap
    return apply(_pnl_color, subset=subset)


def style_positions_table(table: pd.DataFrame):
    styler = table.style.format({
        "Lots": "{:.0f}", "Détention moyenne": format_days, "Plus vieux lot": format_days,
        "Dernier achat": format_days, "P&L latent (%)": "{:+.1f} %", "P&L latent (€)": "{:+.0f} €",
        "Annualisé (%)": "{:+.1f} %", "Valeur (€)": "{:.0f} €",
    }, na_rep="—")
    return _color_cells(styler, ["P&L latent (%)", "P&L latent (€)", "Annualisé (%)"])


def style_closed_table(table: pd.DataFrame):
    styler = table.style.format({
        "Ventes": "{:.0f}", "Détention moyenne": format_days, "Plus courte": format_days,
        "Plus longue": format_days, "Capital vendu (€)": "{:.0f} €", "Produit (€)": "{:.0f} €",
        "P&L réalisé (€)": "{:+.0f} €", "P&L réalisé (%)": "{:+.1f} %", "Incomplètes exclues": "{:.0f}",
    }, na_rep="—")
    return _color_cells(styler, ["P&L réalisé (€)", "P&L réalisé (%)"])


# --- Rendu Streamlit ---------------------------------------------------------------

def _render_kpis(kpis: dict) -> None:
    cols = st.columns(5)
    cols[0].metric("Âge moyen", format_days(kpis["avg_age_days"]),
                   help="Âge des lots ouverts, pondéré par le capital investi.")
    cols[1].metric("Plus vieux lot", format_days(kpis["oldest_days"]))
    young = kpis["young_share_pct"]
    cols[2].metric("Capital < 3 mois", "—" if young is None else f"{young:.0f} %",
                   help="Part de la valeur actuelle détenue depuis moins de 3 mois (position non éprouvée).")
    cols[3].metric("Lots ouverts", str(kpis["n_lots"]))
    ann = kpis["annualized_pct"]
    cols[4].metric("Annualisé (approx.)", "—" if ann is None else f"{ann:+.1f} %",
                   help="P&L latent agrégé ramené à un an sur la détention moyenne pondérée. "
                        "Approximation pour un portefeuille construit en plusieurs achats.")


def _render_trade_stats(stats: dict) -> None:
    cols = st.columns(5)
    win_rate = stats["win_rate_pct"]
    cols[0].metric("Taux de réussite", "—" if win_rate is None else f"{win_rate:.0f} %",
                   help="Part de ventes gagnantes parmi les ventes décisives (pnl ≠ 0).")
    for col, label, eur_key, pct_key in (
        (cols[1], "Gain moyen", "avg_win_eur", "avg_win_pct"),
        (cols[2], "Perte moyenne", "avg_loss_eur", "avg_loss_pct"),
    ):
        eur, pct = stats[eur_key], stats[pct_key]
        col.metric(label, "—" if eur is None else f"{eur:+.0f} €",
                   delta=None if pct is None else f"{pct:+.1f} % en moyenne", delta_color="off")
    payoff = stats["payoff_ratio"]
    cols[3].metric("Ratio gain / perte", "—" if payoff is None else f"{payoff:.2f}×",
                   help="Gain moyen divisé par la perte moyenne (en valeur absolue).")
    expectancy = stats["expectancy_eur"]
    pf = stats["profit_factor"]
    cols[4].metric("Espérance / vente", "—" if expectancy is None else f"{expectancy:+.0f} €",
                   delta=None if pf is None else f"profit factor {pf:.2f}", delta_color="off",
                   help="Résultat moyen par vente décisive. Profit factor = total des gains / total des pertes.")


def render_holding_page() -> None:
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

    kpi_box = st.container()  # rempli plus bas, une fois la sélection connue
    symbols = st.multiselect(
        f"Actifs d'au moins {MIN_POSITION_VALUE_EUR:.0f} € (KPI, vues 1 et 2 et tableau)", held, default=held
    )
    if not symbols:
        st.info("Sélectionne au moins un actif.")
        st.stop()

    pos = positions_df(data, symbols)
    with kpi_box:
        _render_kpis(portfolio_kpis(open_lots_df(holdings, symbols), pos))

    # --- 1. Détention vs performance ---
    st.subheader("1. Détention vs performance")
    if pos.empty:
        st.info("Aucune position ouverte avec un cost basis connu.")
    else:
        c_y, c_x = st.columns([2, 1])
        y_label = c_y.radio("Axe vertical", list(Y_METRICS.values()), horizontal=True)
        log_x = c_x.checkbox("Axe horizontal logarithmique", value=True)
        y_metric = next(k for k, v in Y_METRICS.items() if v == y_label)
        if pos[y_metric].notna().any():
            st.plotly_chart(build_scatter_figure(pos, y_metric, log_x), use_container_width=True)
        else:
            st.info(f"Aucune position détenue depuis au moins {MIN_DAYS_ANNUALIZED} jours : "
                    "pas d'annualisation possible.")
        st.caption(
            "En haut à droite : gagnants tenus longtemps. En bas à droite : perdants que tu gardes (à "
            "réexaminer). À gauche : positions trop jeunes pour un verdict. Taille de bulle = valeur, "
            f"couleur = classe d'actif, ligne pointillée = 1 an. Étiquettes sur les {LABEL_TOP_N} plus "
            "grosses positions. En mode annualisé, les positions de moins de "
            f"{MIN_DAYS_ANNUALIZED} jours sont absentes."
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

    # --- 3. Ventes : rentabilité et durée ---
    st.subheader("3. Ventes : rentabilité et durée (gagnantes vs perdantes)")
    sales = sales_df(data)
    stats = trade_stats(sales)
    _render_trade_stats(stats)
    stats_level, stats_message = trade_stats_message(stats)
    (st.warning if stats_level == "warning" else st.info)(stats_message)

    wl = winners_losers(sales)
    level, message = winners_losers_message(wl)
    if wl["winners"]["n"] and wl["losers"]["n"]:
        st.plotly_chart(build_winners_losers_figure(wl), use_container_width=True)
    (st.warning if level == "warning" else st.info)(message)
    st.caption("Toutes les ventes de titres complètes, indépendamment du filtre (c'est un comportement "
               "global). Les ventes dépassant les lots connus sont exclues. Moyenne pondérée : chaque vente "
               "pèse son capital (coût d'achat). Espérance = résultat moyen par vente décisive.")

    # --- Tableau ---
    if not pos.empty:
        st.subheader("Détail par position")
        table = display_table(pos)
        st.dataframe(
            style_positions_table(table), hide_index=True, use_container_width=True,
            column_config={
                "Poids (%)": st.column_config.ProgressColumn(
                    "Poids", format="%.1f %%", min_value=0.0, max_value=float(max(table["Poids (%)"].max(), 1.0)),
                    help="Part de la valeur de la sélection.",
                ),
            },
        )
        st.caption(
            f"Annualisé = P&L ramené à un an sur la détention moyenne ; « — » sous {MIN_DAYS_ANNUALIZED} "
            "jours. Approximation pour les positions construites en plusieurs achats. « Dernier achat » = âge "
            "du lot le plus récent. Clique sur un en-tête pour trier."
        )

    # --- 4. Positions soldées et ventes par actif ---
    st.subheader("4. Positions soldées et ventes par actif")
    closed = closed_positions_df(data)
    if closed.empty:
        st.info("Aucune vente de titres dans dashboard.json.")
    else:
        only_closed = st.checkbox("Uniquement les positions soldées", value=False)
        shown = closed[closed["fully_closed"]] if only_closed else closed
        if shown.empty:
            st.info("Aucune position entièrement soldée.")
        else:
            st.dataframe(style_closed_table(closed_display_table(shown)), hide_index=True, use_container_width=True)
        st.caption(
            "Titres uniquement (pour la crypto, un retrait est un transfert entre wallets). Indépendant du "
            "filtre d'actifs. Les montants et le P&L excluent les ventes incomplètes (au-delà des lots "
            "connus), comptées dans « Incomplètes exclues ». Durées : lots vendus, pondérées par le capital."
        )
