"""Section Streamlit Actifs : DCA actif + Watchlist. À appeler depuis app.py :

    from assets_data import load_assets_data
    from assets_view import render_assets_section
    ...
    assets_data_ = load_assets_data()
    render_assets_section(assets_data_)
"""
from __future__ import annotations

import json
from pathlib import Path

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


# --- Carte par actif --------------------------------------------------------

def _load_thesis(analysis_path: str) -> tuple[dict | None, str | None]:
    """Charge la thèse pointée par analysis_path. Renvoie (json_dict, None)
    si un .json structuré (généré par md_to_thesis_json.py) est trouvé --
    soit directement, soit à côté d'un .md de même nom -- sinon
    (None, texte_markdown_brut), sinon (None, None) si rien n'est lisible."""
    if not analysis_path:
        return None, None
    path = Path(analysis_path)

    json_path = path if path.suffix == ".json" else path.with_suffix(".json")
    if json_path.exists():
        try:
            return json.loads(json_path.read_text(encoding="utf-8")), None
        except Exception:
            pass

    if path.exists():
        try:
            return None, path.read_text(encoding="utf-8")
        except Exception:
            pass

    return None, None


def _render_thesis_json(thesis: dict) -> None:
    donnees = thesis.get("donnees_financieres") or {}
    if donnees:
        st.markdown("**Données financières**")
        st.markdown(" · ".join(f"{k} : {v}" for k, v in donnees.items() if v is not None))

    avis = thesis.get("avis_analystes") or {}
    if avis:
        bits = []
        if avis.get("recommandation"):
            bits.append(f"**{avis['recommandation']}**")
        if avis.get("nb_analystes"):
            bits.append(f"{avis['nb_analystes']} analystes")
        if avis.get("objectif_cours") is not None:
            bit = f"objectif {avis['objectif_cours']:,.2f}"
            if avis.get("potentiel_pct") is not None:
                bit += f" ({avis['potentiel_pct']:+.1f} %)"
            bits.append(bit)
        if bits:
            st.markdown("**Avis analystes**")
            st.markdown(" · ".join(bits))

    if thesis.get("thesis"):
        st.markdown("**Thèse**")
        st.markdown(thesis["thesis"])

    if thesis.get("invalidation_scenario"):
        st.markdown("**🚩 Scénario d'invalidation**")
        st.markdown(thesis["invalidation_scenario"])


def _render_asset_card(a: Asset, price: float | None, montant: float | None = None) -> None:
    """Une carte par actif : nom/ticker, prix bien visible, éventuellement
    le montant DCA/mois, et la thèse d'investissement dépliable."""
    with st.container(border=True):
        col_head, col_price = st.columns([3, 1])
        with col_head:
            st.markdown(f"**{a.name}** · `{a.ticker}`")
            if montant is not None:
                st.caption(f"{a.quantite_dca_mensuelle:g} / mois ≈ {montant:,.2f} €/mois")
            elif a.in_dca:
                st.caption(f"{a.quantite_dca_mensuelle:g} / mois -- montant non estimable")
        with col_price:
            if price is not None:
                st.markdown(f"### {price:,.2f} €")
            else:
                st.markdown("### —")
                st.caption("prix indisponible")

        thesis_json, thesis_md = _load_thesis(a.analysis_path)
        label = "📄 Thèse d'investissement"
        if a.last_analysis_update:
            label += f" (màj {a.last_analysis_update})"
        with st.expander(label, expanded=False):
            if thesis_json:
                _render_thesis_json(thesis_json)
            elif thesis_md:
                st.markdown(thesis_md)
            elif a.analysis_path:
                st.caption(f"Fichier introuvable : {a.analysis_path}")
            else:
                st.caption("Aucune note d'analyse renseignée.")


# --- DCA actif ----------------------------------------------------------------

def _render_dca_section(assets_data: dict) -> None:
    st.subheader("💶 DCA actif")
    dca_assets = get_dca_assets(assets_data)

    if not dca_assets:
        st.info("Aucun actif en DCA pour l'instant.")
        return

    priced = []
    for a in dca_assets:
        price = get_price_eur(a.ticker)
        montant = a.quantite_dca_mensuelle * price if price is not None else None
        priced.append((a, price, montant))

    total = sum(m for _, _, m in priced if m is not None)
    if any(m is None for _, _, m in priced):
        st.caption("⚠️ Prix indisponible pour au moins un actif -- total calculé hors actifs concernés.")
    st.metric("Total DCA mensuel", f"{total:,.0f} €")

    for a, price, montant in priced:
        _render_asset_card(a, price, montant)

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

    for a in candidates:
        price = get_price_eur(a.ticker)
        _render_asset_card(a, price)

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
