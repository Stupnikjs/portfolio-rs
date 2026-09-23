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

from cash_data import load_cash_data, save_cash_data, compute_monthly_capacity
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

# --- Solde réel du compte (vérité terrain, checkpoint à date libre) ---
st.subheader("🏦 Solde réel du compte")
st.caption(
    "Ajoute un checkpoint quand tu veux -- pas forcément un par mois. "
    "Sert à corriger l'estimation ci-dessus, qui repose sur des dépenses approximatives."
)
 
df_soldes = (
    pd.DataFrame(cash_data["soldes_reels"])
    if cash_data["soldes_reels"]
    else pd.DataFrame(columns=["date", "solde"])
)
if not df_soldes.empty:
    df_soldes["date"] = pd.to_datetime(df_soldes["date"]).dt.date
 
edited_soldes = st.data_editor(
    df_soldes,
    column_config={
        "date": st.column_config.DateColumn("Date", format="DD/MM/YYYY", default=date.today()),
        "solde": st.column_config.NumberColumn("Solde réel (€)", step=10.0, format="%.2f €"),
    },
    hide_index=True,
    num_rows="dynamic",
    use_container_width=True,
    key="soldes_editor",
)
 
if not edited_soldes.equals(df_soldes):
    to_save = edited_soldes.dropna(subset=["date"]).copy()
    to_save["date"] = to_save["date"].apply(lambda d: d.isoformat() if not isinstance(d, str) else d)
    to_save = to_save.sort_values("date")
    cash_data["soldes_reels"] = to_save.to_dict("records")
    save_cash_data(cash_data)
    st.rerun()
 
# --- Évolution du solde réel (tous les checkpoints, granularité libre) ---
soldes_sorted = edited_soldes.dropna(subset=["date", "solde"]).sort_values("date").reset_index(drop=True)
if not soldes_sorted.empty:
    soldes_sorted["date"] = pd.to_datetime(soldes_sorted["date"])
 
if len(soldes_sorted) >= 2:
    fig_solde = px.line(
        soldes_sorted, x="date", y="solde", markers=True,
        labels={"solde": "Solde réel (€)", "date": "Date"},
    )
    fig_solde.update_layout(margin=dict(t=10, b=0, l=0, r=0))
    # st.plotly_chart(fig_solde, use_container_width=True)
   
 
st.divider()

# --- Capacité d'investissement par mois ---
st.subheader("📊 Capacité d'investissement")

col1, col2, col3 = st.columns(3)
col1.metric("Salaire net", f"{salaire:,.2f} €")
col2.metric("Dépenses fixes", f"{total_fixes:,.2f} €")
col3.metric(
    "Flux mensuel théorique",
    f"{salaire - total_fixes:,.2f} €",
    help="Salaire net - dépenses fixes, avant dépenses ponctuelles.",
)

horizon = st.slider("Mois futurs à projeter", min_value=0, max_value=12, value=3)

capacity_df = compute_monthly_capacity(
    salaire=salaire,
    total_fixes=total_fixes,
    ponctuelles=edited_ponctuelles,
    soldes=edited_soldes,
    horizon_mois=horizon,
)

st.caption(
    "Capacité = épargne de départ + salaire - dépenses fixes - ponctuelles du mois. "
    "Un mois « Réel » reprend ton dernier checkpoint du mois (qui recale la chaîne) ; "
    "un mois « Projeté » part du solde du mois précédent en supposant que tu n'investis rien "
    "entre-temps. Pas de lissage : un mois négatif reste négatif."
)
if edited_soldes.dropna(subset=["date", "solde"]).empty:
    st.info("Aucun checkpoint : sans solde de départ, la capacité affichée se limite au flux du mois.")

fig_cap = px.bar(
    capacity_df,
    x="mois",
    y="capacite",
    color="source",
    color_discrete_map={"Réel": "#4C9AFF", "Projeté": "#F5B942", "Flux seul": "#8A8F98"},
    labels={"capacite": "Capacité d'investissement (€)", "mois": "Mois", "source": ""},
)
fig_cap.add_hline(y=0, line_dash="dash", line_color="gray")
fig_cap.update_layout(margin=dict(t=10, b=0, l=0, r=0))
st.plotly_chart(fig_cap, use_container_width=True)

euro = lambda label: st.column_config.NumberColumn(label, format="%.2f €")
st.dataframe(
    capacity_df[["mois", "epargne_depart", "salaire", "fixes", "ponctuelles", "flux", "capacite", "source", "ecart"]],
    column_config={
        "mois": "Mois",
        "epargne_depart": euro("Épargne de départ"),
        "salaire": euro("Salaire"),
        "fixes": euro("Fixes"),
        "ponctuelles": euro("Ponctuelles"),
        "flux": euro("Flux du mois"),
        "capacite": euro("Capacité d'investissement"),
        "source": "Source",
        "ecart": euro("Écart réel vs estimé"),
    },
    hide_index=True,
    use_container_width=True,
)