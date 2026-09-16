"""Page 2 : cashflow personnel (salaire, dépenses fixes et ponctuelles,
solde réel du compte) -- persistant dans data/cash.json, totalement
indépendant du pipeline Rust (dashboard.json). Calcul brut, sans lissage :
une capacité d'investissement nulle ou négative un mois donné reste
affichée telle quelle."""
from __future__ import annotations

from datetime import date

import pandas as pd
import plotly.express as px
import plotly.io as pio
import streamlit as st

from cash_data import load_cash_data, save_cash_data
from theme import BG_COLOR, TEXT_COLOR, GRID_COLOR

# Applique uniquement le template Plotly (set_page_config est déjà géré par app.py)
if "portfolio_dark" not in pio.templates:
    pio.templates["portfolio_dark"] = dict(
        layout=dict(
            paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
            xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR)
        )
    )
pio.templates.default = "portfolio_dark"

st.title("💰 Cashflow personnel")

cash_data = load_cash_data()

# --- Salaire ---
st.subheader("Revenu")
salaire = st.number_input(
    "Salaire net mensuel (€)",
    min_value=0.0,
    value=float(cash_data["salaire_net_mensuel"]),
    step=50.0,
)
if salaire != cash_data["salaire_net_mensuel"]:
    cash_data["salaire_net_mensuel"] = salaire
    save_cash_data(cash_data)

st.divider()

# --- Dépenses fixes ---
# --- Dépenses fixes ---
st.subheader("Dépenses mensuelles fixes")
df_fixes = (
    pd.DataFrame(cash_data["depenses_fixes"])
    if cash_data["depenses_fixes"]
    else pd.DataFrame({"nom": pd.Series(dtype="object"), "montant": pd.Series(dtype="float64")})
)
df_fixes["montant"] = pd.to_numeric(df_fixes["montant"], errors="coerce")

edited_fixes = st.data_editor(
    df_fixes,
    column_config={
        "nom": st.column_config.TextColumn("Poste de dépense"),
        "montant": st.column_config.NumberColumn("Montant (€/mois)", min_value=0.0, step=10.0, format="%.2f €"),
    },
    hide_index=True,
    num_rows="dynamic",
    use_container_width=True,
    key="fixes_editor",
)

if not edited_fixes.equals(df_fixes):
    cash_data["depenses_fixes"] = edited_fixes.dropna(subset=["nom"]).to_dict("records")
    save_cash_data(cash_data)
    st.rerun()

total_fixes = edited_fixes["montant"].fillna(0).sum() if not edited_fixes.empty else 0.0

st.divider()

# --- Dépenses ponctuelles ---
st.subheader("Dépenses ponctuelles")
df_ponctuelles = (
    pd.DataFrame(cash_data["depenses_ponctuelles"])
    if cash_data["depenses_ponctuelles"]
    else pd.DataFrame(columns=["nom", "montant", "date"])
)
if not df_ponctuelles.empty:
    df_ponctuelles["date"] = pd.to_datetime(df_ponctuelles["date"]).dt.date

edited_ponctuelles = st.data_editor(
    df_ponctuelles,
    column_config={
        "nom": st.column_config.TextColumn("Dépense"),
        "montant": st.column_config.NumberColumn("Montant (€)", min_value=0.0, step=10.0, format="%.2f €"),
        "date": st.column_config.DateColumn("Date", format="DD/MM/YYYY", default=date.today()),
    },
    hide_index=True,
    num_rows="dynamic",
    use_container_width=True,
    key="ponctuelles_editor",
)

if not edited_ponctuelles.equals(df_ponctuelles):
    to_save = edited_ponctuelles.dropna(subset=["nom", "date"]).copy()
    to_save["date"] = to_save["date"].apply(lambda d: d.isoformat() if not isinstance(d, str) else d)
    cash_data["depenses_ponctuelles"] = to_save.to_dict("records")
    save_cash_data(cash_data)
    st.rerun()

st.divider()

# --- Capacité d'investissement ---
st.subheader("📊 Capacité d'investissement")

capacite_theorique = salaire - total_fixes

col1, col2, col3 = st.columns(3)
col1.metric("Salaire net", f"{salaire:,.2f} €")
col2.metric("Dépenses fixes", f"{total_fixes:,.2f} €")
col3.metric(
    "Capacité théorique",
    f"{capacite_theorique:,.2f} €",
    help="Salaire net - dépenses fixes. Ne tient pas compte des ponctuelles.",
)

# --- Capacité réelle par mois (brute, sans lissage) ---
if not edited_ponctuelles.empty:
    df_chart = edited_ponctuelles.copy()
    df_chart["date"] = pd.to_datetime(df_chart["date"])
    df_chart["mois"] = df_chart["date"].dt.to_period("M").astype(str)
    monthly_ponctuelles = df_chart.groupby("mois")["montant"].sum().reset_index()
    monthly_ponctuelles["capacite_reelle"] = capacite_theorique - monthly_ponctuelles["montant"]

    st.subheader("Capacité réelle par mois")
    st.caption("Capacité théorique - dépenses ponctuelles du mois. Aucun lissage : un mois à 0€ ou négatif reste affiché tel quel.")

    fig = px.bar(
        monthly_ponctuelles,
        x="mois",
        y="capacite_reelle",
        labels={"capacite_reelle": "Capacité réelle (€)", "mois": "Mois"},
    )
    fig.add_hline(y=0, line_dash="dash", line_color="gray")
    fig.update_layout(margin=dict(t=10, b=0, l=0, r=0))
    st.plotly_chart(fig, use_container_width=True)

    st.dataframe(
        monthly_ponctuelles.rename(columns={
            "mois": "Mois",
            "montant": "Dépenses ponctuelles (€)",
            "capacite_reelle": "Capacité réelle (€)",
        }),
        use_container_width=True,
        hide_index=True,
    )
else:
    st.info("Aucune dépense ponctuelle enregistrée -- la capacité réelle correspond à la capacité théorique chaque mois.")

st.divider()

# --- Solde réel du compte (vérité terrain, saisie manuelle mensuelle) ---
st.subheader("🏦 Solde réel du compte")
st.caption("Saisi à la main chaque mois -- sert à corriger l'estimation ci-dessus, qui repose sur des dépenses approximatives.")

df_soldes = (
    pd.DataFrame(cash_data["soldes_reels"])
    if cash_data["soldes_reels"]
    else pd.DataFrame(columns=["mois", "solde"])
)

edited_soldes = st.data_editor(
    df_soldes,
    column_config={
        "mois": st.column_config.TextColumn("Mois (YYYY-MM)", help="Ex: 2026-09"),
        "solde": st.column_config.NumberColumn("Solde réel (€)", step=10.0, format="%.2f €"),
    },
    hide_index=True,
    num_rows="dynamic",
    use_container_width=True,
    key="soldes_editor",
)

if not edited_soldes.equals(df_soldes):
    to_save = edited_soldes.dropna(subset=["mois"]).sort_values("mois")
    cash_data["soldes_reels"] = to_save.to_dict("records")
    save_cash_data(cash_data)
    st.rerun()

# --- Comparaison estimé vs réel ---
if len(edited_soldes) >= 2:
    st.subheader("Écart estimé vs réel")

    soldes_sorted = edited_soldes.dropna(subset=["mois", "solde"]).sort_values("mois").reset_index(drop=True)
    soldes_sorted["variation_reelle"] = soldes_sorted["solde"].diff()

    if not edited_ponctuelles.empty:
        df_pct = edited_ponctuelles.copy()
        df_pct["date"] = pd.to_datetime(df_pct["date"])
        df_pct["mois"] = df_pct["date"].dt.to_period("M").astype(str)
        ponctuelles_par_mois = df_pct.groupby("mois")["montant"].sum()
    else:
        ponctuelles_par_mois = pd.Series(dtype=float)

    soldes_sorted["capacite_theorique_du_mois"] = soldes_sorted["mois"].apply(
        lambda m: capacite_theorique - ponctuelles_par_mois.get(m, 0.0)
    )
    soldes_sorted["ecart"] = soldes_sorted["variation_reelle"] - soldes_sorted["capacite_theorique_du_mois"]

    comparison = soldes_sorted.dropna(subset=["variation_reelle"])[
        ["mois", "solde", "variation_reelle", "capacite_theorique_du_mois", "ecart"]
    ]

    st.dataframe(
        comparison.rename(columns={
            "mois": "Mois",
            "solde": "Solde réel (€)",
            "variation_reelle": "Variation réelle (€)",
            "capacite_theorique_du_mois": "Capacité estimée (€)",
            "ecart": "Écart (€)",
        }),
        use_container_width=True,
        hide_index=True,
    )

    fig_ecart = px.bar(
        comparison,
        x="mois",
        y=["variation_reelle", "capacite_theorique_du_mois"],
        barmode="group",
        labels={"value": "Montant (€)", "mois": "Mois", "variable": ""},
    )
    fig_ecart.update_layout(margin=dict(t=10, b=0, l=0, r=0), legend_title_text="")
    st.plotly_chart(fig_ecart, use_container_width=True)
elif len(edited_soldes) == 1:
    st.info("Ajoute le solde d'au moins un deuxième mois pour voir la variation réelle et la comparer à l'estimation.")
else:
    st.info("Aucun solde réel enregistré pour le moment.")