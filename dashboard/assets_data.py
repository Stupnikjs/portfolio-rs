"""Modèle Asset : un actif suivi (en DCA actif ou en watchlist), avec son
analyse associée. Persistant dans data/assets.json -- séparé de
dashboard.json (pipeline Rust, lecture seule).

Le DCA est piloté par une QUANTITÉ (ex. 0.01 ETH/mois, 2 parts/mois) et non
par un montant en euros saisi à la main : le montant mensuel se déduit en
multipliant cette quantité par le prix courant (get_price_eur), recalculé à
chaque affichage."""
from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path

import streamlit as st
import yfinance as yf

from config import ROOT_DIR

ASSETS_PATH = ROOT_DIR / "data" / "assets.json"


@dataclass
class Asset:
    name: str
    ticker: str
    in_dca: bool
    analysis_path: str = ""            # chemin vers la note d'analyse (markdown, etc.)
    last_analysis_update: str = ""     # "YYYY-MM-DD"
    quantite_dca_mensuelle: float = 0.0  # quantité achetée par mois -- n'a de sens que si in_dca=True

    def to_dict(self) -> dict:
        return asdict(self)

    @staticmethod
    def from_dict(d: dict) -> "Asset":
        return Asset(**{k: d.get(k, Asset.__dataclass_fields__[k].default) for k in Asset.__dataclass_fields__})


DEFAULT_ASSETS_DATA = {"assets": []}


# --- Chargement / sauvegarde -------------------------------------------------

def load_assets_data(path: Path = ASSETS_PATH) -> dict:
    if not path.exists():
        return {"assets": []}
    with open(path, "r", encoding="utf-8") as f:
        raw = json.load(f)
    raw.setdefault("assets", [])
    return raw


def save_assets_data(data: dict, path: Path = ASSETS_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)


# --- Accès aux objets Asset ---------------------------------------------------

def _assets(data: dict) -> list[Asset]:
    return [Asset.from_dict(a) for a in data["assets"]]


def _find(data: dict, ticker: str) -> dict | None:
    for a in data["assets"]:
        if a["ticker"] == ticker:
            return a
    return None


def add_asset(data: dict, asset: Asset) -> None:
    if _find(data, asset.ticker) is not None:
        raise ValueError(f"{asset.ticker} existe déjà.")
    data["assets"].append(asset.to_dict())


def update_asset(data: dict, ticker: str, **changes) -> None:
    raw = _find(data, ticker)
    if raw is None:
        raise ValueError(f"{ticker} introuvable.")
    allowed = set(Asset.__dataclass_fields__) - {"ticker"}
    unknown = set(changes) - allowed
    if unknown:
        raise ValueError(f"Champs inconnus : {unknown}")
    raw.update(changes)


def remove_asset(data: dict, ticker: str) -> None:
    data["assets"] = [a for a in data["assets"] if a["ticker"] != ticker]


def promote_to_dca(data: dict, ticker: str, quantite_dca_mensuelle: float) -> None:
    """Passe un candidat de la watchlist au panier DCA actif."""
    update_asset(data, ticker, in_dca=True, quantite_dca_mensuelle=quantite_dca_mensuelle)


def demote_to_watchlist(data: dict, ticker: str) -> None:
    """Sort un actif du DCA sans le supprimer -- il redevient un candidat suivi."""
    update_asset(data, ticker, in_dca=False, quantite_dca_mensuelle=0.0)


def get_dca_assets(data: dict) -> list[Asset]:
    return [a for a in _assets(data) if a.in_dca]


def get_watchlist_assets(data: dict) -> list[Asset]:
    return [a for a in _assets(data) if not a.in_dca]


def sync_watchlist_with_portfolio(data: dict, portfolio_df, ticker_resolver=None) -> int:
    """Ajoute automatiquement en watchlist tout actif détenu dans le
    portefeuille (df issu de build_assets_df) mais absent de assets.json.
    Ne touche jamais aux actifs déjà présents -- statut DCA, quantité,
    analyse restent tels quels, y compris si l'actif a été vendu depuis.
    `ticker_resolver` (ex. data.yfinance_ticker_for) convertit le symbole
    du portefeuille en ticker yfinance si les deux diffèrent.
    Renvoie le nombre d'actifs ajoutés."""
    if portfolio_df is None or portfolio_df.empty:
        return 0

    existing_tickers = {a["ticker"] for a in data["assets"]}
    added = 0
    for symbol in portfolio_df["symbol"]:
        ticker = ticker_resolver(symbol) if ticker_resolver else symbol
        if ticker in existing_tickers:
            continue
        add_asset(data, Asset(name=symbol, ticker=ticker, in_dca=False))
        existing_tickers.add(ticker)
        added += 1
    return added


# --- Prix en EUR (yfinance) --------------------------------------------------

@st.cache_data(ttl=3600, show_spinner=False)
def _fx_rate_to_eur(currency: str) -> float | None:
    if currency == "EUR":
        return 1.0
    try:
        hist = yf.Ticker(f"{currency}EUR=X").history(period="5d", interval="1d")
        if not hist.empty:
            return float(hist["Close"].iloc[-1])
    except Exception:
        pass
    return None


@st.cache_data(ttl=900, show_spinner=False)
def get_price_eur(ticker: str) -> float | None:
    """Dernier prix connu, converti en EUR. None si indisponible."""
    try:
        t = yf.Ticker(ticker)
        fast = t.fast_info
        price = fast.get("lastPrice") if hasattr(fast, "get") else getattr(fast, "last_price", None)
        currency = (fast.get("currency") if hasattr(fast, "get") else getattr(fast, "currency", None)) or "USD"
        if price is None:
            hist = t.history(period="5d", interval="1d")
            if hist.empty:
                return None
            price = float(hist["Close"].iloc[-1])
        rate = _fx_rate_to_eur(currency)
        if rate is None:
            return None
        return float(price) * rate
    except Exception:
        return None