"""Page Stratégie : analyse du timing des achats/ventes d'un actif face à son cours.

Croise deux sources :
  - history.json : prix hebdomadaires (un point par snapshot, le dimanche soir).
  - dashboard.json -> trades : achats/ventes avec prix exact et timestamp.

But : visualiser si tu achètes haut et vends bas (ou l'inverse), et détecter l'overtrading
(trades trop rapprochés). La moyenne mobile sert de référence de trend : un achat sous la MA
= bon timing d'entrée, une vente au-dessus = bon timing de sortie.
"""
from __future__ import annotations

import numpy as np
import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from portfolio.data import load_data, load_portfolio
from portfolio.history import PERIODS, load_history
from portfolio.theme import BG_COLOR, GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

LINE_COLOR = PASTEL_DARK_SEQUENCE[0]   # bleu : courbe de prix
MA_COLOR = PASTEL_DARK_SEQUENCE[4]     # violet : moyenne mobile
BUY_COLOR = GAIN_COLOR                 # vert : achat
SELL_COLOR = LOSS_COLOR                # rouge : vente
MARGIN = dict(l=10, r=10, t=30, b=10)

# Fenêtre par défaut de la moyenne mobile (en semaines de snapshot).
DEFAULT_MA_WEEKS = 4  # ~1 mois
# En dessous de ce seuil (jours moyens entre trades), on alerte sur l'overtrading.
OVERTRADING_THRESHOLD_DAYS = 7
# Seuils d'alerte sur le biais de timing (% vs MA).
TIMING_BIAS_WARN_PCT = 5.0


# --- Extraction des données --------------------------------------------------------

def asset_price_series(history: pd.DataFrame | None, symbol: str) -> pd.DataFrame:
    """Série de prix hebdomadaire pour `symbol`, depuis les snapshots de history.json.

    Colonnes : date, price_eur, quantity, value_eur.
    Ignore les snapshots sans cet actif ou avec un prix à 0 (manquant / reporté).
    """
    cols = ["date", "price_eur", "quantity", "value_eur"]
    if history is None or history.empty:
        return pd.DataFrame(columns=cols)

    rows = []
    for _, entry in history.iterrows():
        assets = entry.get("assets")
        if not isinstance(assets, list):
            continue
        for a in assets:
            if a.get("symbol") != symbol:
                continue
            price = float(a.get("price_eur") or 0.0)
            if price <= 0:
                break  # prix manquant : on ne garde pas ce point
            rows.append({
                "date": entry["date"],
                "price_eur": price,
                "quantity": float(a.get("quantity") or 0.0),
                "value_eur": float(a.get("value_eur") or 0.0),
            })
            break  # un seul actif par snapshot

    return pd.DataFrame(rows).sort_values("date").reset_index(drop=True)


def asset_trades(data: dict, symbol: str) -> pd.DataFrame:
    """Trades (Buy/Sell) pour `symbol`, triés par date.

    Colonnes : time (UTC), kind, quantity, unit_price_eur, value_eur, platform.
    Ajoute `time_naive` (UTC tz-naive) pour les graphiques (aligné avec history.json).
    """
    trades = data.get("trades") or []
    if not trades:
        return pd.DataFrame(columns=["time", "time_naive", "kind", "quantity", "unit_price_eur", "value_eur", "platform"])

    df = pd.DataFrame(trades)
    df = df[df["symbol"] == symbol].copy()
    df["time"] = pd.to_datetime(df["time"], utc=True)
    df["time_naive"] = df["time"].dt.tz_localize(None)  # pour aligner avec history.json (UTC naïf)
    return df.sort_values("time").reset_index(drop=True)


def eligible_symbols(history: pd.DataFrame | None, data: dict) -> list[str]:
    """Symboles ayant soit des trades soit un historique de prix : candidats pour la page."""
    symbols: set[str] = set()

    for t in data.get("trades") or []:
        if t.get("symbol"):
            symbols.add(t["symbol"])

    if history is not None and not history.empty:
        for assets in history["assets"]:
            if not isinstance(assets, list):
                continue
            for a in assets:
                if a.get("symbol") and float(a.get("price_eur") or 0.0) > 0:
                    symbols.add(a["symbol"])

    return sorted(symbols)


# --- Analyse du timing --------------------------------------------------------

def rolling_average(prices: pd.DataFrame, window: int = DEFAULT_MA_WEEKS) -> pd.Series:
    """Moyenne mobile sur `window` snapshots hebdomadaires. Index = dates des prix."""
    if prices.empty or window < 1:
        return pd.Series(dtype=float)
    return prices.set_index("date")["price_eur"].rolling(window=window, min_periods=1).mean()


def ma_at_trades(ma: pd.Series, trades: pd.DataFrame) -> pd.Series:
    """Pour chaque trade, la valeur interpolée de la MA au moment exact du trade.

    La MA est calculée sur des dates de snapshots (dimanches). Les trades arrivent n'importe
    quel jour : on interpole linéairement pour obtenir la valeur de la MA au moment du trade.
    Avant le 1er snapshot : clamp à la 1re valeur de MA.
    """
    if ma.empty or trades.empty:
        return pd.Series(dtype=float)
    ma = ma.dropna()
    if ma.empty:
        return pd.Series([np.nan] * len(trades), index=trades.index)

    # np.interp veut des coordonnées numériques : on convertit les dates en nanosecondes.
    ma_x = ma.index.asi8
    ma_y = ma.values
    trade_x = pd.DatetimeIndex(trades["time_naive"]).asi8
    interpolated = np.interp(trade_x, ma_x, ma_y)
    return pd.Series(interpolated, index=trades.index)


def timing_analysis(trades: pd.DataFrame, prices: pd.DataFrame, ma_window: int = DEFAULT_MA_WEEKS) -> dict:
    """Analyse du timing : compare chaque trade à la moyenne mobile au moment du trade.

    Un achat sous la MA = bon timing (tu as acheté sous le trend).
    Une vente au-dessus de la MA = bon timing (tu as vendu au-dessus du trend).
    Renvoie aussi le biais moyen (% d'écart vs MA) pour détecter un pattern systématique.
    """
    out = {
        "n_buys": 0, "n_sells": 0,
        "buys_below_ma": 0, "sells_above_ma": 0,
        "buy_timing_pct": None, "sell_timing_pct": None,
        "avg_buy_vs_ma_pct": None, "avg_sell_vs_ma_pct": None,
    }

    if trades.empty or prices.empty:
        return out

    ma = rolling_average(prices, ma_window)
    if ma.dropna().empty:
        return out

    ma_at = ma_at_trades(ma, trades)

    buys = trades[trades["kind"] == "Buy"]
    sells = trades[trades["kind"] == "Sell"]

    out["n_buys"] = len(buys)
    out["n_sells"] = len(sells)

    def _diff_pct(trade_prices: pd.Series, ma_vals: pd.Series) -> np.ndarray:
        valid = ma_vals.notna() & (ma_vals > 0) & trade_prices.notna() & (trade_prices > 0)
        if not valid.any():
            return np.array([])
        return (trade_prices.loc[valid].values / ma_vals.loc[valid].values - 1) * 100

    if not buys.empty:
        diffs = _diff_pct(buys["unit_price_eur"], ma_at.loc[buys.index])
        if diffs.size:
            out["buys_below_ma"] = int((diffs < 0).sum())
            out["buy_timing_pct"] = out["buys_below_ma"] / len(buys) * 100
            out["avg_buy_vs_ma_pct"] = float(np.nanmean(diffs))

    if not sells.empty:
        diffs = _diff_pct(sells["unit_price_eur"], ma_at.loc[sells.index])
        if diffs.size:
            out["sells_above_ma"] = int((diffs > 0).sum())
            out["sell_timing_pct"] = out["sells_above_ma"] / len(sells) * 100
            out["avg_sell_vs_ma_pct"] = float(np.nanmean(diffs))

    return out


def trade_frequency_stats(trades: pd.DataFrame) -> dict:
    """Stats de fréquence : nb de trades, jours moyens/médians entre trades, trades/mois.

    L'overtrading est signalé quand l'écart moyen entre trades consécutifs est court
    (sous OVERTRADING_THRESHOLD_DAYS).
    """
    out = {
        "n_trades": 0, "avg_days_between": None, "median_days_between": None,
        "min_gap_days": None, "trades_per_month": None, "overtrading": False,
    }
    if trades.empty:
        return out

    out["n_trades"] = len(trades)
    times = trades["time_naive"].sort_values()

    if len(times) >= 2:
        gaps = times.diff().dropna().dt.total_seconds() / 86400  # en jours
        out["avg_days_between"] = float(gaps.mean())
        out["median_days_between"] = float(gaps.median())
        out["min_gap_days"] = float(gaps.min())
        out["overtrading"] = out["avg_days_between"] < OVERTRADING_THRESHOLD_DAYS

        # Trades par mois (approximation : n_trades / nb de mois couverts)
        months = (times.iloc[-1].year - times.iloc[0].year) * 12 + (times.iloc[-1].month - times.iloc[0].month) + 1
        out["trades_per_month"] = len(times) / max(months, 1)

    return out


def net_flow(trades: pd.DataFrame) -> dict:
    """Flux net : total acheté, total vendu, net (acheté - vendu). En EUR."""
    if trades.empty:
        return {"bought": 0.0, "sold": 0.0, "net": 0.0}
    bought = float(trades.loc[trades["kind"] == "Buy", "value_eur"].sum())
    sold = float(trades.loc[trades["kind"] == "Sell", "value_eur"].sum())
    return {"bought": bought, "sold": sold, "net": bought - sold}


# --- Analyse progression (renforcement sur repli / vente par palier) ---------------

def buy_progression_analysis(trades: pd.DataFrame) -> pd.DataFrame:
    """Évolution du prix entre achats consécutifs, pour évaluer un renforcement sur repli.

    Stratégie « renforcement sur repli » : chaque achat suivant devrait être à un prix
    *inférieur* au précédent (tu renforces quand le prix baisse, tu abaisses ton PRU).

    Le 1er achat est la référence (point d'entrée). Pour chaque achat suivant :
      - evolution_from_prev_pct : (prix / prix_précédent - 1) * 100
      - evolution_from_first_pct : (prix / prix_premier - 1) * 100
      - on_dip : True si prix < prix_précédent (repli = bon timing)

    Colonnes : time, price, quantity, value_eur, prev_price, first_price,
               evolution_from_prev_pct, evolution_from_first_pct, on_dip.
    """
    buys = trades[trades["kind"] == "Buy"].sort_values("time_naive").reset_index(drop=True)
    if buys.empty:
        return pd.DataFrame(columns=[
            "time", "price", "quantity", "value_eur", "prev_price", "first_price",
            "evolution_from_prev_pct", "evolution_from_first_pct", "on_dip",
        ])

    result = pd.DataFrame({
        "time": buys["time_naive"].values,
        "price": buys["unit_price_eur"].values,
        "quantity": buys["quantity"].values,
        "value_eur": buys["value_eur"].values,
    })

    result["prev_price"] = result["price"].shift(1)
    result["first_price"] = result["price"].iloc[0]

    result["evolution_from_prev_pct"] = (
        (result["price"] / result["prev_price"] - 1) * 100
    ).where(result["prev_price"].notna() & (result["prev_price"] > 0))

    result["evolution_from_first_pct"] = (
        (result["price"] / result["first_price"] - 1) * 100
    ).where(result["first_price"] > 0)

    result["on_dip"] = result["evolution_from_prev_pct"] < 0
    return result


def sell_progression_analysis(trades: pd.DataFrame) -> pd.DataFrame:
    """Évolution du prix entre ventes consécutives, pour évaluer une vente par palier.

    Stratégie « vente par palier » : chaque vente suivante devrait être à un prix
    *supérieur* au précédent (tu vends par tranches quand le prix monte, tu maximises
    ton prix de sortie moyen).

    Le 1er vente est la référence. Pour chaque vente suivante :
      - evolution_from_prev_pct : (prix / prix_précédent - 1) * 100
      - evolution_from_first_pct : (prix / prix_première - 1) * 100
      - on_palier : True si prix > prix_précédent (palier ascendant = bon timing)
    """
    sells = trades[trades["kind"] == "Sell"].sort_values("time_naive").reset_index(drop=True)
    if sells.empty:
        return pd.DataFrame(columns=[
            "time", "price", "quantity", "value_eur", "prev_price", "first_price",
            "evolution_from_prev_pct", "evolution_from_first_pct", "on_palier",
        ])

    result = pd.DataFrame({
        "time": sells["time_naive"].values,
        "price": sells["unit_price_eur"].values,
        "quantity": sells["quantity"].values,
        "value_eur": sells["value_eur"].values,
    })

    result["prev_price"] = result["price"].shift(1)
    result["first_price"] = result["price"].iloc[0]

    result["evolution_from_prev_pct"] = (
        (result["price"] / result["prev_price"] - 1) * 100
    ).where(result["prev_price"].notna() & (result["prev_price"] > 0))

    result["evolution_from_first_pct"] = (
        (result["price"] / result["first_price"] - 1) * 100
    ).where(result["first_price"] > 0)

    result["on_palier"] = result["evolution_from_prev_pct"] > 0
    return result


def progression_summary(prog: pd.DataFrame, on_key: str) -> dict:
    """Résumé d'une progression (achats ou ventes).

    `on_key` = "on_dip" pour les achats, "on_palier" pour les ventes.
    Renvoie : n_trades, n_good (sur repli/palier), good_pct, avg_evo_from_prev_pct,
              total_evo_from_first_pct.
    """
    out = {
        "n_trades": 0, "n_good": 0, "good_pct": None,
        "avg_evo_from_prev_pct": None, "total_evo_from_first_pct": None,
    }
    if prog.empty:
        return out

    out["n_trades"] = len(prog)
    # Le 1er trade n'a pas de précédent : on compte sur les suivants.
    subsequent = prog.iloc[1:]
    if len(subsequent) == 0:
        return out

    out["n_good"] = int(subsequent[on_key].sum())
    out["good_pct"] = out["n_good"] / len(subsequent) * 100
    out["avg_evo_from_prev_pct"] = float(subsequent["evolution_from_prev_pct"].mean())
    out["total_evo_from_first_pct"] = float(prog["evolution_from_first_pct"].iloc[-1])
    return out


# --- Figures ------------------------------------------------------------------------

def strategy_figure(prices: pd.DataFrame, trades: pd.DataFrame, ma: pd.Series | None) -> go.Figure:
    """Cours (line) + moyenne mobile (dash) + marqueurs achat (triangle-up) / vente (triangle-down).

    Les marqueurs sont positionnés au prix exact du trade (unit_price_eur), pas sur la ligne
    de prix hebdomadaire : un trade au-dessus de la ligne = achat quand le cours était plus haut
    que le dernier snapshot.
    """
    fig = go.Figure()

    # Courbe de prix hebdomadaire
    if not prices.empty:
        fig.add_trace(go.Scatter(
            x=prices["date"], y=prices["price_eur"],
            name="Cours", mode="lines+markers",
            line=dict(color=LINE_COLOR, width=2),
            marker=dict(size=4),
            hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>Cours</extra>",
        ))

    # Moyenne mobile
    if ma is not None and not ma.dropna().empty:
        fig.add_trace(go.Scatter(
            x=ma.index, y=ma.values,
            name="MA", mode="lines",
            line=dict(color=MA_COLOR, width=1.5, dash="dash"),
            hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>MA</extra>",
        ))

    # Achats
    buys = trades[trades["kind"] == "Buy"] if not trades.empty else pd.DataFrame()
    if not buys.empty:
        fig.add_trace(go.Scatter(
            x=buys["time_naive"], y=buys["unit_price_eur"],
            name="Achat", mode="markers",
            marker=dict(symbol="triangle-up", size=13, color=BUY_COLOR,
                        line=dict(width=1, color=BG_COLOR)),
            customdata=buys[["quantity", "value_eur"]].values,
            hovertemplate=(
                "Achat %{x|%d/%m/%Y}<br>Prix : %{y:.2f} €<br>"
                "Qté : %{customdata[0]:.4f}<br>Montant : %{customdata[1]:,.0f} €<extra></extra>"
            ),
        ))

    # Ventes
    sells = trades[trades["kind"] == "Sell"] if not trades.empty else pd.DataFrame()
    if not sells.empty:
        fig.add_trace(go.Scatter(
            x=sells["time_naive"], y=sells["unit_price_eur"],
            name="Vente", mode="markers",
            marker=dict(symbol="triangle-down", size=13, color=SELL_COLOR,
                        line=dict(width=1, color=BG_COLOR)),
            customdata=sells[["quantity", "value_eur"]].values,
            hovertemplate=(
                "Vente %{x|%d/%m/%Y}<br>Prix : %{y:.2f} €<br>"
                "Qté : %{customdata[0]:.4f}<br>Montant : %{customdata[1]:,.0f} €<extra></extra>"
            ),
        ))

    fig.update_layout(
        height=480, hovermode="x unified", margin=MARGIN,
        yaxis=dict(ticksuffix=" €"),
        legend=dict(orientation="h", y=1.08, x=0),
    )
    return fig


def position_evolution_figure(trades: pd.DataFrame) -> go.Figure:
    """Quantité détenue après chaque trade (en escalier) : montre l'accumulation et le trim."""
    fig = go.Figure()
    if trades.empty:
        return fig

    df = trades.sort_values("time_naive").reset_index(drop=True)
    sign = df["kind"].map({"Buy": 1, "Sell": -1}).fillna(0)
    cumulative = (sign * df["quantity"]).cumsum()

    fig.add_trace(go.Scatter(
        x=df["time_naive"], y=cumulative,
        name="Quantité", mode="lines+markers",
        line=dict(color=LINE_COLOR, width=2, shape="hv"),  # escalier (horizontal puis vertical)
        marker=dict(size=6),
        hovertemplate="%{x|%d/%m/%Y}<br>%{y:.4f}<extra>Qté cumulée</extra>",
    ))

    fig.update_layout(
        height=220, hovermode="x unified", margin=MARGIN,
        yaxis=dict(title="Quantité"), showlegend=False,
    )
    return fig


def frequency_figure(trades: pd.DataFrame) -> go.Figure:
    """Nombre de trades par mois pour cet actif (bar chart empilé achats/ventes)."""
    fig = go.Figure()
    if trades.empty:
        return fig

    df = trades.copy()
    df["month"] = df["time_naive"].dt.to_period("M").astype(str)
    monthly = df.groupby("month").agg(
        buys=("kind", lambda x: (x == "Buy").sum()),
        sells=("kind", lambda x: (x == "Sell").sum()),
    ).reset_index()

    fig.add_bar(x=monthly["month"], y=monthly["buys"], name="Achats", marker_color=BUY_COLOR)
    fig.add_bar(x=monthly["month"], y=monthly["sells"], name="Ventes", marker_color=SELL_COLOR)
    fig.update_layout(
        barmode="stack", height=220, margin=MARGIN,
        xaxis=dict(type="category"),
        legend=dict(orientation="h", y=1.1, x=0),
    )
    return fig


def buy_progression_figure(prog: pd.DataFrame) -> go.Figure:
    """Prix des achats successifs reliés : segment vert si repli (prix ↓), rouge si hausse.

    Le 1er achat est en gris (pas de précédent). Chaque achat suivant est coloré :
      - vert (GAIN) si acheté moins cher que le précédent = repli = bon.
      - rouge (LOSS) si acheté plus cher = tu montes = FOMO.

    Le % d'évolution vs précédent est annoté au-dessus de chaque point.
    """
    fig = go.Figure()
    if prog.empty or len(prog) < 2:
        return fig

    # Couleurs des marqueurs : 1er = gris, suivants = vert (repli) ou rouge (hausse)
    marker_colors = ["#888"] + [
        GAIN_COLOR if row["on_dip"] else LOSS_COLOR
        for _, row in prog.iloc[1:].iterrows()
    ]

    # Texte : % d'évolution vs précédent (vide pour le 1er)
    texts = [""] + [
        f"{evo:+.1f}%" if pd.notna(evo) else ""
        for evo in prog["evolution_from_prev_pct"].iloc[1:]
    ]

    fig.add_trace(go.Scatter(
        x=prog["time"], y=prog["price"],
        mode="lines+markers+text",
        line=dict(color="#666", width=1, dash="dot"),
        marker=dict(size=12, color=marker_colors, line=dict(width=1, color=BG_COLOR)),
        text=texts, textposition="top center",
        name="Achats",
        customdata=prog[["quantity", "value_eur", "evolution_from_first_pct"]].values,
        hovertemplate=(
            "Achat %{x|%d/%m/%Y}<br>Prix : %{y:.2f} €<br>"
            "Qté : %{customdata[0]:.4f}<br>"
            "Montant : %{customdata[1]:,.0f} €<br>"
            "vs 1er achat : %{customdata[2]:+.1f}%<extra></extra>"
        ),
    ))

    fig.update_layout(
        height=350, hovermode="x unified", margin=MARGIN,
        yaxis=dict(ticksuffix=" €"),
        showlegend=False,
    )
    return fig


def sell_progression_figure(prog: pd.DataFrame) -> go.Figure:
    """Prix des ventes successives reliées : segment vert si palier (prix ↑), rouge si descente.

    Le 1ère vente est en gris. Chaque vente suivante est colorée :
      - vert (GAIN) si vendue plus cher que la précédente = palier ascendant = bon.
      - rouge (LOSS) si vendue moins cher = tu descends = panic sell.

    Le % d'évolution vs précédent est annoté au-dessus de chaque point.
    """
    fig = go.Figure()
    if prog.empty or len(prog) < 2:
        return fig

    marker_colors = ["#888"] + [
        GAIN_COLOR if row["on_palier"] else LOSS_COLOR
        for _, row in prog.iloc[1:].iterrows()
    ]

    texts = [""] + [
        f"{evo:+.1f}%" if pd.notna(evo) else ""
        for evo in prog["evolution_from_prev_pct"].iloc[1:]
    ]

    fig.add_trace(go.Scatter(
        x=prog["time"], y=prog["price"],
        mode="lines+markers+text",
        line=dict(color="#666", width=1, dash="dot"),
        marker=dict(size=12, color=marker_colors, line=dict(width=1, color=BG_COLOR)),
        text=texts, textposition="top center",
        name="Ventes",
        customdata=prog[["quantity", "value_eur", "evolution_from_first_pct"]].values,
        hovertemplate=(
            "Vente %{x|%d/%m/%Y}<br>Prix : %{y:.2f} €<br>"
            "Qté : %{customdata[0]:.4f}<br>"
            "Montant : %{customdata[1]:,.0f} €<br>"
            "vs 1ère vente : %{customdata[2]:+.1f}%<extra></extra>"
        ),
    ))

    fig.update_layout(
        height=350, hovermode="x unified", margin=MARGIN,
        yaxis=dict(ticksuffix=" €"),
        showlegend=False,
    )
    return fig


# --- Rendu --------------------------------------------------------------------------

def render_strategy_page() -> None:
    st.title("🧭 Stratégie")
    st.caption(
        "Visualise tes achats/ventes d'un actif face à son cours et à la moyenne mobile, "
        "pour évaluer ton timing et détecter l'overtrading."
    )

    data = load_data()
    _, df_all = load_portfolio()

    try:
        history = load_history()
    except ValueError as e:
        st.error(str(e))
        st.stop()

    symbols = eligible_symbols(history, data)
    if not symbols:
        st.info("Aucun actif avec des trades ou un historique de prix.")
        st.stop()

    # --- Choix ---
    with st.container(border=True):
        c1, c2, c3 = st.columns([3, 2, 2])
        symbol = c1.selectbox("Actif", symbols, help="Tous les actifs ayant des trades ou un historique de prix.")
        period = c2.radio("Période", list(PERIODS), index=len(PERIODS) - 1, horizontal=True)
        ma_window = c3.slider(
            "Moyenne mobile (semaines)", min_value=1, max_value=12, value=DEFAULT_MA_WEEKS,
            help="Fenêtre de la moyenne mobile sur les snapshots hebdomadaires."
        )

    prices = asset_price_series(history, symbol)
    all_trades = asset_trades(data, symbol)  # non filtré : l'analyse progression part du 1er achat/vente

    if prices.empty and all_trades.empty:
        st.info(f"Aucune donnée pour {symbol}.")
        st.stop()

    # Filtre par période (sur les trades ET les prix affichés).
    # L'analyse progression utilise `all_trades` (non filtré) pour partir du 1er achat/vente réel.
    trades = all_trades
    days = PERIODS[period]
    if days is not None:
        cutoff_naive = pd.Timestamp.now() - pd.Timedelta(days=days)
        if not trades.empty:
            trades = trades[trades["time_naive"] >= cutoff_naive].reset_index(drop=True)
        if not prices.empty:
            prices = prices[prices["date"] >= cutoff_naive].reset_index(drop=True)

    if prices.empty and trades.empty:
        st.info(f"Aucune donnée sur cette période pour {symbol}.")
        st.stop()

    if prices.empty:
        st.warning(
            f"Aucun historique de prix pour {symbol} sur cette période. "
            "L'analyse de timing (vs moyenne mobile) est désactivée ; seuls les trades sont affichés."
        )

    # --- KPIs ---
    timing = timing_analysis(trades, prices, ma_window)
    freq = trade_frequency_stats(trades)
    flow = net_flow(trades)

    c1, c2, c3, c4, c5 = st.columns(5)
    c1.metric("Trades", f"{freq['n_trades']}", f"{timing['n_buys']} achat / {timing['n_sells']} vente")

    avg_gap = freq["avg_days_between"]
    c2.metric(
        "Écart moyen entre trades",
        "—" if avg_gap is None else f"{avg_gap:.1f} j",
        delta="⚠ overtrading" if freq["overtrading"] else None,
        delta_color="inverse" if freq["overtrading"] else "off",
        help=f"En dessous de {OVERTRADING_THRESHOLD_DAYS} jours : l'écart est trop court (overtrading).",
    )

    buy_timing_pct = timing["buy_timing_pct"]
    c3.metric(
        "Achats sous MA",
        "—" if buy_timing_pct is None else f"{buy_timing_pct:.0f} %",
        help=f"Part des achats faits sous la moyenne mobile de {ma_window} semaines : bon timing d'entrée.",
    )

    sell_timing_pct = timing["sell_timing_pct"]
    c4.metric(
        "Ventes au-dessus MA",
        "—" if sell_timing_pct is None else f"{sell_timing_pct:.0f} %",
        help=f"Part des ventes faites au-dessus de la moyenne mobile de {ma_window} semaines : bon timing de sortie.",
    )

    tpm = freq["trades_per_month"]
    c5.metric("Trades / mois", "—" if tpm is None else f"{tpm:.1f}")

    # --- Flux net ---
    with st.container(border=True):
        f1, f2, f3 = st.columns(3)
        f1.metric("Total acheté", f"{flow['bought']:,.0f} €")
        f2.metric("Total vendu", f"{flow['sold']:,.0f} €")
        f3.metric("Flux net", f"{flow['net']:+,.0f} €", delta_color="off",
                  help="Total acheté - total vendu. Positif = tu as plus investi que retiré.")

    # --- Graphique principal ---
    ma = rolling_average(prices, ma_window) if not prices.empty else None
    st.plotly_chart(strategy_figure(prices, trades, ma), use_container_width=True)
    st.caption(
        "Triangles verts (↑) = achats, triangles rouges (↓) = ventes, positionnés au prix exact du trade. "
        "La moyenne mobile (tirets) sert de référence de trend : un achat sous la MA = bon timing d'entrée, "
        "une vente au-dessus = bon timing de sortie."
    )

    # --- Évolution de la position ---
    if not trades.empty:
        st.plotly_chart(position_evolution_figure(trades), use_container_width=True)
        st.caption("Quantité cumulée après chaque trade (en escalier) : montre l'accumulation et le trim.")

    # --- Fréquence ---
    if not trades.empty:
        st.plotly_chart(frequency_figure(trades), use_container_width=True)
        st.caption("Trades par mois : un pic = suractivité potentielle sur cette période.")

    # --- Avertissements de timing ---
    warned = False
    if freq["overtrading"]:
        warned = True
        st.warning(
            f"Écart moyen entre trades : {avg_gap:.1f} jours (sous le seuil de "
            f"{OVERTRADING_THRESHOLD_DAYS} j). Tu trades cet actif trop fréquemment : "
            "envisage de spacer tes ordres pour réduire les frais et le bruit de décision."
        )

    avg_buy = timing["avg_buy_vs_ma_pct"]
    if avg_buy is not None and avg_buy > TIMING_BIAS_WARN_PCT:
        warned = True
        st.warning(
            f"Tes achats sont en moyenne {avg_buy:+.1f} % au-dessus de la MA : tu tends à acheter "
            "quand le prix est au-dessus de son trend (FOMO / achat trop tardif)."
        )

    avg_sell = timing["avg_sell_vs_ma_pct"]
    if avg_sell is not None and avg_sell < -TIMING_BIAS_WARN_PCT:
        warned = True
        st.warning(
            f"Tes ventes sont en moyenne {avg_sell:+.1f} % sous la MA : tu tends à vendre quand "
            "le prix est sous son trend (panic sell / vente trop tôt)."
        )

    if not warned and freq["n_trades"] > 0 and buy_timing_pct is not None and sell_timing_pct is not None:
        st.success(
            f"Pas de biais de timing détecté : {buy_timing_pct:.0f} % d'achats sous MA, "
            f"{sell_timing_pct:.0f} % de ventes au-dessus. Continue comme ça."
        )

    # --- Renforcement sur repli (achats) ---
    # Utilise `all_trades` (non filtré par période) pour partir du tout 1er achat de l'actif.
    buy_prog = buy_progression_analysis(all_trades)
    buy_sum = progression_summary(buy_prog, "on_dip")

    if buy_sum["n_trades"] >= 2:
        st.subheader("📈 Renforcement sur repli (achats)")
        st.caption(
            "Le 1er achat est la référence. Chaque achat suivant est comparé au précédent : "
            "vert = acheté moins cher (repli = tu abaisses ton PRU), rouge = acheté plus cher (FOMO)."
        )

        b1, b2, b3 = st.columns(3)
        b1.metric(
            "Achats sur repli",
            "—" if buy_sum["good_pct"] is None else f"{buy_sum['good_pct']:.0f} %",
            delta=f"{buy_sum['n_good']}/{buy_sum['n_trades'] - 1}",
            delta_color="off",
            help="Part des achats (hors 1er) faits à un prix inférieur au précédent : renforcement sur repli.",
        )
        b2.metric(
            "Évolution moyenne entre achats",
            "—" if buy_sum["avg_evo_from_prev_pct"] is None else f"{buy_sum['avg_evo_from_prev_pct']:+.1f} %",
            help="Négatif = tu renforces en descendant (bon). Positif = tu achètes en montant (FOMO).",
        )
        b3.metric(
            "Évolution depuis le 1er achat",
            "—" if buy_sum["total_evo_from_first_pct"] is None else f"{buy_sum['total_evo_from_first_pct']:+.1f} %",
            help="Évolution du prix entre ton 1er achat et le dernier : contexte de la stratégie.",
        )

        st.plotly_chart(buy_progression_figure(buy_prog), use_container_width=True)

        # Tableau détaillé des achats
        bp_view = buy_prog.copy()
        bp_view["time"] = pd.to_datetime(bp_view["time"])
        st.dataframe(
            bp_view.rename(columns={
                "time": "Date", "price": "Prix (€)", "quantity": "Quantité",
                "value_eur": "Montant (€)", "evolution_from_prev_pct": "vs précédent (%)",
                "evolution_from_first_pct": "vs 1er achat (%)",
            }),
            hide_index=True, use_container_width=True,
            column_config={
                "Date": st.column_config.DatetimeColumn("Date", format="DD/MM/YYYY"),
                "Prix (€)": st.column_config.NumberColumn("Prix", format="%.2f €"),
                "Quantité": st.column_config.NumberColumn("Quantité", format="%.4f"),
                "Montant (€)": st.column_config.NumberColumn("Montant", format="%.0f €"),
                "vs précédent (%)": st.column_config.NumberColumn(
                    "vs précédent", format="%+.1f %%",
                    help="Négatif (vert) = repli : tu as acheté moins cher. Positif (rouge) = FOMO."),
                "vs 1er achat (%)": st.column_config.NumberColumn(
                    "vs 1er achat", format="%+.1f %%"),
            },
        )

        # Verdict
        if buy_sum["good_pct"] is not None:
            if buy_sum["good_pct"] >= 70:
                st.success(
                    f"{buy_sum['good_pct']:.0f} % de tes achats sont sur repli : tu suis bien ta stratégie "
                    "de renforcement en descendant."
                )
            elif buy_sum["good_pct"] < 40 and buy_sum["avg_evo_from_prev_pct"] is not None \
                    and buy_sum["avg_evo_from_prev_pct"] > 0:
                st.warning(
                    f"Seulement {buy_sum['good_pct']:.0f} % de tes achats sont sur repli, et tu montes en "
                    f"moyenne de {buy_sum['avg_evo_from_prev_pct']:+.1f} % entre achats : tu achètes "
                    "en montant plutôt qu'en repli (FOMO). Envisage d'attendre un repli avant le prochain."
                )

    # --- Vente par palier (ventes) ---
    sell_prog = sell_progression_analysis(all_trades)
    sell_sum = progression_summary(sell_prog, "on_palier")

    if sell_sum["n_trades"] >= 2:
        st.subheader("📉 Vente par palier (ventes)")
        st.caption(
            "La 1ère vente est la référence. Chaque vente suivante est comparée à la précédente : "
            "vert = vendue plus cher (palier ascendant = tu maximises), rouge = vendue moins cher (panic sell)."
        )

        s1, s2, s3 = st.columns(3)
        s1.metric(
            "Ventes sur palier",
            "—" if sell_sum["good_pct"] is None else f"{sell_sum['good_pct']:.0f} %",
            delta=f"{sell_sum['n_good']}/{sell_sum['n_trades'] - 1}",
            delta_color="off",
            help="Part des ventes (hors 1ère) faites à un prix supérieur au précédent : vente par palier ascendant.",
        )
        s2.metric(
            "Évolution moyenne entre ventes",
            "—" if sell_sum["avg_evo_from_prev_pct"] is None else f"{sell_sum['avg_evo_from_prev_pct']:+.1f} %",
            help="Positif = tu vends en montant (bon). Négatif = tu vends en descendant (panic sell).",
        )
        s3.metric(
            "Évolution depuis la 1ère vente",
            "—" if sell_sum["total_evo_from_first_pct"] is None else f"{sell_sum['total_evo_from_first_pct']:+.1f} %",
            help="Évolution du prix entre ta 1ère vente et la dernière : contexte de la stratégie.",
        )

        st.plotly_chart(sell_progression_figure(sell_prog), use_container_width=True)

        # Tableau détaillé des ventes
        sp_view = sell_prog.copy()
        sp_view["time"] = pd.to_datetime(sp_view["time"])
        st.dataframe(
            sp_view.rename(columns={
                "time": "Date", "price": "Prix (€)", "quantity": "Quantité",
                "value_eur": "Montant (€)", "evolution_from_prev_pct": "vs précédent (%)",
                "evolution_from_first_pct": "vs 1ère vente (%)",
            }),
            hide_index=True, use_container_width=True,
            column_config={
                "Date": st.column_config.DatetimeColumn("Date", format="DD/MM/YYYY"),
                "Prix (€)": st.column_config.NumberColumn("Prix", format="%.2f €"),
                "Quantité": st.column_config.NumberColumn("Quantité", format="%.4f"),
                "Montant (€)": st.column_config.NumberColumn("Montant", format="%.0f €"),
                "vs précédent (%)": st.column_config.NumberColumn(
                    "vs précédent", format="%+.1f %%",
                    help="Positif (vert) = palier : tu as vendu plus cher. Négatif (rouge) = panic sell."),
                "vs 1ère vente (%)": st.column_config.NumberColumn(
                    "vs 1ère vente", format="%+.1f %%"),
            },
        )

        # Verdict
        if sell_sum["good_pct"] is not None:
            if sell_sum["good_pct"] >= 70:
                st.success(
                    f"{sell_sum['good_pct']:.0f} % de tes ventes sont sur palier ascendant : tu suis "
                    "bien ta stratégie de vente en montant."
                )
            elif sell_sum["good_pct"] < 40 and sell_sum["avg_evo_from_prev_pct"] is not None \
                    and sell_sum["avg_evo_from_prev_pct"] < 0:
                st.warning(
                    f"Seulement {sell_sum['good_pct']:.0f} % de tes ventes sont sur palier, et tu descends "
                    f"en moyenne de {sell_sum['avg_evo_from_prev_pct']:+.1f} % entre ventes : tu vends "
                    "en descendant plutôt qu'en montant (panic sell). Envisage d'attendre un palier supérieur."
                )

    # --- Tableau des trades ---
    if not trades.empty:
        st.subheader("Détail des trades")
        view = trades[["time", "kind", "quantity", "unit_price_eur", "value_eur", "platform"]].copy()
        view["time"] = view["time"].dt.tz_convert("Europe/Paris")
        st.dataframe(
            view,
            hide_index=True,
            use_container_width=True,
            column_config={
                "time": st.column_config.DatetimeColumn("Date", format="DD/MM/YYYY HH:mm"),
                "kind": "Type",
                "quantity": st.column_config.NumberColumn("Quantité", format="%.4f"),
                "unit_price_eur": st.column_config.NumberColumn("Prix unitaire", format="%.2f €"),
                "value_eur": st.column_config.NumberColumn("Montant", format="%.2f €"),
                "platform": "Plateforme",
            },
        )
