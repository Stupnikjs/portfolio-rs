"""Statistiques par tag : résumé, historique dans le temps, activité de trading.

Logique pure (pandas uniquement, aucun st.*) pour rester testable.
Un actif à plusieurs tags compte dans chacun d'eux : les lignes par tag ne s'additionnent pas.
Un actif sans tag tombe dans le tag « Sans tag ».
"""
from __future__ import annotations

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