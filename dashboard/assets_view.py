"""Section Streamlit Actifs : DCA actif + Watchlist. À appeler depuis app.py :

    from assets_data import load_assets_data
    from assets_view import render_assets_section
    ...
    assets_data_ = load_assets_data()
    render_assets_section(assets_data_)
"""
from __future__ import annotations

import pandas as pd
import streamlit as st

from assets_data import (
    Asset,
    add_asset,
    demote_to_watchlist,
    get_dca_assets,
    get_price_eur,
    get_watchlist_assets,
    promote_to_dca,
    remove_asset,
    save_assets_data,
    update_asset,
)


def render_assets_section(assets_data: dict) -> None:
    _render_dca_section(assets_data)
    st.divider()
    _render_watchlist_section(assets_data)
    st.divider()
    _render_add_form(assets_data)


# --- DCA actif ----------------------------------------------------------------

def _render_dca_section(assets_data: dict) -> None:
    st.subheader("💶 DCA actif")
    dca_assets = get_dca_assets(assets_data)

    if not dca_assets:
        st.info("Aucun actif en DCA pour l'instant.")
        return

    rows = []
    for a in dca_assets:
        price = get_price_eur(a.ticker)
        montant = a.quantite_dca_mensuelle * price if price is not None else None
        rows.append({
            "Nom": a.name,
            "Ticker": a.ticker,
            "Quantité / mois": a.quantite_dca_mensuelle,
            "Prix actuel (€)": price,
            "Montant DCA / mois (€)": montant,
            "Dernière analyse": a.last_analysis_update or "—",
        })
    df = pd.DataFrame(rows)

    total = df["Montant DCA / mois (€)"].sum(skipna=True)
    if df["Montant DCA / mois (€)"].isna().any():
        st.caption("⚠️ Prix indisponible pour au moins un actif -- total calculé hors actifs concernés.")
    st.metric("Total DCA mensuel", f"{total:,.0f} €")

    disp = df.copy()
    disp["Quantité / mois"] = disp["Quantité / mois"].apply(lambda x: f"{x:g}")
    disp["Prix actuel (€)"] = disp["Prix actuel (€)"].apply(
        lambda x: f"{x:,.2f} €" if pd.notna(x) else "indisponible"
    )
    disp["Montant DCA / mois (€)"] = disp["Montant DCA / mois (€)"].apply(
        lambda x: f"{x:,.2f} €" if pd.notna(x) else "indisponible"
    )
    st.dataframe(disp, use_container_width=True, hide_index=True)

    with st.expander("Gérer les positions DCA"):
        ticker = st.selectbox("Actif", options=[a.ticker for a in dca_assets], key="dca_manage_select")
        current_qty = next(a.quantite_dca_mensuelle for a in dca_assets if a.ticker == ticker)
        current_price = get_price_eur(ticker)

        col1, col2 = st.columns(2)
        new_qty = col1.number_input(
            "Nouvelle quantité mensuelle",
            min_value=0.0, step=0.0001, format="%.6f",
            value=current_qty,
            key="dca_manage_qty",
        )
        if current_price is not None:
            col1.caption(f"≈ {new_qty * current_price:,.2f} € au prix actuel ({current_price:,.2f} €)")
        else:
            col1.caption("Prix actuel indisponible -- montant non estimable.")

        if col1.button("Mettre à jour la quantité"):
            update_asset(assets_data, ticker, quantite_dca_mensuelle=new_qty)
            save_assets_data(assets_data)
            st.rerun()
        if col2.button("↩️ Repasser en watchlist"):
            demote_to_watchlist(assets_data, ticker)
            save_assets_data(assets_data)
            st.rerun()


# --- Watchlist ------------------------------------------------------------

def _render_watchlist_section(assets_data: dict) -> None:
    st.subheader("👀 Watchlist")
    candidates = get_watchlist_assets(assets_data)

    if not candidates:
        st.caption("Aucun candidat en watchlist.")
        return

    rows = []
    for a in candidates:
        price = get_price_eur(a.ticker)
        rows.append({
            "Nom": a.name,
            "Ticker": a.ticker,
            "Prix actuel (€)": price,
            "Analyse": a.analysis_path or "—",
            "Dernière analyse": a.last_analysis_update or "—",
        })
    disp = pd.DataFrame(rows)
    disp["Prix actuel (€)"] = disp["Prix actuel (€)"].apply(
        lambda x: f"{x:,.2f} €" if pd.notna(x) else "indisponible"
    )
    st.dataframe(disp, use_container_width=True, hide_index=True)

    with st.expander("Promouvoir un candidat vers le DCA"):
        ticker = st.selectbox("Candidat", options=[a.ticker for a in candidates], key="watchlist_promote_select")
        price = get_price_eur(ticker)
        quantite = st.number_input(
            "Quantité DCA mensuelle", min_value=0.0, step=0.0001, format="%.6f",
            key="watchlist_promote_qty",
        )
        if price is not None:
            st.caption(f"≈ {quantite * price:,.2f} € au prix actuel ({price:,.2f} €)")
        else:
            st.caption("Prix actuel indisponible -- montant non estimable.")

        col1, col2 = st.columns(2)
        if col1.button("✅ Ajouter au DCA"):
            promote_to_dca(assets_data, ticker, quantite)
            save_assets_data(assets_data)
            st.rerun()
        if col2.button("🗑️ Retirer de la watchlist"):
            remove_asset(assets_data, ticker)
            save_assets_data(assets_data)
            st.rerun()


# --- Ajout d'un nouvel actif ---------------------------------------------

def _render_add_form(assets_data: dict) -> None:
    st.markdown("##### ➕ Ajouter un actif")
    with st.form("asset_add_form", clear_on_submit=True):
        name = st.text_input("Nom")
        ticker = st.text_input("Ticker (format yfinance)").strip().upper()
        in_dca = st.radio("Destination", options=["Watchlist", "DCA actif"], horizontal=True) == "DCA actif"
        quantite = (
            st.number_input("Quantité DCA mensuelle", min_value=0.0, step=0.0001, format="%.6f")
            if in_dca else 0.0
        )
        analysis_path = st.text_input("Chemin de l'analyse (optionnel)")
        last_update = st.date_input("Date de dernière analyse", value=None)
        submitted = st.form_submit_button("Ajouter")

    if submitted:
        if not name or not ticker:
            st.error("Nom et ticker sont obligatoires.")
            return
        try:
            add_asset(assets_data, Asset(
                name=name,
                ticker=ticker,
                in_dca=in_dca,
                analysis_path=analysis_path,
                last_analysis_update=last_update.isoformat() if last_update else "",
                quantite_dca_mensuelle=quantite,
            ))
            save_assets_data(assets_data)
            st.success(f"{ticker} ajouté.")
            st.rerun()
        except ValueError as e:
            st.error(str(e))