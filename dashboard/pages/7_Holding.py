"""Page Détention : durée de détention par actif (data/dashboard.json -> `holdings`)."""
from portfolio.holding import render_holding_page
from portfolio.theme import apply_theme

apply_theme()
render_holding_page()
