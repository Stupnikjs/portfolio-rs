"""Point d'entrée du dashboard. Lancer avec : streamlit run app.py"""
from __future__ import annotations

import streamlit as st

from theme import apply_theme
from data import load_data, build_assets_df, compute_kpis, yfinance_ticker_for  # <-- Ajout de yfinance_ticker_for
from charts import render_kpis, render_allocation_and_pnl, render_positions_table
from correlation import render_correlation_section
from assets_data import load_assets_data, save_assets_data, sync_watchlist_with_portfolio  # <-- NOUVELLE LIGNE

apply_theme()  # doit être appelé avant tout autre st.* ou px.*

data = load_data()
df = build_assets_df(data)
# --- SYNCHRONISATION ET FUSION DES TAGS ---
assets_data = load_assets_data()

def resolver(symbol):
    row = df[df["symbol"] == symbol]
    if not row.empty:
        return yfinance_ticker_for(row.iloc[0])
    return symbol

added = sync_watchlist_with_portfolio(assets_data, df, ticker_resolver=resolver)
if added > 0:
    save_assets_data(assets_data)

tag_map = {a["ticker"]: a.get("tags", []) for a in assets_data["assets"]}

def get_tags(row):
    if row.get("ticker") in tag_map:
        return tag_map[row["ticker"]]
    elif row.get("symbol") in tag_map:
        return tag_map[row["symbol"]]
    return []

df["tags"] = df.apply(get_tags, axis=1)

df["tags"] = df.apply(get_tags, axis=1)

# --- NOUVEAU : TABLEAU D'AGRÉGATION GLOBAL PAR TAG ---
# On explode les tags : si un actif a 2 tags, il compte dans les 2 lignes du tableau
df_global = df.copy()
if not df_global.empty and "tags" in df_global.columns:
    df_exploded = df_global.explode("tags").dropna(subset=["tags"])
    if not df_exploded.empty:
        tag_summary = df_exploded.groupby("tags").agg(
            Valeur=("value_eur", "sum"),
            Cout=("cost_basis_eur", "sum"),
            PnL=("pnl_eur", "sum")
        ).reset_index()
        
        tag_summary["PnL %"] = tag_summary.apply(lambda r: (r["PnL"]/r["Cout"]*100) if r["Cout"] > 0 else 0.0, axis=1)
        tag_summary = tag_summary.sort_values("Valeur", ascending=False)
        
        st.subheader("📊 Vue d'ensemble : Performance par Tag")
        
        # Formatage pour l'affichage
        tag_display = tag_summary.copy()
        tag_display["Valeur"] = tag_display["Valeur"].apply(lambda x: f"{x:,.2f} €")
        tag_display["Cout"] = tag_display["Cout"].apply(lambda x: f"{x:,.2f} €")
        tag_display["PnL"] = tag_display["PnL"].apply(lambda x: f"{x:+,.2f} €")
        tag_display["PnL %"] = tag_display["PnL %"].apply(lambda x: f"{x:+.2f} %")
        
        st.dataframe(tag_display.rename(columns={
            "tags": "Tag",
            "Valeur": "Valeur Totale",
            "Cout": "Cost Basis Total",
            "PnL": "P&L (€)",
            "PnL %": "P&L (%)"
        }), use_container_width=True, hide_index=True)
        st.divider()

# --- FILTRAGE PAR TAGS ---
all_tags = sorted(set(tag for tags in df["tags"] for tag in tags))

if all_tags:
    st.sidebar.subheader("🏷️ Filtrer par Tag")
    selected_tags = st.sidebar.multiselect("Sélectionner un ou plusieurs tags", all_tags)
    
    if selected_tags:
        df = df[df["tags"].apply(lambda x: any(t in x for t in selected_tags))].copy()

kpis = compute_kpis(df, realized_pnl_eur=data.get("realized_pnl_eur", 0.0))

render_kpis(kpis)

st.divider()

render_allocation_and_pnl(df)

st.divider()
render_positions_table(df)

st.divider()
render_correlation_section(df, data)
