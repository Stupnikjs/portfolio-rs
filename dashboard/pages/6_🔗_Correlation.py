"""Page Corrélation : par actif, groupes automatiques et variation selon la fenêtre."""
from portfolio.correlation import render_correlation_page
from portfolio.theme import apply_theme

apply_theme()
render_correlation_page()
