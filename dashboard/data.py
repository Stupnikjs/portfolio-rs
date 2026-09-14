"""Chargement du JSON généré par le pipeline Rust, et mise en forme."""
from __future__ import annotations

import json
from pathlib import Path

import pandas as pd
import streamlit as st

from config import DATA_PATH


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
    df["pnl_color"] = df["pnl_eur"].apply(lambda x: "Gain" if x >= 0 else "Perte")
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

    return df_display.rename(columns={
        "symbol": "Symbole",
        "kind": "Type",
        "quantity": "Quantité",
        "price_eur": "Prix",
        "value_eur": "Valeur",
        "cost_basis_eur": "Cost Basis",
        "pnl_eur": "P&L (EUR)",
        "pnl_pct": "P&L (%)",
    })
