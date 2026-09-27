# Format de thèse d'investissement

Deux formats selon le type d'actif, tous deux en JSON pointés par
`Asset.analysis_path` (ticker/nom déjà connus via l'objet `Asset`, pas
besoin de les dupliquer).

## Actions

```json
{
  "donnees_financieres": {
    "cours_reference": 47.80,
    "capitalisation_md": 69.27,
    "pe_2026": 17.8,
    "pe_2027": 18.8,
    "ev_ca_2026": 3.65,
    "dividende_rendement_pct": null
  },
  "avis_analystes": {
    "nb_analystes": 31,
    "recommandation": "ACHETER",
    "objectif_cours": 62.69,
    "potentiel_pct": 31.15
  },
  "thesis": "La thèse d'investissement en texte libre.",
  "invalidation_scenario": "Ce qui invaliderait la thèse -- à relire à chaque revue."
}
```

`donnees_financieres` est un dict libre : on y met les métriques
pertinentes pour l'action en question, rien de plus.

Voir `analyse-boston-scientific-BSX.json` pour un exemple complet.

## Autres actifs (crypto, ETF...)

Pas de données financières structurées ni d'avis analystes -- juste la
thèse et le scénario d'invalidation :

```json
{
  "thesis": "La thèse d'investissement en texte libre.",
  "invalidation_scenario": "Ce qui invaliderait la thèse -- à relire à chaque revue."
}
```
