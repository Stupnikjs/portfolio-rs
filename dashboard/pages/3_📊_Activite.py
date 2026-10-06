"""Page 4 : activité de trading -- fréquence des achats/ventes (par mois ou
par semaine) et historique filtrable des trades.

Lit uniquement `trades` et `trade_frequency` dans dashboard.json (pipeline
Rust, lecture seule). Aucune persistance propre à cette page."""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st

from data import load_data
from theme import BG_COLOR, TEXT_COLOR, GRID_COLOR, PASTEL_DARK_SEQUENCE

# Applique uniquement le template Plotly (set_page_config est déjà géré par app.py)
if "portfolio_dark" not in pio.templates:
    pio.templates["portfolio_dark"] = dict(
        layout=dict(
            paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
            xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR),
        )
    )
pio.templates.default = "portfolio_dark"

BUY_COLOR = PASTEL_DARK_SEQUENCE[0]   # bleu
SELL_COLOR = PASTEL_DARK_SEQUENCE[2]  # orange
WEEKS_DEFAULT = 52

st.title("📊 Activité")

data = load_data()
trades = data.get("trades") or []
freq = data.get("trade_frequency") or {}

if not trades:
    st.info("Aucun trade dans dashboard.json.")
    st.stop()

df = pd.DataFrame(trades)
df["time"] = pd.to_datetime(df["time"], utc=True)
df = df.sort_values("time", ascending=False).reset_index(drop=True)
df["date"] = df["time"].dt.date

# --- KPIs ---------------------------------------------------------------------
buys_eur = float(df.loc[df["kind"] == "Buy", "value_eur"].sum())
sells_eur = float(df.loc[df["kind"] == "Sell", "value_eur"].sum())

c1, c2, c3, c4, c5 = st.columns(5)
c1.metric("Trades", f"{freq.get('total_trades', len(df)):,}")
c2.metric("Moyenne / mois", f"{freq.get('avg_per_month', 0.0):.1f}")
c3.metric("Moyenne / semaine", f"{freq.get('avg_per_week', 0.0):.1f}")
c4.metric("Total acheté", f"{buys_eur:,.0f} €")
c5.metric("Total vendu", f"{sells_eur:,.0f} €")

st.divider()

# --- Fréquence ----------------------------------------------------------------
st.subheader("Fréquence")

granularity = st.radio("Période", ["Mois", "Semaine"], horizontal=True, label_visibility="collapsed")
rows = freq.get("by_month" if granularity == "Mois" else "by_week") or []
periods = pd.DataFrame(rows)

if periods.empty:
    st.info("Pas de données de fréquence.")
else:
    if granularity == "Semaine":
        show_all = st.checkbox("Afficher toute la période", value=False)
        if not show_all:
            periods = periods.tail(WEEKS_DEFAULT)
            st.caption(f"{WEEKS_DEFAULT} dernières semaines (la période est la fin de semaine, dimanche).")

    left, right = st.columns(2)

    fig_count = go.Figure()
    fig_count.add_bar(x=periods["period"], y=periods["buys"], name="Achats", marker_color=BUY_COLOR)
    fig_count.add_bar(x=periods["period"], y=periods["sells"], name="Ventes", marker_color=SELL_COLOR)
    fig_count.update_layout(
        barmode="stack", title="Nombre de trades", height=360,
        margin=dict(l=10, r=10, t=50, b=10), xaxis=dict(type="category"),
        legend=dict(orientation="h", y=1.1, x=0),
    )
    left.plotly_chart(fig_count, use_container_width=True)

    fig_vol = go.Figure()
    fig_vol.add_bar(x=periods["period"], y=periods["volume_eur"], name="Volume", marker_color=BUY_COLOR)
    fig_vol.update_layout(
        title="Volume échangé (€)", height=360,
        margin=dict(l=10, r=10, t=50, b=10), xaxis=dict(type="category"),
        yaxis=dict(ticksuffix=" €"), showlegend=False,
    )
    right.plotly_chart(fig_vol, use_container_width=True)

st.divider()

# --- Historique filtrable -------------------------------------------------------
st.subheader("Historique")

f1, f2, f3, f4 = st.columns([2, 1, 1, 2])
symbols = f1.multiselect("Actif", sorted(df["symbol"].unique()))
kinds = f2.multiselect("Type", sorted(df["kind"].unique()))
platforms = f3.multiselect("Plateforme", sorted(df["platform"].unique()))
date_range = f4.date_input("Dates", value=(df["date"].min(), df["date"].max()))

mask = pd.Series(True, index=df.index)
if symbols:
    mask &= df["symbol"].isin(symbols)
if kinds:
    mask &= df["kind"].isin(kinds)
if platforms:
    mask &= df["platform"].isin(platforms)
# date_input renvoie un tuple d'un élément tant que la plage n'est pas complète
if isinstance(date_range, (tuple, list)) and len(date_range) == 2:
    mask &= (df["date"] >= date_range[0]) & (df["date"] <= date_range[1])

view = df[mask]

st.caption(
    f"{len(view)} trades — achats {view.loc[view['kind'] == 'Buy', 'value_eur'].sum():,.0f} €, "
    f"ventes {view.loc[view['kind'] == 'Sell', 'value_eur'].sum():,.0f} €"
)

st.dataframe(
    view[["time", "symbol", "kind", "platform", "quantity", "unit_price_eur", "value_eur"]],
    hide_index=True,
    use_container_width=True,
    column_config={
        "time": st.column_config.DatetimeColumn("Date", format="DD/MM/YYYY HH:mm"),
        "symbol": "Actif",
        "kind": "Type",
        "platform": "Plateforme",
        "quantity": st.column_config.NumberColumn("Quantité", format="%.4f"),
        "unit_price_eur": st.column_config.NumberColumn("Prix unitaire", format="%.2f €"),
        "value_eur": st.column_config.NumberColumn("Valeur", format="%.2f €"),
    },
)

# --- Récap par actif (sur la sélection) ----------------------------------------
if not view.empty:
    st.subheader("Par actif")
    recap = (
        view.assign(
            achete=view["value_eur"].where(view["kind"] == "Buy", 0.0),
            vendu=view["value_eur"].where(view["kind"] == "Sell", 0.0),
        )
        .groupby("symbol")
        .agg(trades=("symbol", "size"), achete=("achete", "sum"), vendu=("vendu", "sum"), dernier=("time", "max"))
        .sort_values("trades", ascending=False)
        .reset_index()
    )
    st.dataframe(
        recap,
        hide_index=True,
        use_container_width=True,
        column_config={
            "symbol": "Actif",
            "trades": "Trades",
            "achete": st.column_config.NumberColumn("Acheté", format="%.0f €"),
            "vendu": st.column_config.NumberColumn("Vendu", format="%.0f €"),
            "dernier": st.column_config.DatetimeColumn("Dernier trade", format="DD/MM/YYYY"),
        },
    )