"""Performance incluant le cash disponible (XTB). Lecture seule de history.json / dashboard.json."""
from __future__ import annotations

import pandas as pd

COLUMNS = ["date", "stocks", "cash", "contrib", "equity", "gain_eur", "cash_share", "twr_index"]


def equity_frame(history: list[dict]) -> pd.DataFrame:
    """Une ligne par snapshot : titres, cash, apports nets, gain total, indice TWR."""
    rows = []
    for e in history:
        stocks = sum(a["value_eur"] for a in e.get("assets", []) if a["kind"] == "Stock")
        rows.append(
            {
                "date": pd.to_datetime(e["date"]),
                "stocks": stocks,
                "cash": e.get("cash_eur", 0.0),
                "contrib": e.get("net_contributions_eur", 0.0),
            }
        )
    if not rows:
        return pd.DataFrame(columns=COLUMNS)

    df = pd.DataFrame(rows).sort_values("date").reset_index(drop=True)
    df["equity"] = df["stocks"] + df["cash"]
    df["gain_eur"] = df["equity"] - df["contrib"]
    df["cash_share"] = (df["cash"] / df["equity"]).where(df["equity"] > 0, 0.0)
    df["twr_index"] = _twr_index(df)
    return df


def _twr_index(df: pd.DataFrame) -> pd.Series:
    """Valeur de la part (base 1.0) : seuls les flux externes sont neutralisés.

    Hypothèse : un apport arrive en fin de semaine, donc le rendement de la
    semaine t est (equity_t - flux_t) / equity_{t-1} - 1. Approximation correcte
    à pas hebdomadaire, un peu pessimiste si l'apport est investi en début de semaine.
    """
    nav, out = 1.0, []
    for i in range(len(df)):
        if i > 0 and df.at[i - 1, "equity"] > 0:
            flow = df.at[i, "contrib"] - df.at[i - 1, "contrib"]
            nav *= (df.at[i, "equity"] - flow) / df.at[i - 1, "equity"]
        out.append(nav)
    return pd.Series(out, index=df.index)


def current_kpis(dashboard: dict) -> dict:
    """Valeur totale, cash, gain et rendement simple à date (depuis dashboard.json)."""
    cash = dashboard["cash"]["total"]
    equity = dashboard["stocks_value_eur"] + cash["cash_eur"]
    contrib = cash["net_contributions_eur"]
    gain = equity - contrib
    return {
        "equity_eur": equity,
        "cash_eur": cash["cash_eur"],
        "cash_share": cash["cash_eur"] / equity if equity > 0 else 0.0,
        "net_contributions_eur": contrib,
        "gain_eur": gain,
        "return_pct": gain / contrib * 100 if contrib > 0 else None,
    }