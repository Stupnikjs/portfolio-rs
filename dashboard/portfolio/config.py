"""Constantes et chemins partagés par tout le dashboard."""
from __future__ import annotations

from pathlib import Path

# --- Chemins ---
# dashboard/portfolio/ (ce fichier) -> dashboard/ -> racine du projet, où vivent data/ et .env.
# On part de l'emplacement du fichier plutôt que du cwd : ça marche d'où que `streamlit run` soit lancé.
DASHBOARD_DIR = Path(__file__).resolve().parent.parent
ROOT_DIR = DASHBOARD_DIR.parent

DATA_PATH = ROOT_DIR / "data" / "dashboard.json"
HISTORY_PATH = ROOT_DIR / "data" / "history.json"
ASSETS_DIR = ROOT_DIR / "data" / "assets"                # un fichier JSON par actif
LEGACY_ASSETS_PATH = ROOT_DIR / "data" / "assets.json"   # ancien format (tous les actifs dans un fichier)

# --- Positions de poussière ---
# Les positions détenues dont la valeur actuelle est inférieure à ce seuil (EUR) sont masquées
# des tableaux et graphiques. Les totaux (KPIs, historique) les incluent toujours.
MIN_POSITION_VALUE_EUR = 30.0

# --- Discipline ---
WEEKLY_TRADE_QUOTA = 4

# --- Fenêtres de corrélation (labels côté Rust) ---
DEFAULT_WINDOW_ORDER = ["90d", "6m", "1y"]
WINDOW_LABELS = {"90d": "3 mois (90j)", "6m": "6 mois (180j)", "1y": "1 an (365j)"}

# --- Benchmarks toujours affichés dans la corrélation ---
BENCHMARK_TICKERS = {
    "MSCI China": "MCHI",
    "CAC 40": "^FCHI",
    "S&P 500": "^GSPC",
    "Or": "GC=F",
    "Argent": "SI=F",
    "NVIDIA": "NDVA",
    "Indice Dollar": "DX-Y.NYB",
    "NASDAQ": "^NDX",
    "SU.FR": "SU.PA",
}
BENCHMARK_LABELS = set(BENCHMARK_TICKERS.keys())
