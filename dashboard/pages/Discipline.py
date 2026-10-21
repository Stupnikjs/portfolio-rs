"""Page Discipline : quota de trades par semaine (à placer dans dashboard/pages/)."""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from theme import apply_theme
from config import WEEKLY_TRADE_QUOTA
from data import load_data

TZ = "Europe/Paris"
GREEN, RED = "#2ecc71", "#e74c3c"
QUOTA = WEEKLY_TRADE_QUOTA

apply_theme()  # même en-tête que les autres pages
st.title("🎯 Discipline : quota de trades")

data = load_data()
raw = pd.DataFrame(data.get("trades", []))
if raw.empty:
    st.info("Aucun trade dans dashboard.json.")
    st.stop()

# --- Trades : heure locale, semaines du lundi au dimanche ------------------------------------
st.sidebar.subheader("🎯 Discipline")
group_fills = st.sidebar.checkbox(
    "Regrouper les exécutions d'un même ordre",
    value=True,
    help="Même actif, même sens, même minute = 1 trade (ex. une vente qui ferme plusieurs lots XTB).",
)

df = raw.copy()
df["time"] = pd.to_datetime(df["time"], utc=True).dt.tz_convert(TZ)
if group_fills:
    key = df["symbol"] + "|" + df["kind"] + "|" + df["time"].dt.floor("min").astype(str)
    df = df.loc[~key.duplicated()]

since = st.sidebar.date_input(
    "Compter à partir du",
    value=df["time"].min().date(),
    help="Mets la date du jour pour ne juger que les semaines à venir.",
)
df = df[df["time"].dt.date >= since].copy()
df["week"] = df["time"].dt.tz_localize(None).dt.to_period("W-SUN").dt.start_time  # lundi

# --- Série hebdomadaire complète (semaines sans trade = 0) -----------------------------------
current_week = pd.Timestamp.now(tz=TZ).tz_localize(None).to_period("W-SUN").start_time
start = df["week"].min() if not df.empty else current_week
weeks = pd.date_range(start, current_week, freq="7D")
counts = df.groupby("week").size().reindex(weeks, fill_value=0)

# --- Décompte de la semaine en cours ---------------------------------------------------------
used = int(counts.iloc[-1])
left = QUOTA - used

c1, c2, c3, c4 = st.columns(4)
if left >= 0:
    c1.metric("Restants cette semaine", f"{left} / {QUOTA}", f"{used} fait(s)", delta_color="off")
else:
    c1.metric("Quota dépassé", f"{used} / {QUOTA}", f"+{-left}", delta_color="inverse")

done = counts.iloc[:-1]  # semaines terminées
respected = (done <= QUOTA).tolist()
streak = 0
for ok in reversed(respected):
    if not ok:
        break
    streak += 1

c2.metric(
    "Semaines respectées",
    f"{sum(respected)} / {len(done)}" if len(done) else "—",
    f"{sum(respected) / len(done) * 100:.0f} %" if len(done) else None,
    delta_color="off",
)
c3.metric("Série en cours", f"{streak} sem.")
c4.metric("Moyenne / semaine", f"{done.mean():.1f}" if len(done) else "—")

st.progress(min(used / QUOTA, 1.0))
if left > 0:
    st.success(f"Il te reste {left} trade(s) cette semaine.")
elif left == 0:
    st.warning("Quota atteint : plus de trade cette semaine.")
else:
    st.error(f"Quota dépassé de {-left} trade(s) cette semaine.")

# --- Graphique : rouge si dépassé, vert sinon ------------------------------------------------
colors = [RED if n > QUOTA else GREEN for n in counts]
outline = [0] * (len(counts) - 1) + [2]  # contour sur la semaine en cours

fig = go.Figure(
    go.Bar(
        x=counts.index,
        y=counts.values,
        marker=dict(color=colors, line=dict(width=outline, color="#888888")),
        hovertemplate="Semaine du %{x|%d/%m/%Y}<br>%{y} trade(s)<extra></extra>",
    )
)
empty = counts[counts == 0]
if not empty.empty:  # un point vert pour les semaines sans trade (sinon aucune barre visible)
    fig.add_trace(
        go.Scatter(
            x=empty.index,
            y=[0] * len(empty),
            mode="markers",
            marker=dict(color=GREEN, size=8),
            hovertemplate="Semaine du %{x|%d/%m/%Y}<br>0 trade<extra></extra>",
        )
    )
fig.add_hline(y=QUOTA, line_dash="dash", annotation_text=f"Quota : {QUOTA}")
fig.update_layout(showlegend=False, bargap=0.25, xaxis_title=None, yaxis_title="Trades")
fig.update_yaxes(dtick=1, rangemode="tozero")
st.plotly_chart(fig, use_container_width=True)
st.caption(
    "Vert : quota respecté. Rouge : dépassé. Contour : semaine en cours. "
    "Un trade = achat ou vente d'un actif hors cash ; un convert Binance compte pour 2."
)

# --- Détail des semaines dépassées -----------------------------------------------------------
over_weeks = counts[counts > QUOTA].index
if len(over_weeks):
    with st.expander(f"Trades des {len(over_weeks)} semaine(s) dépassée(s)"):
        detail = df[df["week"].isin(over_weeks)].sort_values("time", ascending=False)
        table = pd.DataFrame(
            {
                "Semaine du": detail["week"].dt.strftime("%d/%m/%Y"),
                "Date": detail["time"].dt.strftime("%d/%m %H:%M"),
                "Actif": detail["symbol"],
                "Sens": detail["kind"],
                "Montant (€)": detail["value_eur"].round(2),
            }
        )
        st.dataframe(table, use_container_width=True, hide_index=True)
