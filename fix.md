Voici le récap des deux correctifs à faire :

## 1. Fetch CNYA.DE (prices.rs)

- **Fenêtre trop courte** : `yahoo_historical_price` ne regarde que 7 jours avant la date cible. CNYA.DE (cross-listing peu liquide sur Xetra, libellé en USD) peut rester plus d'une semaine sans clôture → élargir à ~21 jours :
  ```rust
  let period1 = (target - chrono::Duration::days(21)).timestamp();
  ```
- **Warning trompeur** dans `historical_price_eur` : le message `[WARN] Prix Yahoo indisponible pour {ticker}` est identique que ce soit le fetch du ticker lui-même ou celui de la paire FX (`EUR{fx_currency}=X`) qui échoue. Corriger le message dans la branche FX pour dire explicitement que c'est la paire de change qui a échoué, pas le ticker.

## 2. history.json faux (structurel, plus important)

Cause racine : `historical_price_eur` avale les échecs réseau et renvoie `0.0` silencieusement (comportement voulu pour ne pas bloquer le pipeline, mais sans distinguer "vaut 0" de "prix introuvable"). `portfolio_snapshot_at` ne vérifie jamais ce cas, et `record_weekly_history` grave le résultat dans le marbre dès qu'une semaine est écrite (immutabilité voulue du passé).

Résultat : des semaines valorisées à 0 € pour un ou plusieurs actifs, jamais recalculées.

**Correctif en 4 étapes :**
1. `historical_price_eur` → renvoyer `Option<f64>` (ou `Result`) au lieu d'avaler l'erreur en `0.0`.
2. `PortfolioSnapshot` → ajouter un champ `incomplete: bool` (même pattern que `RealizedGain.incomplete` dans cost_basis.rs), mis à `true` si un actif a un prix manquant.
3. `record_weekly_history` → ne jamais ajouter à `history` (ni marquer "done") une semaine dont le snapshot est `incomplete` — elle sera retentée au run suivant.
4. **Purger `history.json`** : les entrées déjà écrites avec des prix à 0 doivent être supprimées à la main, sinon `already_done` continuera de les considérer comme définitives et le fix ne s'appliquera qu'aux semaines futures.

À demain 👋