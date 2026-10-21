"""Chargement du JSON généré par le pipeline Rust, et mise en forme."""
from __future__ import annotations

import json
from pathlib import Path

import pandas as pd
import streamlit as st

from config import DATA_PATH, HISTORY_PATH, MIN_POSITION_VALUE_EUR


CASH_KIND = "Cash"  # ajuste si besoin

def compute_kpis(df: pd.DataFrame, exclude_cash: bool = True) -> dict:
    """Recalcule les totaux globaux à partir du df filtré."""
    scope = df[df["kind"] != CASH_KIND] if exclude_cash else df
    total_value = float(scope["value_eur"].sum())
    total_cost = float(scope["cost_basis_eur"].sum())
    total_pnl_latent = float(scope["pnl_eur"].sum())
    
    # On récupère le P&L réalisé par actif (s'il existe dans le df)
    total_pnl_realized = float(scope["realized_pnl_eur"].sum()) if "realized_pnl_eur" in scope.columns else 0.0
    total_realized_cost = float(scope["realized_cost_eur"].sum()) if "realized_cost_eur" in scope.columns else 0.0

    return {
        "total_value_eur": total_value,
        "total_cost_basis_eur": total_cost,
        # Capital engagé : cost basis des lots ouverts + coût des lots déjà vendus.
        "total_invested_eur": total_cost + total_realized_cost,
        "total_pnl_latent_eur": total_pnl_latent,
        "total_pnl_realized_eur": total_pnl_realized,
        "total_pnl_eur": total_pnl_latent + total_pnl_realized,
    }

def compute_global_kpis(df_all: pd.DataFrame, data: dict) -> dict | None:
    """Performance globale cash inclus (titres + crypto + cash XTB).

    Gain = P&L latent + P&L réalisé (titres et crypto) + dividendes/intérêts nets des taxes.
    Base = cost basis des lots ouverts + cash disponible : le cash dormant dilue la performance.

    Volontairement indépendant des apports nets : (valeur + cash - apports) n'est juste que si
    TOUT l'historique des versements est dans les exports (positions transférées, export qui ne
    remonte pas à l'ouverture du compte... faussent le résultat).

    Renvoie None si dashboard.json n'a pas encore les champs du patch cash (pas de crash).
    """
    cash = (data.get("cash") or {}).get("total")
    if cash is None:
        return None

    k = compute_kpis(df_all)
    cash_eur = float(cash["cash_eur"])
    income = float(cash.get("income_net_eur", 0.0))
    equity = k["total_value_eur"] + cash_eur
    gain = k["total_pnl_eur"] + income
    base = k["total_cost_basis_eur"] + cash_eur
    return {
        "equity_eur": equity,
        "cash_eur": cash_eur,
        "cash_share": cash_eur / equity if equity > 0 else 0.0,
        "gain_eur": gain,
        "return_pct": gain / base * 100 if base > 0 else None,
        "income_net_eur": income,
    }


def filter_small_positions(df: pd.DataFrame, min_value_eur: float = MIN_POSITION_VALUE_EUR) -> pd.DataFrame:
    """Écarte les lignes dont la valeur actuelle est inférieure au seuil (>= seuil conservé).

    À réserver à l'AFFICHAGE : une position soldée (valeur 0) est aussi écartée, or elle porte
    du P&L réalisé qui doit rester dans les totaux (voir `compute_kpis`).
    """
    return df[df["value_eur"] >= min_value_eur].copy()


def small_positions_summary(df: pd.DataFrame, min_value_eur: float = MIN_POSITION_VALUE_EUR) -> tuple[int, float]:
    """(nombre, valeur totale) des positions encore détenues mais sous le seuil.

    Les positions soldées (valeur nulle) ne comptent pas : on ne les « possède » plus.
    """
    dust = df[(df["value_eur"] > 0) & (df["value_eur"] < min_value_eur)]
    return int(len(dust)), float(dust["value_eur"].sum())


def load_data(path: Path = DATA_PATH) -> dict:
    """Charge le JSON du pipeline Rust. Arrête l'app proprement s'il est absent."""
    if not path.exists():
        st.error("Fichier dashboard.json introuvable. Lance d'abord le binaire Rust (`cargo run`).")
        st.stop()
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def build_assets_df(data: dict) -> pd.DataFrame:
    """Construit le DataFrame des positions à partir du JSON brut."""
    df = pd.DataFrame(data["assets"])
    for col in ("realized_pnl_eur", "realized_cost_eur"):  # absents d'un ancien dashboard.json
        if col not in df.columns:
            df[col] = 0.0
    df["pnl_total_eur"] = df["pnl_eur"] + df["realized_pnl_eur"]
    df["invested_eur"] = df["cost_basis_eur"] + df["realized_cost_eur"]
    df["pnl_total_pct"] = (df["pnl_total_eur"] / df["invested_eur"] * 100).where(df["invested_eur"] > 0, 0.0)
    df["pnl_color"] = df["pnl_total_eur"].apply(lambda x: "Gain" if x >= 0 else "Perte")
    return df


def yfinance_ticker_for(row: pd.Series) -> str:
    """Résout le ticker Yahoo Finance à utiliser pour une position donnée."""
    ticker = row.get("ticker")
    if isinstance(ticker, str) and ticker:
        return ticker
    if row.get("kind") == "Crypto":
        return f"{row['symbol']}-EUR"
    return row["symbol"]


def format_display_df(df: pd.DataFrame) -> pd.DataFrame:
    """Renvoie une copie du DataFrame formatée pour l'affichage (st.dataframe)."""
    df_display = df.copy()
    df_display["quantity"] = df_display["quantity"].apply(lambda x: f"{x:,.4f}")
    df_display["price_eur"] = df_display["price_eur"].apply(lambda x: f"{x:,.2f} €")
    df_display["value_eur"] = df_display["value_eur"].apply(lambda x: f"{x:,.2f} €")
    df_display["cost_basis_eur"] = df_display["cost_basis_eur"].apply(lambda x: f"{x:,.2f} €")
    df_display["pnl_eur"] = df_display["pnl_eur"].apply(lambda x: f"{x:+,.2f} €")
    df_display["pnl_pct"] = df_display["pnl_pct"].apply(lambda x: f"{x:+.2f} %")
    df_display["realized_pnl_eur"] = df_display["realized_pnl_eur"].apply(lambda x: f"{x:+,.2f} €")
    df_display["pnl_total_eur"] = df_display["pnl_total_eur"].apply(lambda x: f"{x:+,.2f} €")
    df_display["pnl_total_pct"] = df_display["pnl_total_pct"].apply(lambda x: f"{x:+.2f} %")

    return df_display.rename(columns={
        "symbol": "Symbole",
        "kind": "Type",
        "quantity": "Quantité",
        "price_eur": "Prix",
        "value_eur": "Valeur",
        "cost_basis_eur": "Cost Basis",
        "pnl_eur": "P&L latent",
        "pnl_pct": "Latent (%)",
        "realized_pnl_eur": "P&L réalisé",
        "pnl_total_eur": "P&L total",
        "pnl_total_pct": "Perf. totale (%)",
    })


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