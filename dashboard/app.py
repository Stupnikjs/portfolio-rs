"""Point d'entrée du dashboard : déclare la navigation. Lancer avec : streamlit run app.py

Nécessite Streamlit >= 1.36 (`st.navigation` / `st.Page`). Avec `st.navigation`, le dossier
`pages/` n'est plus parcouru automatiquement : titres, icônes, groupes et URLs sont définis ICI,
en un seul endroit, quel que soit le nom des fichiers. Pour ajouter une page : un `st.Page(...)`.
"""
from __future__ import annotations

import streamlit as st

from portfolio.theme import apply_theme

apply_theme()  # set_page_config + template Plotly : avant tout autre st.*, une seule fois pour le routeur

PAGES = {
    "Portefeuille": [
        st.Page("pages/Home.py", title="Vue globale", icon="📈", url_path="accueil", default=True),
        st.Page("pages/2 🏷 vue par tag.py", title="Vue par tag", icon="🏷️", url_path="vue-par-tag"),
        st.Page("pages/5_Historique.py", title="Historique", icon="🕒", url_path="historique"),
        st.Page("pages/7_Holding.py", title="Détention", icon="⏳", url_path="detention"),
    ],
    "Analyse": [
        st.Page("pages/3_📊_Activite.py", title="Activité", icon="📊", url_path="activite"),
        st.Page("pages/6_🔗_Correlation.py", title="Corrélation", icon="🔗", url_path="correlation"),
        st.Page("pages/Comparer.py", title="Comparer", icon="⚖️", url_path="comparer"),
    ],
    "Organisation": [
        st.Page("pages/Discipline.py", title="Discipline", icon="🎯", url_path="discipline"),
        st.Page("pages/4_🏷️_Gestion_Tags.py", title="Gestion Tags", icon="⚙️", url_path="gestion-tags"),
    ],
}

st.navigation(PAGES).run()
