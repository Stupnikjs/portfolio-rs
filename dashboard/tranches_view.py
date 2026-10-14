"""Page Tranches : retour de chaque apport de cash (méthode des parts / NAV).

Lit `tranches` dans dashboard.json (pipeline Rust, lecture seule). La logique
pure (DataFrame, figures) est séparée du rendu Streamlit pour être testable,
comme `history_view.py`.
"""
from __future__ import annotations

import pandas as pd
import plotly.graph_objects as go
import plotly.io as pio
import streamlit as st

from data import load_data
from theme import BG_COLOR, GAIN_COLOR, GRID_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE, TEXT_COLOR

INVESTED_COLOR = PASTEL_DARK_SEQUENCE[2]  # jaune (comme le cost basis)
VALUE_COLOR = PASTEL_DARK_SEQUENCE[0]     # bleu (comme la valeur totale)


# --- Logique pure ---------------------------------------------------------------

def tranches_df(report: dict) -> pd.DataFrame:
    """Une ligne par tranche, avec le gain total (réalisé + latent)."""
    df = pd.DataFrame(report.get("tranches") or [])
    if df.empty:
        return df
    df["date"] = pd.to_datetime(df["date"])
    df["gain_eur"] = df["realized_gain_eur"] + df["unrealized_gain_eur"]
    return df.sort_values("date").reset_index(drop=True)


def build_roi_figure(df: pd.DataFrame) -> go.Figure:
    """ROI de chaque apport depuis sa date d'entrée (vert/rouge selon le signe)."""
    colors = [GAIN_COLOR if v >= 0 else LOSS_COLOR for v in df["roi_pct"]]
    annualized = df["annualized_pct"].map(lambda a: "—" if pd.isna(a) else f"{a:+.1f} %")
    fig = go.Figure(go.Bar(
        x=df["date"], y=df["roi_pct"], marker_color=colors,
        customdata=list(zip(df["invested_eur"], df["gain_eur"], annualized)),
        hovertemplate=(
            "ROI %{y:+.2f} %<br>Apporté %{customdata[0]:,.0f} €"
            "<br>Gain %{customdata[1]:+,.0f} €<br>Annualisé %{customdata[2]}<extra></extra>"
        ),
    ))
    fig.update_layout(
        height=360, margin=dict(l=10, r=10, t=30, b=10),
        yaxis=dict(ticksuffix=" %", zeroline=True), xaxis=dict(title="Date de l'apport"),
    )
    return fig


def build_compare_figure(df: pd.DataFrame) -> go.Figure:
    """Apporté vs valeur actuelle, tranche par tranche."""
    fig = go.Figure()
    fig.add_trace(go.Bar(x=df["date"], y=df["invested_eur"], name="Apporté",
                         marker_color=INVESTED_COLOR, hovertemplate="%{y:,.0f} €"))
    fig.add_trace(go.Bar(x=df["date"], y=df["current_value_eur"], name="Valeur actuelle",
                         marker_color=VALUE_COLOR, hovertemplate="%{y:,.0f} €"))
    fig.update_layout(
        barmode="group", height=360, margin=dict(l=10, r=10, t=30, b=10),
        yaxis=dict(ticksuffix=" €"), xaxis=dict(title="Date de l'apport"),
        legend=dict(orientation="h", y=1.1, x=0),
    )
    return fig


def display_table(df: pd.DataFrame) -> pd.DataFrame:
    out = df[["date", "invested_eur", "current_value_eur", "gain_eur", "roi_pct", "annualized_pct", "inferred"]]
    return out.sort_values("date", ascending=False).reset_index(drop=True)


# --- Rendu Streamlit ---------------------------------------------------------------

def _ensure_dark_template() -> None:
    if "portfolio_dark" not in pio.templates:
        pio.templates["portfolio_dark"] = dict(
            layout=dict(
                paper_bgcolor=BG_COLOR, plot_bgcolor=BG_COLOR, font=dict(color=TEXT_COLOR),
                xaxis=dict(gridcolor=GRID_COLOR), yaxis=dict(gridcolor=GRID_COLOR),
            )
        )
    pio.templates.default = "portfolio_dark"


def render_tranches_page() -> None:
    _ensure_dark_template()
    st.title("💶 Retour par apport")

    reports = load_data().get("tranches") or []
    if not reports:
        st.info("Aucun rapport par tranche dans dashboard.json : relance `cargo run`.")
        st.stop()

    names = [r["account"] for r in reports]
    account = names[0] if len(names) == 1 else st.radio("Compte", names, horizontal=True)
    report = next(r for r in reports if r["account"] == account)

    c1, c2, c3, c4 = st.columns(4)
    c1.metric("Apports nets", f"{report['net_contributions_eur']:,.0f} €")
    c2.metric("Valeur actuelle", f"{report['current_value_eur']:,.0f} €",
              f"{report['total_gain_eur']:+,.0f} €")
    c3.metric("Rendement du compte (TWR)", f"{report['time_weighted_return_pct']:+.2f} %")
    c4.metric("Valeur de la part (NAV)", f"{report['nav_now']:.4f}")

    for warning in report.get("warnings") or []:
        st.warning(warning)

    df = tranches_df(report)
    if df.empty:
        st.info("Aucune tranche : aucun apport externe détecté sur ce compte.")
        st.stop()

    st.subheader("ROI de chaque apport depuis son entrée")
    st.plotly_chart(build_roi_figure(df), use_container_width=True)
    st.caption(
        "Le retour d'une tranche ne dépend que de sa DATE d'entrée (le cash est fongible) : "
        "deux apports le même jour ont le même retour. La somme des gains des tranches "
        "égale le gain total du compte."
    )

    st.subheader("Apporté vs valeur actuelle")
    st.plotly_chart(build_compare_figure(df), use_container_width=True)

    st.dataframe(
        display_table(df), hide_index=True, use_container_width=True,
        column_config={
            "date": st.column_config.DateColumn("Date", format="DD/MM/YYYY"),
            "invested_eur": st.column_config.NumberColumn("Apporté", format="%.2f €"),
            "current_value_eur": st.column_config.NumberColumn("Valeur actuelle", format="%.2f €"),
            "gain_eur": st.column_config.NumberColumn("Gain (réalisé + latent)", format="%+.2f €"),
            "roi_pct": st.column_config.NumberColumn("ROI", format="%+.2f %%"),
            "annualized_pct": st.column_config.NumberColumn("Annualisé", format="%+.1f %%"),
            "inferred": st.column_config.CheckboxColumn("Déduit"),
        },
    )