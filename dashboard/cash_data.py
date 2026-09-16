"""Chargement/sauvegarde du cashflow personnel (salaire, dépenses fixes et
ponctuelles, soldes réels de compte), persistant dans data/cash.json --
séparé de dashboard.json qui, lui, est en lecture seule (pipeline Rust)."""
from __future__ import annotations

import json
from pathlib import Path

from config import ROOT_DIR

CASH_PATH = ROOT_DIR / "data" / "cash.json"

DEFAULT_CASH_DATA = {
    "salaire_net_mensuel": 0.0,
    "depenses_fixes": [],       # [{"nom": str, "montant": float}]
    "depenses_ponctuelles": [], # [{"nom": str, "montant": float, "date": "YYYY-MM-DD"}]
    "soldes_reels": [],         # [{"mois": "YYYY-MM", "solde": float}] -- saisi à la main chaque mois
}


def load_cash_data(path: Path = CASH_PATH) -> dict:
    if not path.exists():
        return {k: (v.copy() if isinstance(v, list) else v) for k, v in DEFAULT_CASH_DATA.items()}
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    for key, default in DEFAULT_CASH_DATA.items():
        data.setdefault(key, default if not isinstance(default, list) else default.copy())
    return data


def save_cash_data(data: dict, path: Path = CASH_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)