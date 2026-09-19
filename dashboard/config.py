"""Constantes et chemins partagés par tout le dashboard."""
from __future__ import annotations

from pathlib import Path


# --- Racine du projet ---
# dashboard/ (ce fichier) est un sous-dossier de la racine, où vivent
# data/ et .env. On part de l'emplacement du fichier plutôt que du cwd
# pour que ça marche peu importe d'où `streamlit run` est lancé.
DASHBOARD_DIR = Path(__file__).resolve().parent
ROOT_DIR = DASHBOARD_DIR.parent



# --- Chemins ---
DATA_PATH = ROOT_DIR / "data" / "dashboard.json"

# --- Fenêtres de corrélation ---
# Mapping label de fenêtre (côté Rust) -> période acceptée par yfinance
WINDOW_TO_YF_PERIOD = {"90d": "3mo", "6m": "6mo", "1y": "1y"}
DEFAULT_WINDOW_ORDER = ["90d", "6m", "1y"]

# --- Benchmarks toujours affichés dans la corrélation ---
BENCHMARK_TICKERS = {
    "MSCI China": "MCHI",
    "CAC 40": "^FCHI",
    "S&P 500": "^GSPC",
    "Or": "GC=F",
    "Argent": "SI=F",
    "NVIDIA": "NDVA",
    "Indice Dollar" :"DX-Y.NYB",
    "NASDAQ": "^NDX",
}
BENCHMARK_LABELS = set(BENCHMARK_TICKERS.keys())
