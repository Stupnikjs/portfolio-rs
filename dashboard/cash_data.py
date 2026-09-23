"""Chargement/sauvegarde du cashflow personnel (salaire, dépenses fixes et
ponctuelles, soldes réels de compte), persistant dans data/cash.json --
séparé de dashboard.json qui, lui, est en lecture seule (pipeline Rust)."""
from __future__ import annotations


from datetime import date

import pandas as pd

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




"""Capacité d'investissement mensuelle.

Pour chaque mois M :

    flux(M)      = salaire net - dépenses fixes - dépenses ponctuelles(M)
    solde_fin(M) = dernier checkpoint réel du mois si il y en a un,
                   sinon solde_fin(M-1) + flux(M)   (projection)
    capacité(M)  = solde_fin(M)  = épargne de départ + flux du mois

Autrement dit : ce qu'il y a sur le compte au départ + ce que le mois
rapporte net. Un checkpoint réel « recale » la chaîne, donc l'erreur des
dépenses approximatives ne se cumule jamais au-delà du prochain checkpoint.

Conventions (identiques à celles de la page Cashflow) :
- le dernier checkpoint d'un mois vaut solde de fin de mois ;
- pas de lissage : une capacité nulle ou négative reste affichée telle quelle ;
- entre deux checkpoints, la projection suppose qu'aucun investissement
  n'est sorti du compte (le checkpoint suivant le reflétera).
"""


COLUMNS = [
    "mois", "epargne_depart", "salaire", "fixes", "ponctuelles",
    "flux", "capacite", "source", "ecart",
]


def _ponctuelles_par_mois(ponctuelles: pd.DataFrame | None) -> pd.Series:
    if ponctuelles is None or ponctuelles.empty:
        return pd.Series(dtype=float)
    df = ponctuelles.dropna(subset=["date"]).copy()
    df["montant"] = pd.to_numeric(df["montant"], errors="coerce").fillna(0.0)
    df["mois"] = pd.to_datetime(df["date"]).dt.to_period("M")
    return df.groupby("mois")["montant"].sum()


def _checkpoints_par_mois(soldes: pd.DataFrame | None) -> pd.Series:
    """Dernier checkpoint de chaque mois (valeur de fin de mois)."""
    if soldes is None or soldes.empty:
        return pd.Series(dtype=float)
    df = soldes.dropna(subset=["date", "solde"]).copy()
    if df.empty:
        return pd.Series(dtype=float)
    df["date"] = pd.to_datetime(df["date"])
    df = df.sort_values("date")
    df["mois"] = df["date"].dt.to_period("M")
    return df.groupby("mois")["solde"].last().astype(float)


def compute_monthly_capacity(
    salaire: float,
    total_fixes: float,
    ponctuelles: pd.DataFrame | None,
    soldes: pd.DataFrame | None,
    horizon_mois: int = 3,
    today: date | None = None,
) -> pd.DataFrame:
    """Une ligne par mois, du premier checkpoint jusqu'à `horizon_mois` mois
    après le mois courant (ou uniquement à partir du mois courant s'il n'y a
    aucun checkpoint).

    Colonnes : mois, epargne_depart, salaire, fixes, ponctuelles, flux,
    capacite, source ("Réel" | "Projeté" | "Flux seul"), ecart.
    `ecart` = solde réel - solde estimé, uniquement pour les mois avec checkpoint
    ayant un mois précédent connu.
    """
    current = pd.Timestamp(today or date.today()).to_period("M")
    ponct = _ponctuelles_par_mois(ponctuelles)
    checkpoints = _checkpoints_par_mois(soldes)

    if checkpoints.empty:
        start, end = current, current + horizon_mois
    else:
        start = checkpoints.index.min()
        end = max(current + horizon_mois, checkpoints.index.max())

    rows = []
    prev_fin: float | None = None  # solde de fin du mois précédent (None = inconnu)

    for m in pd.period_range(start, end, freq="M"):
        depenses_ponct = float(ponct.get(m, 0.0))
        flux = float(salaire) - float(total_fixes) - depenses_ponct

        ecart = None
        if m in checkpoints.index:
            solde_fin = float(checkpoints[m])
            source = "Réel"
            if prev_fin is not None:
                ecart = solde_fin - (prev_fin + flux)
        elif prev_fin is not None:
            solde_fin = prev_fin + flux
            source = "Projeté"
        else:  # aucun checkpoint du tout : on ne connaît que le flux
            solde_fin = None
            source = "Flux seul"

        rows.append({
            "mois": str(m),
            "epargne_depart": prev_fin,
            "salaire": float(salaire),
            "fixes": float(total_fixes),
            "ponctuelles": depenses_ponct,
            "flux": flux,
            "capacite": solde_fin if solde_fin is not None else flux,
            "source": source,
            "ecart": ecart,
        })
        prev_fin = solde_fin

    df = pd.DataFrame(rows, columns=COLUMNS)
    df[["epargne_depart", "ecart"]] = df[["epargne_depart", "ecart"]].astype(float)  # None -> NaN
    return df