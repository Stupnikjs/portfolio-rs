"""Dashboard portefeuille : carte du code.

Flux de données (lecture seule, produites par le pipeline Rust `cargo run`) :
    data/dashboard.json  -> positions, trades, ventes, corrélations, cash
    data/history.json    -> valeur et cost basis par semaine
Persistance propre au dashboard : data/assets/<TICKER>.json (tags, thèse, DCA).

Modules (chacun ne dépend que de ceux de la liste au-dessus) :
    config       constantes et chemins
    fmt          formats d'affichage français (montants, pourcentages), fonctions pures
    theme        palette, template Plotly sombre, apply_theme() (à appeler en 1er sur chaque page)
    data         chargement de dashboard.json, DataFrame des positions, KPIs
    assets       actifs suivis et tags (data/assets/*.json)
    home         page d'accueil : KPIs, allocation (treemap), P&L par actif, tableau des positions
    history      page Historique (logique pure + rendu)
    holding      page Détention (logique pure + rendu)
    correlation  page Corrélation : matrice, groupes (clustering), variation par fenêtre
    compare      page Comparer : un tag face à un actif (base 100, corrélation, bêta)

Navigation : app.py est le routeur (`st.navigation`) ; il déclare titres, icônes, groupes et URLs.
Pages Streamlit (dossier pages/) : soit un script autonome (Accueil, Activité, Tags, Discipline),
soit un appel à `render_<page>_page()` du package (Historique, Détention, Corrélation, Comparer).
Convention : logique pure (DataFrames, figures) séparée du rendu `st.*`, pour pouvoir la tester.
"""
