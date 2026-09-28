"""Modèle Asset : un actif suivi (en DCA actif ou en watchlist), avec son
analyse associée. Persistant dans data/assets/<TICKER>.json (un fichier
JSON par actif) -- séparé de dashboard.json (pipeline Rust, lecture seule).

Le DCA est piloté par une QUANTITÉ (ex. 0.01 ETH/mois, 2 parts/mois) et non
par un montant en euros saisi à la main : le montant mensuel se déduit en
multipliant cette quantité par le prix courant (get_price_eur), recalculé à
chaque affichage.

La thèse d'investissement (texte, scénario d'invalidation, données financières,
avis analystes) est stockée directement dans l'Asset, donc dans son fichier JSON."""
from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass, field
from datetime import date
from pathlib import Path

import streamlit as st
import yfinance as yf

from config import ROOT_DIR

ASSETS_DIR = ROOT_DIR / "data" / "assets"                  # un fichier JSON par actif
LEGACY_ASSETS_PATH = ROOT_DIR / "data" / "assets.json"     # ancien format (tous les actifs dans un fichier)


# --- Schéma FIXE des blocs de données --------------------------------------
# Toutes les clés existent toujours, pour tous les actifs (valeur null si
# inconnue) : le code peut donc les lire sans jamais tester leur présence.
# "num" = nombre ou null, "str" = texte ou null. L'ordre = ordre d'affichage.
# Les années sont dans le nom des clés (2026/2027) : au changement d'exercice,
# on renomme ces clés ICI, une seule fois, et dans les fichiers d'actifs.

FINANCIAL_SCHEMA: dict[str, str] = {
    "date_donnees": "str",                      # "YYYY-MM-DD" (date du cours)
    "devise_cours": "str",                      # "EUR", "USD"...
    "cours_reference": "num",                   # dans devise_cours
    "capitalisation_md": "num",
    "valeur_entreprise_md": "num",
    "flottant_pct": "num",
    "pe_2026": "num",
    "pe_2027": "num",
    "pe_ttm": "num",                            # P/E trailing (12 derniers mois)
    "ev_ebitda_ttm": "num",
    "ev_ebitda_2026": "num",
    "ev_ebitda_2027": "num",
    "fcf_ttm_md": "num",                        # free cash flow, dans devise_cours
    "fcf_2026_md": "num",
    "fcf_2027_md": "num",
    "ev_ca_2026": "num",
    "ev_ca_2027": "num",
    "ca_2026_md": "num",
    "ca_2027_md": "num",
    "resultat_net_2026_md": "num",
    "resultat_net_2027_md": "num",
    "croissance_ca_dernier_trimestre_cer_pct": "num",
    "croissance_bnpa_dernier_trimestre_cer_pct": "num",
    "guidance_croissance_2026": "str",          # texte libre : "5-6% (organique)"
    "endettement_net_2026_md": "num",
    "endettement_net_2027_md": "num",
    "produit_principal": "str",
    "part_produit_principal_ca_pct": "num",
    "croissance_produit_principal_dernier_trimestre_cer_pct": "num",
    "part_usd_ca_pct": "num",
    "dividende_dernier": "num",                 # dans devise_cours
    "dividende_rendement_2026_pct": "num",      # 0.0 si l'actif ne verse rien
    "dividende_rendement_2027_pct": "num",
    "perf_1an_pct": "num",
    "perf_3ans_pct": "num",
    "perf_1an_moyenne_pairs_pct": "num",
    "perf_3ans_moyenne_pairs_pct": "num",
}

ANALYST_SCHEMA: dict[str, str] = {
    "nb_analystes": "num",
    "recommandation": "str",                    # "ACHETER", "ACCUMULER", "CONSERVER"...
    "objectif_cours": "num",
    "potentiel_pct": "num",
}

BLOCK_SCHEMAS = {"donnees_financieres": FINANCIAL_SCHEMA, "avis_analystes": ANALYST_SCHEMA}


def empty_block(schema: dict[str, str]) -> dict:
    return {k: None for k in schema}


def normalize_block(block: dict | None, schema: dict[str, str]) -> dict:
    """Toutes les clés du schéma, dans l'ordre du schéma, null si absentes.
    Les clés hors schéma (à la main) sont gardées à la fin, jamais perdues."""
    block = block or {}
    out = {k: block.get(k) for k in schema}
    out.update({k: v for k, v in block.items() if k not in schema})
    return out


def check_block(name: str, block: dict, schema: dict[str, str]) -> None:
    """Lève ValueError si une clé est hors schéma ou d'un mauvais type."""
    unknown = set(block) - set(schema)
    if unknown:
        raise ValueError(f"{name} : clés hors schéma {sorted(unknown)}")
    for k, v in block.items():
        if v is None:
            continue
        if schema[k] == "str":
            ok = isinstance(v, str)
        else:
            ok = isinstance(v, (int, float)) and not isinstance(v, bool)
        if not ok:
            raise ValueError(f"{name}.{k} : {'texte' if schema[k] == 'str' else 'nombre'} attendu, reçu {v!r}")


@dataclass
class Asset:
    name: str
    ticker: str
    in_dca: bool
    analysis_path: str = ""            # chemin vers la note d'analyse (markdown, etc.)
    last_analysis_update: str = ""     # "YYYY-MM-DD"
    quantite_dca_mensuelle: float = 0.0  # quantité achetée par mois -- n'a de sens que si in_dca=True
    # --- Thèse d'investissement (saisie à la main ou importée) ---
    thesis: str = ""
    invalidation_scenario: str = ""
    donnees_financieres: dict = field(default_factory=lambda: empty_block(FINANCIAL_SCHEMA))
    avis_analystes: dict = field(default_factory=lambda: empty_block(ANALYST_SCHEMA))

    def to_dict(self) -> dict:
        return asdict(self)

    @staticmethod
    def from_dict(d: dict) -> "Asset":
        # Les champs absents (ancien assets.json) prennent leur valeur par défaut.
        return Asset(**{k: d[k] for k in Asset.__dataclass_fields__ if k in d and d[k] is not None})


DEFAULT_ASSETS_DATA = {"assets": []}


# --- Chargement / sauvegarde -------------------------------------------------
# Un actif = un fichier data/assets/<TICKER>.json (le contenu de Asset.to_dict()).
# `data` garde en mémoire deux clés techniques, jamais écrites sur disque :
#   "_files"   : ticker -> fichier d'origine (un fichier renommé à la main
#                est donc réécrit sous son nom, pas dupliqué)
#   "_removed" : tickers retirés, dont le fichier est supprimé à la sauvegarde.
# Rien d'autre n'est jamais supprimé : un fichier illisible ou ajouté à la main
# entre-temps n'est pas touché.

def _asset_file(ticker: str, directory: Path) -> Path:
    return directory / f"{re.sub(r'[^A-Za-z0-9._-]', '_', ticker)}.json"


def _json_files(directory: Path) -> list[Path]:
    return sorted(directory.glob("*.json")) if directory.is_dir() else []


def _write_if_changed(path: Path, content: dict) -> None:
    """N'écrit que si le contenu change (garde l'historique git propre)."""
    text = json.dumps(content, indent=2, ensure_ascii=False) + "\n"
    if path.exists() and path.read_text(encoding="utf-8") == text:
        return
    path.write_text(text, encoding="utf-8")


def _migrate_legacy(directory: Path, legacy_path: Path) -> None:
    """Éclate l'ancien assets.json en un fichier par actif, puis le renomme
    en assets.json.migrated (il n'est plus lu)."""
    with open(legacy_path, "r", encoding="utf-8") as f:
        raw = json.load(f)
    directory.mkdir(parents=True, exist_ok=True)
    for a in raw.get("assets", []):
        if a.get("ticker"):
            _write_if_changed(_asset_file(a["ticker"], directory), a)
    legacy_path.rename(legacy_path.with_suffix(".json.migrated"))


def _normalize_and_warn(a: dict, filename: str) -> None:
    """Complète les blocs au schéma fixe et signale (sans rien supprimer)
    les clés hors schéma ou les valeurs de mauvais type."""
    for name, schema in BLOCK_SCHEMAS.items():
        block = a.get(name) if isinstance(a.get(name), dict) else {}
        unknown = set(block) - set(schema)
        if unknown:
            st.warning(f"{filename} : clés hors schéma dans {name} : {sorted(unknown)}")
        try:
            check_block(name, {k: v for k, v in block.items() if k in schema}, schema)
        except ValueError as e:
            st.warning(f"{filename} : {e}")
        a[name] = normalize_block(block, schema)


def load_assets_data(directory: Path = ASSETS_DIR, legacy_path: Path = LEGACY_ASSETS_PATH) -> dict:
    if not _json_files(directory) and legacy_path.exists():
        _migrate_legacy(directory, legacy_path)

    assets: list[dict] = []
    files: dict[str, Path] = {}
    for p in _json_files(directory):
        try:
            with open(p, "r", encoding="utf-8") as f:
                a = json.load(f)
            ticker = a["ticker"]
        except Exception as e:
            st.warning(f"{p.name} ignoré : {e}")
            continue
        if ticker in files:
            st.warning(f"{p.name} ignoré : le ticker {ticker} existe déjà dans {files[ticker].name}.")
            continue
        if p.name != _asset_file(ticker, directory).name:
            st.warning(f"{p.name} : le nom du fichier doit être le ticker ({_asset_file(ticker, directory).name}).")
        _normalize_and_warn(a, p.name)
        assets.append(a)
        files[ticker] = p
    return {"assets": assets, "_files": files, "_removed": []}


def save_assets_data(data: dict, directory: Path = ASSETS_DIR) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    files: dict = data.setdefault("_files", {})

    for ticker in data.get("_removed", []):
        f = files.pop(ticker, None) or _asset_file(ticker, directory)
        if f.exists():
            f.unlink()
    data["_removed"] = []

    for a in data["assets"]:
        f = files.get(a["ticker"]) or _asset_file(a["ticker"], directory)
        _write_if_changed(f, a)
        files[a["ticker"]] = f


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


THESIS_FIELDS = ("thesis", "invalidation_scenario", "donnees_financieres", "avis_analystes")


def update_thesis(data: dict, ticker: str, payload: dict) -> None:
    """Met à jour la thèse d'un actif à partir d'un dict (saisie manuelle ou
    JSON importé). Seuls les champs présents dans `payload` sont modifiés ;
    pour les deux blocs de données, seules les clés fournies changent (fusion
    clé par clé) et elles doivent appartenir au schéma fixe. last_analysis_update
    passe à la date du jour."""
    unknown = set(payload) - set(THESIS_FIELDS)
    if unknown:
        raise ValueError(f"Champs inconnus : {sorted(unknown)} (attendus : {list(THESIS_FIELDS)})")
    for k in ("thesis", "invalidation_scenario"):
        if k in payload and not isinstance(payload[k], str):
            raise ValueError(f"« {k} » doit être un texte.")

    raw = _find(data, ticker)
    if raw is None:
        raise ValueError(f"{ticker} introuvable.")
    payload = dict(payload)
    for name, schema in BLOCK_SCHEMAS.items():
        if name not in payload:
            continue
        if not isinstance(payload[name], dict):
            raise ValueError(f"« {name} » doit être un objet JSON.")
        check_block(name, payload[name], schema)
        merged = {**normalize_block(raw.get(name), schema), **payload[name]}
        payload[name] = normalize_block(merged, schema)

    update_asset(data, ticker, last_analysis_update=date.today().isoformat(), **payload)


def remove_asset(data: dict, ticker: str) -> None:
    data["assets"] = [a for a in data["assets"] if a["ticker"] != ticker]
    data.setdefault("_removed", []).append(ticker)  # le fichier est supprimé à la prochaine sauvegarde


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

def _yahoo_symbol(ticker: str) -> str:
    """Ticker de l'actif -> symbole Yahoo Finance. Le suffixe ".US" (convention
    courtier : BSX.US) n'existe pas chez Yahoo, où les actions US n'ont pas de
    suffixe : BSX.US -> BSX. Les autres tickers (SAN.PA, ETH-USD...) passent tels quels."""
    return ticker[:-3] if ticker.endswith(".US") else ticker


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
        t = yf.Ticker(_yahoo_symbol(ticker))
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