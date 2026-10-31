"""Statistiques par tag : résumé, historique dans le temps, activité de trading.

Logique pure (pandas uniquement, aucun st.*) pour rester testable.
Un actif à plusieurs tags compte dans chacun d'eux : les lignes par tag ne s'additionnent pas.
Un actif sans tag tombe dans le tag « Sans tag ».
"""
from __future__ import annotations

import numpy as np
import pandas as pd

UNTAGGED = "Sans tag"
CASH_KIND = "Cash"


def _tags_or_untagged(tags) -> list[str]:
    return list(tags) if isinstance(tags, (list, tuple)) and len(tags) > 0 else [UNTAGGED]


def explode_tags(df: pd.DataFrame) -> pd.DataFrame:
    """Une ligne par (position, tag). Nécessite la colonne `tags` (voir attach_tags)."""
    out = df[df["kind"] != CASH_KIND].copy()
    out["tag"] = out["tags"].apply(_tags_or_untagged)
    return out.explode("tag").reset_index(drop=True)


def symbol_tags_map(df_all: pd.DataFrame) -> dict[str, list[str]]:
    """symbole -> liste de tags (ou [« Sans tag »])."""
    return {row.symbol: _tags_or_untagged(row.tags) for row in df_all.itertuples()}


# --- Résumé par tag ---------------------------------------------------------------

def tag_summary(df_all: pd.DataFrame) -> pd.DataFrame:
    """Une ligne par tag : valeur, poids, capital engagé, P&L latent/réalisé/total, perf., nb de positions.

    `df_all` doit contenir TOUTES les positions (y compris soldées) : une position soldée
    porte du P&L réalisé qui doit rester dans les totaux.
    """
    scope = df_all[df_all["kind"] != CASH_KIND]
    if scope.empty:
        return pd.DataFrame()

    ex = explode_tags(df_all)
    summary = (
        ex.assign(held=(ex["value_eur"] > 0).astype(int))
        .groupby("tag")
        .agg(
            value=("value_eur", "sum"),
            cost_basis=("cost_basis_eur", "sum"),
            invested=("invested_eur", "sum"),
            pnl_latent=("pnl_eur", "sum"),
            pnl_realized=("realized_pnl_eur", "sum"),
            pnl_total=("pnl_total_eur", "sum"),
            positions=("held", "sum"),
        )
        .reset_index()
    )
    total_value = float(scope["value_eur"].sum())  # total réel, sans double compte
    summary["weight_pct"] = summary["value"] / total_value * 100 if total_value > 0 else 0.0
    # cost_basis = capital engagé (lots ouverts) ; invested = total investi cumulé (ouverts + vendus).
    summary["perf_pct"] = (summary["pnl_total"] / summary["invested"] * 100).where(summary["invested"] > 0, 0.0)
    summary["latent_pct"] = (summary["pnl_latent"] / summary["cost_basis"] * 100).where(summary["cost_basis"] > 0, 0.0)
    summary["pnl_color"] = summary["pnl_total"].apply(lambda x: "Gain" if x >= 0 else "Perte")
    return summary.sort_values("value", ascending=False).reset_index(drop=True)


def positions_of_tag(df_all: pd.DataFrame, tag: str) -> pd.DataFrame:
    """Positions portant ce tag (ou sans aucun tag pour « Sans tag »)."""
    scope = df_all[df_all["kind"] != CASH_KIND]
    return scope[scope["tags"].apply(lambda t: tag in _tags_or_untagged(t))].copy()


# --- Historique par tag ---------------------------------------------------------------

def tag_history(history: pd.DataFrame | None, sym_tags: dict[str, list[str]]) -> pd.DataFrame:
    """Valeur, cost basis et P&L latent par (date, tag), à partir des snapshots de history.json.

    Colonnes : date, tag, value_eur, cost_basis_eur, pnl_eur, pnl_pct.
    Les snapshots sans détail par actif (ancien format) sont ignorés.
    """
    cols = ["date", "tag", "value_eur", "cost_basis_eur", "pnl_eur", "pnl_pct"]
    if history is None or history.empty:
        return pd.DataFrame(columns=cols)

    rows = []
    for date, assets in zip(history["date"], history["assets"]):
        if not isinstance(assets, list):
            continue
        for a in assets:
            if a.get("kind") == CASH_KIND:
                continue
            for tag in sym_tags.get(a.get("symbol"), [UNTAGGED]):
                rows.append({
                    "date": date, "tag": tag,
                    "value_eur": float(a.get("value_eur") or 0.0),
                    "cost_basis_eur": float(a.get("cost_basis_eur") or 0.0),
                })
    if not rows:
        return pd.DataFrame(columns=cols)

    out = pd.DataFrame(rows).groupby(["date", "tag"], as_index=False).sum()
    out["pnl_eur"] = out["value_eur"] - out["cost_basis_eur"]
    out["pnl_pct"] = (out["pnl_eur"] / out["cost_basis_eur"] * 100).where(out["cost_basis_eur"] > 0, 0.0)
    return out[cols].sort_values(["tag", "date"]).reset_index(drop=True)


# --- Activité par tag ---------------------------------------------------------------

def tag_trades(trades: list[dict] | None, sym_tags: dict[str, list[str]]) -> pd.DataFrame:
    """Un trade par (trade, tag), avec une colonne `month` (AAAA-MM). Vide si pas de trades."""
    if not trades:
        return pd.DataFrame()
    df = pd.DataFrame(trades)
    df["time"] = pd.to_datetime(df["time"], utc=True)
    df["month"] = df["time"].dt.tz_convert(None).dt.to_period("M").astype(str)
    df["tag"] = df["symbol"].map(lambda s: sym_tags.get(s, [UNTAGGED]))
    return df.explode("tag").reset_index(drop=True)


def trades_by_month(trades_tag: pd.DataFrame) -> pd.DataFrame:
    """Pour un tag déjà filtré : montants achetés / vendus et nombre de trades par mois."""
    if trades_tag.empty:
        return pd.DataFrame(columns=["month", "bought", "sold", "n_trades"])
    d = trades_tag.assign(
        bought=trades_tag["value_eur"].where(trades_tag["kind"] == "Buy", 0.0),
        sold=trades_tag["value_eur"].where(trades_tag["kind"] == "Sell", 0.0),
    )
    return (
        d.groupby("month")
        .agg(bought=("bought", "sum"), sold=("sold", "sum"), n_trades=("symbol", "size"))
        .reset_index()
        .sort_values("month")
    )


# --- Chevauchement entre tags ---------------------------------------------------------

def tag_overlap(df_all: pd.DataFrame) -> pd.DataFrame:
    """Matrice de chevauchement (en %) : cellule (A, B) = part de la valeur de A portée aussi par B.

    Diagonale = 100. Vide s'il y a moins de 2 tags réels (« Sans tag » est ignoré).
    """
    ex = explode_tags(df_all)
    ex = ex[(ex["tag"] != UNTAGGED) & (ex["value_eur"] > 0)]
    if ex["tag"].nunique() < 2:
        return pd.DataFrame()
    m = ex.pivot_table(index="symbol", columns="tag", values="value_eur", aggfunc="sum", fill_value=0.0)
    m = m[m.sum().sort_values(ascending=False).index]          # tags triés par valeur décroissante
    shared = m.T @ (m > 0).astype(float)                        # [A, B] = valeur de A sur les actifs aussi dans B
    return shared.div(m.sum(), axis=0) * 100


# --- Performance par tag, insensible aux flux ---------------------------------------------

def tag_returns(history: pd.DataFrame | None, sym_tags: dict[str, list[str]]) -> pd.DataFrame:
    """Rendement par (date, tag) entre deux snapshots consécutifs, hors achats/ventes.

    Pour chaque actif détenu au snapshot précédent : gain = quantité_précédente x variation de prix.
    Le rendement du tag = somme des gains / somme des valeurs de départ. Un achat ou une vente
    entre deux dates ne change donc pas la performance (contrairement à la valeur en €).

    Colonnes : date, tag, ret (fraction), gain_eur, base_eur (valeur de départ du tag),
    port_gain_eur, port_base_eur (même chose pour tout le portefeuille, sans double compte).
    Chaque tag commence par une ligne d'ancrage (ret = 0, gain/base = 0, port_* = NaN).
    """
    cols = ["date", "tag", "ret", "gain_eur", "base_eur", "port_gain_eur", "port_base_eur"]
    if history is None or history.empty:
        return pd.DataFrame(columns=cols)

    rows, prev, prev_date = [], None, None
    for date, assets in zip(history["date"], history["assets"]):
        if not isinstance(assets, list):
            prev, prev_date = None, None
            continue
        cur = {a["symbol"]: (float(a.get("quantity") or 0.0), float(a.get("price_eur") or 0.0))
               for a in assets if a.get("kind") != CASH_KIND and a.get("symbol")}
        if prev is not None:
            gain: dict[str, float] = {}
            base: dict[str, float] = {}
            pg = pb = 0.0
            for sym, (q0, p0) in prev.items():
                p1 = cur.get(sym, (0.0, 0.0))[1]
                if q0 <= 0 or p0 <= 0 or p1 <= 0:
                    continue
                pg += q0 * (p1 - p0)
                pb += q0 * p0
                for tag in sym_tags.get(sym, [UNTAGGED]):
                    gain[tag] = gain.get(tag, 0.0) + q0 * (p1 - p0)
                    base[tag] = base.get(tag, 0.0) + q0 * p0
            rows += [(prev_date, date, tag, gain[tag] / b, gain[tag], b, pg, pb) for tag, b in base.items()]
        prev, prev_date = cur, date

    if not rows:
        return pd.DataFrame(columns=cols)

    r = pd.DataFrame(rows, columns=["prev_date"] + cols)
    out = []
    for tag, g in r.sort_values("date").groupby("tag"):
        anchor = pd.DataFrame({"date": [g["prev_date"].iloc[0]], "tag": [tag], "ret": [0.0],
                               "gain_eur": [0.0], "base_eur": [0.0],
                               "port_gain_eur": [np.nan], "port_base_eur": [np.nan]})
        out.append(pd.concat([anchor, g[cols]], ignore_index=True))
    return pd.concat(out, ignore_index=True).sort_values(["tag", "date"]).reset_index(drop=True)


def rebase_returns(returns: pd.DataFrame, start: pd.Timestamp | None = None) -> pd.DataFrame:
    """Ajoute `index` : base 100 au premier point de chaque tag dans la fenêtre (date >= start)."""
    d = returns if start is None else returns[returns["date"] >= start]
    d = d.sort_values(["tag", "date"]).reset_index(drop=True)
    if d.empty:
        return d.assign(index=pd.Series(dtype=float))
    d.loc[~d.duplicated("tag"), "ret"] = 0.0
    d["index"] = 100 * (1 + d["ret"]).groupby(d["tag"]).cumprod()
    return d


def tag_perf_stats(rebased: pd.DataFrame, min_points_vol: int = 8) -> pd.DataFrame:
    """Par tag : performance sur la fenêtre, volatilité annualisée, drawdown max, nb de points."""
    rows = []
    for tag, g in rebased.sort_values("date").groupby("tag"):
        idx = g["index"]
        rets = g["ret"].iloc[1:]
        vol = np.nan
        if len(rets) >= min_points_vol:
            step = g["date"].diff().dt.days.median()
            vol = rets.std() * np.sqrt(365 / step if step and step > 0 else 52) * 100
        rows.append({
            "tag": tag,
            "perf_pct": (idx.iloc[-1] / 100 - 1) * 100,
            "vol_pct": vol,
            "max_dd_pct": (idx / idx.cummax() - 1).min() * 100,
            "points": len(g),
        })
    return pd.DataFrame(rows, columns=["tag", "perf_pct", "vol_pct", "max_dd_pct", "points"]) \
        .sort_values("perf_pct", ascending=False).reset_index(drop=True)


def tag_contribution(returns: pd.DataFrame, start: pd.Timestamp | None = None) -> tuple[pd.DataFrame, float]:
    """Contribution de chaque tag à la performance du portefeuille sur la fenêtre (date >= start).

    Par période : gain du tag / valeur de départ de TOUT le portefeuille ; les périodes sont ensuite
    additionnées (approximation). Un actif à plusieurs tags contribue à chacun : les contributions
    ne s'additionnent donc pas. Renvoie (df[tag, contrib_pts, gain_eur], performance du portefeuille en %).
    """
    empty = pd.DataFrame(columns=["tag", "contrib_pts", "gain_eur"])
    d = returns if start is None else returns[returns["date"] >= start]
    if d.empty:
        return empty, 0.0
    start_point = d["date"].min()                       # point de départ de la fenêtre (ancrage inclus)
    d = d[(d["date"] > start_point) & (d["port_base_eur"] > 0)]   # périodes qui commencent dans la fenêtre
    if d.empty:
        return empty, 0.0

    port = d.drop_duplicates("date")
    port_perf = float(((1 + port["port_gain_eur"] / port["port_base_eur"]).prod() - 1) * 100)
    d = d.assign(c=d["gain_eur"] / d["port_base_eur"] * 100)
    out = d.groupby("tag").agg(contrib_pts=("c", "sum"), gain_eur=("gain_eur", "sum")).reset_index()
    return out.sort_values("contrib_pts", ascending=False).reset_index(drop=True), port_perf


def tag_correlation(rebased: pd.DataFrame, min_points: int = 8) -> pd.DataFrame:
    """Corrélation des rendements périodiques entre tags (fenêtre déjà appliquée par rebase_returns).

    Vide s'il y a moins de 2 tags ou moins de `min_points` périodes communes.
    Attention : deux tags qui partagent des actifs sont corrélés mécaniquement.
    """
    if rebased.empty or rebased["tag"].nunique() < 2:
        return pd.DataFrame()
    d = rebased[rebased.duplicated("tag")]              # retire le 1er point (ret forcé à 0)
    wide = d.pivot(index="date", columns="tag", values="ret").dropna(how="all")
    if len(wide) < min_points:
        return pd.DataFrame()
    return wide.corr(min_periods=min_points).dropna(how="all").dropna(axis=1, how="all")
