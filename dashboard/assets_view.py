"""Section Streamlit Actifs : DCA actif + Watchlist. À appeler depuis app.py :

    from assets_data import load_assets_data
    from assets_view import render_assets_section
    ...
    assets_data_ = load_assets_data()
    render_assets_section(assets_data_)
"""
from __future__ import annotations

import hashlib
import json
import re
from pathlib import Path

import pandas as pd
import streamlit as st

from assets_data import (
    ANALYST_SCHEMA,
    FINANCIAL_SCHEMA,
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
    update_thesis,
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


def _has_content(block: dict | None) -> bool:
    """Un bloc au schéma fixe existe toujours : il a du contenu si une valeur est non nulle."""
    return any(v is not None for v in (block or {}).values())


def _thesis_for(a: Asset) -> tuple[dict | None, str | None]:
    """Thèse stockée dans assets.json en priorité ; sinon repli sur l'ancien
    fichier pointé par analysis_path (JSON généré ou markdown)."""
    own = {
        "thesis": a.thesis,
        "invalidation_scenario": a.invalidation_scenario,
        "donnees_financieres": a.donnees_financieres,
        "avis_analystes": a.avis_analystes,
    }
    if a.thesis or a.invalidation_scenario or _has_content(a.donnees_financieres) or _has_content(a.avis_analystes):
        return own, None
    return _load_thesis(a.analysis_path)


_EMPTY = "—"


def _num(v, dec: int = 1, suffix: str = "", signed: bool = False) -> str:
    """Format français : 1 234,5 — et « — » si la valeur est absente."""
    if v is None:
        return _EMPTY
    if isinstance(v, str):
        return v
    txt = f"{v:+,.{dec}f}" if signed else f"{v:,.{dec}f}"
    return txt.replace(",", "\u202f").replace(".", ",") + suffix


def _table(rows: list[dict], columns: list[str]) -> None:
    """Affiche un tableau en ignorant les lignes entièrement vides."""
    rows = [r for r in rows if any(r[c] != _EMPTY for c in columns[1:])]
    if rows:
        st.dataframe(pd.DataFrame(rows, columns=columns), hide_index=True, use_container_width=True)


def _render_financial_data(d: dict) -> None:
    dev = d.get("devise_cours") or ""
    md = f" Md {dev}".rstrip()

    if d.get("date_donnees"):
        st.caption(f"Données au {d['date_donnees']}")

    left, right = st.columns(2)

    # --- Gauche : valorisation / rentabilité cash + comptes ------------------
    with left:
        st.markdown("**Valorisation & rentabilité cash**")
        cols = ["Indicateur", "12 derniers mois", "2026 (prév.)", "2027 (prév.)"]
        cap = d.get("capitalisation_md")

        def _fcf_yield(fcf):
            return fcf / cap * 100 if fcf is not None and cap else None

        def _conversion(fcf, net):
            return fcf / net * 100 if fcf is not None and net else None

        fcf = {"ttm": d.get("fcf_ttm_md"), "2026": d.get("fcf_2026_md"), "2027": d.get("fcf_2027_md")}
        val = [
            ("P/E", d.get("pe_ttm"), d.get("pe_2026"), d.get("pe_2027"), 1, "x"),
            ("EV / EBITDA", d.get("ev_ebitda_ttm"), d.get("ev_ebitda_2026"), d.get("ev_ebitda_2027"), 1, "x"),
            ("EV / CA", None, d.get("ev_ca_2026"), d.get("ev_ca_2027"), 2, "x"),
            ("Free cash flow", fcf["ttm"], fcf["2026"], fcf["2027"], 1, md),
            ("Rendement FCF", _fcf_yield(fcf["ttm"]), _fcf_yield(fcf["2026"]), _fcf_yield(fcf["2027"]), 1, " %"),
            ("FCF / résultat net", None,
             _conversion(fcf["2026"], d.get("resultat_net_2026_md")),
             _conversion(fcf["2027"], d.get("resultat_net_2027_md")), 0, " %"),
        ]
        _table(
            [{"Indicateur": lbl, cols[1]: _num(t, dec, unit), cols[2]: _num(y1, dec, unit), cols[3]: _num(y2, dec, unit)}
             for lbl, t, y1, y2, dec, unit in val],
            cols,
        )
        st.caption("Rendement FCF = free cash flow / capitalisation. Conversion = free cash flow / résultat net.")

        st.markdown("**Comptes**")
        yearly = [
            ("Résultat net", "resultat_net_{y}_md", 1, md),
            ("Dette nette", "endettement_net_{y}_md", 1, md),
        ]
        years = ("2026", "2027")
        _table(
            [{"Indicateur": lbl, **{y: _num(d.get(k.format(y=y)), dec, unit) for y in years}}
             for lbl, k, dec, unit in yearly],
            ["Indicateur", *years],
        )

    # --- Droite : performance vs pairs + profil ----------------------------
    with right:
        st.markdown("**Performance**")
        perf = []
        for lbl, k in (("1 an", "perf_1an"), ("3 ans", "perf_3ans")):
            a, p = d.get(f"{k}_pct"), d.get(f"{k}_moyenne_pairs_pct")
            perf.append({
                "Période": lbl,
                "Actif": _num(a, 1, " %", signed=True),
                "Moy. pairs": _num(p, 1, " %", signed=True),
                "Écart": _num(a - p, 1, " pts", signed=True) if a is not None and p is not None else _EMPTY,
            })
        _table(perf, ["Période", "Actif", "Moy. pairs", "Écart"])

        st.markdown("**Profil**")
        prod = d.get("produit_principal")
        profile = [
            ("Valeur d'entreprise", _num(d.get("valeur_entreprise_md"), 1, md)),
            ("Flottant", _num(d.get("flottant_pct"), 1, " %")),
            ("Produit principal", prod or _EMPTY),
            ("… part du CA", _num(d.get("part_produit_principal_ca_pct"), 1, " %")),
            ("… croissance dernier trim.", _num(d.get("croissance_produit_principal_dernier_trimestre_cer_pct"), 1, " %", signed=True)),
            ("Part USD du CA", _num(d.get("part_usd_ca_pct"), 1, " %")),
            ("Croissance CA dernier trim.", _num(d.get("croissance_ca_dernier_trimestre_cer_pct"), 1, " %", signed=True)),
            ("Croissance BNPA dernier trim.", _num(d.get("croissance_bnpa_dernier_trimestre_cer_pct"), 1, " %", signed=True)),
            ("Guidance croissance 2026", d.get("guidance_croissance_2026") or _EMPTY),
        ]
        _table([{"Indicateur": a, "Valeur": b} for a, b in profile], ["Indicateur", "Valeur"])


def _render_analysts(avis: dict) -> None:
    if not any(v is not None for v in avis.values()):
        return
    st.markdown("**Avis analystes**")
    cols = ["Recommandation", "Objectif de cours", "Potentiel", "Nb d'analystes"]
    _table(
        [{
            cols[0]: avis.get("recommandation") or _EMPTY,
            cols[1]: _num(avis.get("objectif_cours"), 2),
            cols[2]: _num(avis.get("potentiel_pct"), 1, " %", signed=True),
            cols[3]: _num(avis.get("nb_analystes"), 0),
        }],
        cols,
    )


def _render_thesis_json(thesis: dict) -> None:
    donnees = thesis.get("donnees_financieres") or {}
    if any(v is not None for v in donnees.values()):
        st.markdown("**Données financières**")
        _render_financial_data(donnees)

    _render_analysts(thesis.get("avis_analystes") or {})

    if thesis.get("thesis"):
        st.markdown("**Thèse**")
        st.markdown(thesis["thesis"])

    if thesis.get("invalidation_scenario"):
        st.markdown("**🚩 Scénario d'invalidation**")
        st.markdown(thesis["invalidation_scenario"])


def _parse_field(raw: str, kind: str):
    """Champ vide -> None ; texte -> texte ; nombre -> int ou float
    (virgule décimale acceptée). Lève ValueError si le nombre est invalide."""
    raw = raw.strip()
    if raw == "":
        return None
    if kind == "str":
        return raw
    cleaned = raw.replace(",", ".").replace(" ", "").replace("\u202f", "")
    if re.fullmatch(r"-?\d+", cleaned):
        return int(cleaned)
    return float(cleaned)


def _block_inputs(prefix: str, schema: dict, values: dict, ticker: str, ver: str) -> dict:
    """Un champ par clé du schéma (3 colonnes). Renvoie les textes saisis."""
    raw = {}
    cols = st.columns(3)
    for i, (key, kind) in enumerate(schema.items()):
        v = values.get(key)
        raw[key] = cols[i % 3].text_input(
            key.replace("_", " "),
            value="" if v is None else str(v),
            key=f"{prefix}_{ticker}_{key}_{ver}",
            help=("texte" if kind == "str" else "nombre") + " -- vide = non renseigné",
        )
    return raw


def _render_thesis_editor(assets_data: dict, a: Asset) -> None:
    """Édition d'un actif depuis sa carte : thèse, données financières et avis
    analystes (champs du schéma fixe), ou import d'un JSON. Tout est écrit
    dans le fichier JSON de l'actif."""
    base, _ = _thesis_for(a)
    base = base or {}

    # Les clés de widgets changent quand le contenu stocké change : les champs
    # se rafraîchissent après un import / une édition du fichier à la main.
    ver_thesis = hashlib.md5(
        (base.get("thesis", "") + base.get("invalidation_scenario", "")).encode("utf-8")
    ).hexdigest()[:8]
    ver_data = hashlib.md5(
        json.dumps([a.name, a.donnees_financieres, a.avis_analystes], sort_keys=True, default=str).encode("utf-8")
    ).hexdigest()[:8]

    with st.expander("✏️ Éditer", expanded=False):
        tab_thesis, tab_data, tab_import = st.tabs(["Thèse", "Données", "Import JSON"])

        # --- Thèse + scénario d'invalidation ---
        with tab_thesis:
            with st.form(f"thesis_form_{a.ticker}"):
                thesis = st.text_area(
                    "Thèse d'investissement (markdown accepté)",
                    value=base.get("thesis", ""),
                    height=220,
                    key=f"thesis_txt_{a.ticker}_{ver_thesis}",
                )
                invalidation = st.text_area(
                    "🚩 Scénario d'invalidation",
                    value=base.get("invalidation_scenario", ""),
                    height=110,
                    key=f"thesis_inv_{a.ticker}_{ver_thesis}",
                    help="Ce qui te ferait sortir de la position / abandonner la thèse.",
                )
                thesis_submitted = st.form_submit_button("💾 Enregistrer la thèse")
            if thesis_submitted:
                payload = {"thesis": thesis, "invalidation_scenario": invalidation}
                # Migration douce : on reprend données/avis de l'ancien fichier
                # d'analyse (clés du schéma uniquement) si l'actif n'en a pas encore.
                for name, schema in (("donnees_financieres", FINANCIAL_SCHEMA), ("avis_analystes", ANALYST_SCHEMA)):
                    if not _has_content(getattr(a, name)) and isinstance(base.get(name), dict):
                        legacy = {k: v for k, v in base[name].items() if k in schema}
                        if legacy:
                            payload[name] = legacy
                update_thesis(assets_data, a.ticker, payload)
                save_assets_data(assets_data)
                st.rerun()

        # --- Nom + données financières + avis analystes (schéma fixe) ---
        with tab_data:
            with st.form(f"data_form_{a.ticker}"):
                new_name = st.text_input("Nom", value=a.name, key=f"name_{a.ticker}_{ver_data}")
                st.markdown("**Données financières**")
                raw_fin = _block_inputs("fin", FINANCIAL_SCHEMA, a.donnees_financieres, a.ticker, ver_data)
                st.markdown("**Avis analystes**")
                raw_avis = _block_inputs("avis", ANALYST_SCHEMA, a.avis_analystes, a.ticker, ver_data)
                data_submitted = st.form_submit_button("💾 Enregistrer les données")
            if data_submitted:
                errors, parsed = [], {}
                for name, schema, raw in (
                    ("donnees_financieres", FINANCIAL_SCHEMA, raw_fin),
                    ("avis_analystes", ANALYST_SCHEMA, raw_avis),
                ):
                    parsed[name] = {}
                    for key, kind in schema.items():
                        try:
                            parsed[name][key] = _parse_field(raw[key], kind)
                        except ValueError:
                            errors.append(f"- **{key}** : nombre invalide (« {raw[key]} »)")
                if not new_name.strip():
                    errors.append("- **Nom** : ne peut pas être vide")
                if errors:
                    st.error("Rien n'a été enregistré :\n" + "\n".join(errors))
                else:
                    update_asset(assets_data, a.ticker, name=new_name.strip())
                    update_thesis(assets_data, a.ticker, parsed)
                    save_assets_data(assets_data)
                    st.rerun()

        # --- Import d'un JSON complet ---
        with tab_import:
            st.caption("Colle un JSON (thesis, invalidation_scenario, donnees_financieres, avis_analystes) : "
                       "seules les clés fournies sont modifiées.")
            raw_json = st.text_area("JSON", height=140, key=f"thesis_import_{a.ticker}_{ver_data}_{ver_thesis}",
                                    label_visibility="collapsed")
            if st.button("📥 Importer le JSON", key=f"thesis_import_btn_{a.ticker}"):
                try:
                    update_thesis(assets_data, a.ticker, json.loads(raw_json))
                except (ValueError, json.JSONDecodeError) as e:
                    st.error(f"Import impossible : {e}")
                else:
                    save_assets_data(assets_data)
                    st.rerun()


def _render_asset_card(assets_data: dict, a: Asset, price: float | None, montant: float | None = None) -> None:
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

        thesis_json, thesis_md = _thesis_for(a)
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
                st.caption("Aucune thèse renseignée.")

        _render_thesis_editor(assets_data, a)


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
        _render_asset_card(assets_data, a, price, montant)

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
        _render_asset_card(assets_data, a, price)

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
        thesis = st.text_area("Thèse d'investissement (optionnel)", height=160)
        invalidation = st.text_area("🚩 Scénario d'invalidation (optionnel)", height=90)
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
                quantite_dca_mensuelle=quantite,
            ))
            if thesis.strip() or invalidation.strip():
                update_thesis(assets_data, ticker, {"thesis": thesis, "invalidation_scenario": invalidation})
            save_assets_data(assets_data)
            st.success(f"{ticker} ajouté.")
            st.rerun()
        except ValueError as e:
            st.error(str(e))