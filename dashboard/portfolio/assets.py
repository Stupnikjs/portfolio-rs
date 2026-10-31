"""Actifs suivis (tags, thèse, DCA) : un fichier JSON par actif dans data/assets/<TICKER>.json.

Séparé de dashboard.json (pipeline Rust, lecture seule). Le dashboard ne modifie que les tags
et l'ajout automatique des actifs détenus ; les autres champs (thèse, données financières, DCA)
sont préservés tels quels à chaque sauvegarde.
"""
from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path

import pandas as pd
import streamlit as st

from portfolio.config import ASSETS_DIR, LEGACY_ASSETS_PATH
from portfolio.data import yfinance_ticker_for


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
    analysis_path: str = ""
    last_analysis_update: str = ""
    quantite_dca_mensuelle: float = 0.0
    thesis: str = ""
    invalidation_scenario: str = ""
    donnees_financieres: dict = field(default_factory=lambda: empty_block(FINANCIAL_SCHEMA))
    avis_analystes: dict = field(default_factory=lambda: empty_block(ANALYST_SCHEMA))
    tags: list = field(default_factory=list)

    def to_dict(self) -> dict:
        return asdict(self)


# --- Chargement / sauvegarde -------------------------------------------------
# Un actif = un fichier data/assets/<TICKER>.json (le contenu de Asset.to_dict()).
# `data` garde en mémoire une clé technique "_files" (ticker -> fichier d'origine), jamais écrite
# sur disque : un fichier renommé à la main est réécrit sous son nom, pas dupliqué.
# Rien n'est jamais supprimé : un fichier illisible ou ajouté à la main entre-temps n'est pas touché.

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
    return {"assets": assets, "_files": files}


def save_assets_data(data: dict, directory: Path = ASSETS_DIR) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    files: dict = data.setdefault("_files", {})
    for a in data["assets"]:
        f = files.get(a["ticker"]) or _asset_file(a["ticker"], directory)
        _write_if_changed(f, a)
        files[a["ticker"]] = f


# --- Accès aux actifs ---------------------------------------------------------

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


def sync_watchlist_with_portfolio(data: dict, portfolio_df: pd.DataFrame) -> int:
    """Ajoute en watchlist tout actif détenu (df issu de build_assets_df) absent de data/assets/.

    Ne touche jamais aux actifs déjà présents (DCA, quantité, analyse restent tels quels, y compris
    si l'actif a été vendu depuis). Renvoie le nombre d'actifs ajoutés.
    """
    if portfolio_df is None or portfolio_df.empty:
        return 0

    existing_tickers = {a["ticker"] for a in data["assets"]}
    added = 0
    for _, row in portfolio_df.iterrows():
        ticker = yfinance_ticker_for(row)
        if ticker in existing_tickers:
            continue
        add_asset(data, Asset(name=row["symbol"], ticker=ticker, in_dca=False))
        existing_tickers.add(ticker)
        added += 1
    return added


def load_synced_assets(portfolio_df: pd.DataFrame) -> tuple[dict, int]:
    """Charge les actifs suivis, y ajoute les nouveaux actifs détenus et sauvegarde si besoin.

    Renvoie (données, nombre d'actifs ajoutés).
    """
    assets_data = load_assets_data()
    added = sync_watchlist_with_portfolio(assets_data, portfolio_df)
    if added:
        save_assets_data(assets_data)
    return assets_data, added


def attach_tags(df: pd.DataFrame, assets_data: dict) -> pd.DataFrame:
    """Copie de `df` avec une colonne `tags` (liste), recherchée par ticker puis par symbole."""
    tag_map = {a["ticker"]: a.get("tags", []) for a in assets_data["assets"]}

    def tags_for(row: pd.Series) -> list:
        for key in (row.get("ticker"), row.get("symbol")):
            if key in tag_map:
                return tag_map[key]
        return []

    out = df.copy()
    out["tags"] = [tags_for(row) for _, row in out.iterrows()]
    return out
