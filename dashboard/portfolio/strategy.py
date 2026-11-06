"""Page Stratégie : analyse du timing des achats/ventes d'un actif face à son cours.

Croise trois sources :
  - cours quotidiens en EUR (yfinance), INDÉPENDANTS de la détention et avec une période
    d'échauffement avant le 1er trade : MA, σ et pic sont déjà « chauds » à l'entrée.
  - history.json : prix hebdomadaires, utilisés en REPLI seulement (ils n'existent que pendant
    la détention : peu fiables autour des premières entrées).
  - dashboard.json -> trades : achats/ventes avec prix exact et timestamp.

Toutes les références (MA, σ, pic) sont CAUSALES : un trade du jour J est comparé à la clôture
de J-1, sans interpolation vers une observation postérieure au trade.

But : visualiser si tu achètes haut et vends bas (ou l'inverse), et détecter l'overtrading
(trades trop rapprochés). La moyenne mobile sert de référence de trend : un achat sous la MA
= bon timing d'entrée, une vente au-dessus = bon timing de sortie.
"""
from __future__ import annotations

import numpy as np
import pandas as pd
import plotly.graph_objects as go
import streamlit as st

from portfolio.data import load_data, load_portfolio, yfinance_ticker_for
from portfolio.history import PERIODS, load_history
from portfolio.theme import BG_COLOR, GAIN_COLOR, LOSS_COLOR, PASTEL_DARK_SEQUENCE

LINE_COLOR = PASTEL_DARK_SEQUENCE[0]   # bleu : courbe de prix
MA_COLOR = PASTEL_DARK_SEQUENCE[4]     # violet : moyenne mobile
BUY_COLOR = GAIN_COLOR                 # vert : achat
SELL_COLOR = LOSS_COLOR                # rouge : vente
MARGIN = dict(l=10, r=10, t=30, b=10)

# Fenêtre par défaut de la moyenne mobile (en semaines calendaires).
DEFAULT_MA_WEEKS = 4  # ~1 mois
# En dessous de ce seuil (jours moyens entre trades), on alerte sur l'overtrading.
OVERTRADING_THRESHOLD_DAYS = 7
# Seuils d'alerte sur le biais de timing (% vs MA).
TIMING_BIAS_WARN_PCT = 5.0
# Échauffement ajouté avant le 1er trade (>= plus grande fenêtre des curseurs : 26 semaines).
WARMUP_WEEKS = 26


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


# --- Cours indépendants de la détention --------------------------------------------

_EMPTY_PRICES = pd.DataFrame(columns=["date", "price_eur"])


def _close_series(raw: pd.DataFrame) -> pd.Series:
    """Colonne Close d'un téléchargement yfinance (MultiIndex ou non), index tz-naïf au jour."""
    if raw is None or raw.empty or "Close" not in raw:
        return pd.Series(dtype=float)
    close = raw["Close"]
    if isinstance(close, pd.DataFrame):
        close = close.iloc[:, 0]
    close = close.dropna().astype(float)
    close.index = pd.DatetimeIndex(close.index).tz_localize(None).normalize()
    return close[~close.index.duplicated(keep="last")]


@st.cache_data(ttl=6 * 3600, show_spinner=False)
def fetch_daily_prices_eur(ticker: str, start: str) -> pd.DataFrame:
    """Clôtures quotidiennes converties en EUR (colonnes date, price_eur).

    Cours BRUTS (auto_adjust=False) : tes trades sont à des prix réels, pas ajustés des dividendes.
    Renvoie un DataFrame vide si yfinance est absent, si le ticker est introuvable ou si la
    conversion en EUR est impossible (le repli sur history.json prend alors le relais).
    """
    try:
        import yfinance as yf
    except ImportError:
        return _EMPTY_PRICES.copy()
    try:
        close = _close_series(yf.download(ticker, start=start, interval="1d", auto_adjust=False, progress=False))
        if close.empty:
            return _EMPTY_PRICES.copy()
        currency = str(yf.Ticker(ticker).fast_info["currency"])
        if currency == "GBp":  # pence
            close, currency = close * 0.01, "GBP"
        if currency.upper() != "EUR":
            fx = _close_series(yf.download(f"EUR{currency.upper()}=X", start=start, interval="1d",
                                           auto_adjust=False, progress=False))
            if fx.empty:
                return _EMPTY_PRICES.copy()
            fx = fx.reindex(close.index.union(fx.index)).ffill().reindex(close.index)
            close = (close / fx).dropna()
        close = close[close > 0]
        return pd.DataFrame({"date": close.index, "price_eur": close.values})
    except Exception:
        return _EMPTY_PRICES.copy()


def ticker_for_symbol(df_all: pd.DataFrame, symbol: str) -> str | None:
    """Ticker Yahoo d'un symbole du portefeuille (positions soldées comprises), sinon None."""
    rows = df_all[df_all["symbol"] == symbol]
    return None if rows.empty else yfinance_ticker_for(rows.iloc[0])


def full_price_series(
    history: pd.DataFrame | None, symbol: str, ticker: str | None, trades: pd.DataFrame,
    warmup_weeks: int = WARMUP_WEEKS,
) -> tuple[pd.DataFrame, str]:
    """(cours, source) avec source = "daily" (cours complets) ou "held_only" (repli history.json)."""
    held = asset_price_series(history, symbol)
    firsts = []
    if not trades.empty:
        firsts.append(trades["time_naive"].min())
    if not held.empty:
        firsts.append(held["date"].min())
    if ticker and firsts:
        start = (min(firsts) - pd.Timedelta(weeks=warmup_weeks)).strftime("%Y-%m-%d")
        daily = fetch_daily_prices_eur(ticker, start)
        if len(daily) >= 10:
            return daily, "daily"
    return held, "held_only"


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

def _window(weeks: int) -> str:
    """Fenêtre temporelle pandas en semaines calendaires (valable pour du quotidien comme de l'hebdo)."""
    return f"{7 * weeks}D"


def rolling_average(prices: pd.DataFrame, window: int = DEFAULT_MA_WEEKS) -> pd.Series:
    """Moyenne mobile sur `window` semaines calendaires. Index = dates des prix."""
    if prices.empty or window < 1:
        return pd.Series(dtype=float)
    s = prices.set_index("date")["price_eur"].astype(float)
    return s.rolling(_window(window), min_periods=min(3, window)).mean()


def value_before(series: pd.Series, times) -> pd.Series:
    """Dernière valeur de `series` datée STRICTEMENT avant le jour de chaque instant de `times`.

    Pas d'interpolation : interpoler entre deux observations ferait entrer dans la référence le
    cours d'APRÈS le trade (biais d'anticipation). Un trade du jour J est comparé à la clôture de J-1.
    NaN si aucune observation ne précède le trade ou si la valeur n'est pas encore définie.
    Résultat indexé comme `times`.
    """
    times = pd.DatetimeIndex(times)
    series = series.sort_index()
    if series.empty or len(times) == 0:
        return pd.Series(np.nan, index=times)
    obs = series.index.values.astype("datetime64[ns]")
    days = times.normalize().values.astype("datetime64[ns]")
    pos = np.searchsorted(obs, days, side="left") - 1
    vals = series.to_numpy(dtype=float)
    out = np.where(pos >= 0, vals[np.clip(pos, 0, None)], np.nan)
    return pd.Series(out, index=times)


def ma_at_trades(ma: pd.Series, trades: pd.DataFrame) -> pd.Series:
    """Pour chaque trade, la MA à la clôture précédente (causale, sans interpolation).

    Indexé comme `trades`. NaN avant la première valeur de MA.
    """
    if ma.empty or trades.empty:
        return pd.Series(dtype=float)
    ref = value_before(ma, trades["time_naive"])
    return pd.Series(ref.values, index=trades.index)


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


# --- Volatilité et suffisance du repli -------------------------------------------

# Fenêtre par défaut pour le pic récent et la volatilité (en semaines calendaires).
DEFAULT_DIP_WINDOW = 8  # ~2 mois
# Seuils du score de suffisance (|drawdown| / (σ × √n), voir dip_indicators).
DIP_SCORE_DEEP = 2.0       # >= 2σ : repli très profond
DIP_SCORE_SIGNIFICANT = 1.0  # >= 1σ : repli significatif
DIP_SCORE_MODERATE = 0.5    # >= 0.5σ : repli modéré
# En dessous de 0.5σ : superficiel (trop tôt).


def dip_indicators(prices: pd.DataFrame, window_weeks: int = DEFAULT_DIP_WINDOW) -> pd.DataFrame:
    """Par date : drawdown depuis le pic de la fenêtre, σ par pas, pas écoulés depuis le pic, score.

    Score = |drawdown| / (σ × √n), n = nombre d'observations écoulées depuis le pic.
    Après n pas, l'amplitude « normale » d'une baisse croît en √n : comparer un drawdown de
    plusieurs semaines à σ d'un seul pas gonflait le score. Au sommet (n = 0) le score vaut 0.
    σ = écart-type des rendements d'un pas (jour avec des cours quotidiens, semaine en repli).
    """
    cols = ["drawdown_pct", "volatility_pct", "bars_since_peak", "dip_score"]
    if prices.empty:
        return pd.DataFrame(columns=cols, dtype=float)
    s = prices.set_index("date")["price_eur"].astype(float)
    win = _window(window_weeks)
    peak = s.rolling(win, min_periods=1).max()
    # a[::-1].argmax() = nombre d'observations depuis le DERNIER maximum de la fenêtre.
    bars = s.rolling(win, min_periods=1).apply(lambda a: float(np.argmax(a[::-1])), raw=True)
    vol = s.pct_change().rolling(win, min_periods=5).std() * 100
    dd = (s / peak - 1) * 100
    score = (dd.abs() / (vol * np.sqrt(bars))).where((bars > 0) & (vol > 0))
    score = score.mask((bars == 0) & (vol > 0), 0.0)
    return pd.DataFrame({"drawdown_pct": dd, "volatility_pct": vol, "bars_since_peak": bars, "dip_score": score})


def dip_sufficiency_analysis(
    buys: pd.DataFrame, prices: pd.DataFrame, window: int = DEFAULT_DIP_WINDOW,
) -> pd.DataFrame:
    """Pour chaque achat : drawdown depuis pic récent, volatilité, et score de suffisance.

    Les indicateurs sont ceux de la clôture PRÉCÉDENTE (causal). Score (voir `dip_indicators`) :
      >= 2.0 : repli très profond ; 1.0–2.0 : significatif ; 0.5–1.0 : modéré ;
      < 0.5 : superficiel ; achat alors que la veille était un sommet de fenêtre : « Au sommet ».

    Colonnes ajoutées : price, drawdown_pct, volatility_pct, dip_score, dip_verdict.
    """
    nan_cols = dict(price=np.nan, drawdown_pct=np.nan, volatility_pct=np.nan, dip_score=np.nan, dip_verdict="—")
    if buys.empty or len(prices) < 6:
        return buys.assign(**nan_cols)

    ind = dip_indicators(prices, window)
    buy_times = pd.DatetimeIndex(buys["time_naive"])

    result = buys.copy()
    # Alias `price` = `unit_price_eur` : cohérence avec buy_progression_analysis et les figures.
    result["price"] = result["unit_price_eur"]
    for col in ("drawdown_pct", "volatility_pct", "dip_score"):
        result[col] = value_before(ind[col], buy_times).values

    def _verdict(score: float, drawdown: float) -> str:
        if pd.isna(score):
            return "—"
        if score >= DIP_SCORE_DEEP:
            return "Très profond"
        if score >= DIP_SCORE_SIGNIFICANT:
            return "Significatif"
        if score >= DIP_SCORE_MODERATE:
            return "Modéré"
        return "Au sommet" if drawdown == 0 else "Superficiel"

    result["dip_verdict"] = [_verdict(sc, dd) for sc, dd in zip(result["dip_score"], result["drawdown_pct"])]
    return result


def dip_sufficiency_summary(dip_df: pd.DataFrame) -> dict:
    """Résumé de la suffisance des replis sur tous les achats."""
    out = {
        "avg_volatility_pct": None, "avg_drawdown_pct": None,
        "avg_dip_score": None, "pct_sufficient": None, "n_sufficient": 0,
    }
    valid = dip_df["dip_score"].dropna()
    if valid.empty:
        return out
    out["avg_volatility_pct"] = float(dip_df["volatility_pct"].dropna().mean())
    out["avg_drawdown_pct"] = float(dip_df["drawdown_pct"].dropna().mean())
    out["avg_dip_score"] = float(valid.mean())
    out["n_sufficient"] = int((valid >= DIP_SCORE_SIGNIFICANT).sum())
    out["pct_sufficient"] = out["n_sufficient"] / len(valid) * 100
    return out


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
            name="Cours", mode="lines",
            line=dict(color=LINE_COLOR, width=2),
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


def volatility_figure(prices: pd.DataFrame, dip_df: pd.DataFrame, ma_window: int) -> go.Figure:
    """Bandes de Bollinger (MA ± 2σ des cours) + achats colorés par suffisance du repli.

    La bande est centrée sur la MA et mesurée sur les COURS ; la couleur des achats vient du
    score de repli (drawdown depuis le pic récent, normalisé en σ √n). Ce sont deux références
    distinctes : un achat sous la bande n'a pas forcément un score ≈ 2.
    Couleurs : vert foncé = très profond, vert = significatif, orange = modéré,
    rouge = superficiel ou au sommet.
    """
    fig = go.Figure()

    if prices.empty:
        return fig

    s = prices.set_index("date")["price_eur"].astype(float)
    roll = s.rolling(_window(ma_window), min_periods=min(3, ma_window))
    ma, sd = roll.mean(), roll.std()
    upper, lower = ma + 2 * sd, ma - 2 * sd

    fig.add_trace(go.Scatter(
        x=upper.index, y=upper.values,
        name="MA + 2σ", mode="lines",
        line=dict(color="#444", width=0),
        hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>MA + 2σ</extra>",
    ))
    fig.add_trace(go.Scatter(
        x=lower.index, y=lower.values,
        name="MA − 2σ", mode="lines",
        line=dict(color="#444", width=0),
        fill="tonexty", fillcolor="rgba(100,100,100,0.12)",
        hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>MA − 2σ</extra>",
    ))
    fig.add_trace(go.Scatter(
        x=ma.index, y=ma.values,
        name="MA", mode="lines",
        line=dict(color=MA_COLOR, width=1.5, dash="dash"),
        hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>MA</extra>",
    ))
    fig.add_trace(go.Scatter(
        x=prices["date"], y=prices["price_eur"],
        name="Cours", mode="lines",
        line=dict(color=LINE_COLOR, width=2),
        hovertemplate="%{x|%d/%m/%Y} : %{y:.2f} €<extra>Cours</extra>",
    ))

    if not dip_df.empty:
        verdict_colors = {
            "Très profond": "#1a7a4b",   # vert foncé
            "Significatif": GAIN_COLOR,   # vert
            "Modéré": "#F5B942",          # orange
            "Superficiel": LOSS_COLOR,    # rouge
            "Au sommet": LOSS_COLOR,      # rouge : aucun repli
            "—": "#888",
        }
        colors = [verdict_colors.get(v, "#888") for v in dip_df["dip_verdict"]]
        fig.add_trace(go.Scatter(
            x=dip_df["time_naive"], y=dip_df["price"],
            name="Achats", mode="markers",
            marker=dict(symbol="triangle-up", size=13, color=colors,
                        line=dict(width=1, color=BG_COLOR)),
            customdata=dip_df[["drawdown_pct", "volatility_pct", "dip_score", "dip_verdict"]].values,
            hovertemplate=(
                "Achat %{x|%d/%m/%Y}<br>Prix : %{y:.2f} €<br>"
                "Drawdown : %{customdata[0]:+.1f}%<br>"
                "σ : %{customdata[1]:.1f}%<br>"
                "Score : %{customdata[2]:.2f} (%{customdata[3]})<extra></extra>"
            ),
        ))

    fig.update_layout(
        height=420, hovermode="x unified", margin=MARGIN,
        yaxis=dict(ticksuffix=" €"),
        legend=dict(orientation="h", y=1.08, x=0),
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
        c1, c2, c3, c4 = st.columns([3, 2, 2, 2])
        symbol = c1.selectbox("Actif", symbols, help="Tous les actifs ayant des trades ou un historique de prix.")
        period = c2.radio("Période", list(PERIODS), index=len(PERIODS) - 1, horizontal=True)
        ma_window = c3.slider(
            "MA (semaines)", min_value=1, max_value=12, value=DEFAULT_MA_WEEKS,
            help="Fenêtre de la moyenne mobile, en semaines calendaires."
        )
        dip_window = c4.slider(
            "Volatilité / pic (semaines)", min_value=4, max_value=26, value=DEFAULT_DIP_WINDOW,
            help="Fenêtre pour le calcul de la volatilité (σ) et du drawdown depuis pic récent.",
        )

    # Trades non filtrés : l'analyse progression part du 1er achat/vente réel.
    all_trades = asset_trades(data, symbol)
    # Cours complets (échauffement inclus). Les indicateurs sont TOUJOURS calculés sur toute la série,
    # puis seulement affichés sur la période choisie : changer de période ne change pas les verdicts.
    prices_full, price_source = full_price_series(history, symbol, ticker_for_symbol(df_all, symbol), all_trades)
    prices = prices_full.copy()  # copie d'AFFICHAGE : filtrée par période juste après

    if prices.empty and all_trades.empty:
        st.info(f"Aucune donnée pour {symbol}.")
        st.stop()

    # Filtre par période (sur les trades ET les prix affichés).
    # L'analyse progression utilise `all_trades` (non filtré) pour partir du 1er achat/vente réel.
    trades = all_trades
    days = PERIODS[period]
    cutoff_naive = None
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

    if price_source == "held_only" and not prices_full.empty:
        st.warning(
            "Cours quotidiens indisponibles (yfinance absent, ticker introuvable ou conversion en EUR "
            "impossible) : repli sur history.json, qui ne contient les cours que PENDANT la détention. "
            "MA, σ et drawdown sont peu fiables autour de tes premières entrées."
        )

    # --- KPIs ---
    # Référence calculée sur la série COMPLÈTE (échauffement), pas sur la fenêtre affichée.
    timing = timing_analysis(trades, prices_full, ma_window)
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
    ma = rolling_average(prices_full, ma_window) if not prices_full.empty else None
    if ma is not None and cutoff_naive is not None:
        ma = ma[ma.index >= cutoff_naive]  # affichage seulement : la MA a été calculée sur toute la série
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

    # --- Volatilité et suffisance du repli ---
    # Utilise `prices_full` (non filtré par période) et `all_trades` pour calculer la volatilité
    # et le drawdown depuis le pic récent sur toute la vie de l'actif.
    all_buys = all_trades[all_trades["kind"] == "Buy"].sort_values("time_naive").reset_index(drop=True) if not all_trades.empty else pd.DataFrame()
    if not all_buys.empty and len(prices_full) >= 6:
        dip_df = dip_sufficiency_analysis(all_buys, prices_full, dip_window)
        step = "quotidien" if price_source == "daily" else "hebdomadaire"
        step_unit = "jours" if price_source == "daily" else "semaines"
        dip_sum = dip_sufficiency_summary(dip_df)

        if dip_sum["avg_dip_score"] is not None:
            st.subheader("📉 Volatilité et suffisance du repli")
            st.caption(
                f"σ {step} sur {dip_window} semaines. Score de suffisance = |drawdown depuis le pic| / "
                f"(σ × √n), n = {step_unit} écoulés depuis le pic (une baisse « normale » croît en √n). "
                f"≥ {DIP_SCORE_DEEP} = très profond, ≥ {DIP_SCORE_SIGNIFICANT} = significatif, "
                f"≥ {DIP_SCORE_MODERATE} = modéré, < {DIP_SCORE_MODERATE} = superficiel ; « au sommet » = "
                f"la veille était un plus haut de la fenêtre. Références prises à la clôture précédente."
            )

            d1, d2, d3, d4 = st.columns(4)
            d1.metric(
                f"Volatilité {step} (σ)",
                f"{dip_sum['avg_volatility_pct']:.1f} %",
                help="Écart-type moyen des rendements d'un pas (jour ou semaine) : le bruit normal de l'actif.",
            )
            d2.metric(
                "Drawdown moyen au moment de l'achat",
                f"{dip_sum['avg_drawdown_pct']:+.1f} %",
                help="Baisse moyenne du prix depuis son pic récent quand tu as acheté. "
                     "Négatif = tu as acheté sous le récent sommet.",
            )
            d3.metric(
                "Score de suffisance moyen",
                f"{dip_sum['avg_dip_score']:.2f}",
                help=f"|Drawdown| / (σ × √n). ≥ {DIP_SCORE_SIGNIFICANT} = le repli était significatif "
                     f"relativement à la volatilité de l'actif sur la durée écoulée depuis le pic.",
            )
            d4.metric(
                "Achats avec repli suffisant",
                f"{dip_sum['pct_sufficient']:.0f} %",
                delta=f"{dip_sum['n_sufficient']}/{len(dip_df['dip_score'].dropna())}",
                delta_color="off",
                help=f"Part des achats avec score ≥ {DIP_SCORE_SIGNIFICANT}. Un achat au sommet compte comme insuffisant.",
            )

            # Graphique Bollinger + achats colorés
            st.plotly_chart(
                volatility_figure(prices_full, dip_df, ma_window),
                use_container_width=True,
            )
            st.caption(
                "Bande grise = MA ± 2σ des cours (Bollinger), indépendante du score. Couleurs des "
                "triangles (score de repli depuis le pic) : 🟩 très profond, 🟢 significatif, "
                "🟧 modéré, 🟥 superficiel ou au sommet."
            )

            # Verdict global
            if dip_sum["pct_sufficient"] >= 60:
                st.success(
                    f"{dip_sum['pct_sufficient']:.0f} % de tes achats ont un repli suffisant "
                    f"(≥ 1σ) : tu attends bien des dips significatifs avant de renforcer."
                )
            elif dip_sum["pct_sufficient"] < 40:
                st.warning(
                    f"Seulement {dip_sum['pct_sufficient']:.0f} % de tes achats ont un repli suffisant : "
                    f"tu achètes souvent sur des replis superficiels (score < 1). Envisage d'attendre un "
                    f"repli plus net depuis le pic récent avant de renforcer."
                )

            # Tableau détaillé
            dp_view = dip_df[["time_naive", "price", "drawdown_pct", "volatility_pct", "dip_score", "dip_verdict"]].copy()
            dp_view["time_naive"] = pd.to_datetime(dp_view["time_naive"])
            st.dataframe(
                dp_view.rename(columns={
                    "time_naive": "Date", "price": "Prix (€)",
                    "drawdown_pct": "Drawdown (%)", "volatility_pct": "σ (%)",
                    "dip_score": "Score", "dip_verdict": "Verdict",
                }),
                hide_index=True, use_container_width=True,
                column_config={
                    "Date": st.column_config.DatetimeColumn("Date", format="DD/MM/YYYY"),
                    "Prix (€)": st.column_config.NumberColumn("Prix", format="%.2f €"),
                    "Drawdown (%)": st.column_config.NumberColumn(
                        "Drawdown", format="%+.1f %%",
                        help="Baisse depuis le pic des dernières semaines. Négatif = sous le sommet récent."),
                    "σ (%)": st.column_config.NumberColumn(
                        "σ", format="%.1f %%",
                        help="Volatilité d'un pas (jour ou semaine) : le bruit normal de l'actif."),
                    "Score": st.column_config.NumberColumn(
                        "Score", format="%.2f",
                        help="|Drawdown| / (σ × √n). ≥ 2 = très profond, ≥ 1 = significatif, < 0.5 = superficiel."),
                    "Verdict": "Verdict",
                },
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