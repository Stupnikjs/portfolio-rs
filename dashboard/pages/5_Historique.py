"""Page Historique : valeur totale et cost basis dans le temps (data/history.json)."""
from portfolio.history import render_history_page
from portfolio.theme import apply_theme

apply_theme()
render_history_page()
