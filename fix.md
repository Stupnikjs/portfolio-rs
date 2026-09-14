# Patch — Ne plus avaler silencieusement les prix manquants

## Contexte

`historical_price_eur` (dans `prices.rs`) capture les échecs réseau/format
et renvoie `0.0` en cas d'échec, sans distinguer "l'actif vaut vraiment 0 €"
de "le prix n'a pas pu être récupéré". Ce `0.0` silencieux remonte jusqu'à
`portfolio_snapshot_at`, puis est gravé de façon permanente par
`record_weekly_history` (le passé n'est jamais retouché). Résultat :
des semaines de `history.json` valorisées à 0 € pour un ou plusieurs
actifs, jamais recalculées.

## Principe du correctif

Ne pas introduire de nouveau type ni de champ `incomplete` séparé :
utiliser le mécanisme d'erreur déjà présent dans la base de code
(`Result<T, PriceError>` + `?`), déjà utilisé par `compute_fifo`,
`save_wallet`, `parse_*`. On arrête simplement d'avaler l'erreur au
niveau de `historical_price_eur`, et on laisse `?` la propager
naturellement à travers `portfolio_snapshot_at` jusqu'aux deux sites
d'appel (`main.rs` et `history.rs`), qui décident chacun quoi faire
de l'échec.

## Fichiers modifiés

### 1. `market/prices.rs`

**Fenêtre Yahoo élargie** (corrige le cas CNYA.DE, cross-listing peu
liquide qui peut rester plus d'une semaine sans clôture) :

```rust
// avant
let period1 = (target - chrono::Duration::days(7)).timestamp();
// après
let period1 = (target - chrono::Duration::days(21)).timestamp();
```

**`historical_price_eur` devient fallible** — plus de `f64` avec
repli à `0.0`, retourne `Result<f64, PriceError>` :

```rust
pub fn historical_price_eur(
    symbol: &str,
    time: DateTime<Utc>,
    kind: AssetKind,
    ticker: Option<&str>,
) -> Result<f64, PriceError> {
    let symbol = symbol.to_uppercase();

    if kind == AssetKind::Cash {
        match symbol.as_str() {
            "EUR" | "EURI" => return Ok(1.0),
            "USD" | "USDT" | "USDC" | "BUSD" => return Ok(0.92),
            "GBP" => return Ok(1.15),
            _ => {}
        }
    }

    let day_str = time.format("%Y-%m-%d").to_string();

    match kind {
        AssetKind::Stock => {
            let ticker = ticker
                .ok_or_else(|| PriceError::Message(format!("pas de ticker Yahoo pour {symbol}")))?;
            let (mut price, currency) = yahoo_historical_price(ticker, &day_str)?;
            if currency != "EUR" {
                let (fx_currency, price_factor) = normalize_currency_for_fx(&currency);
                price *= price_factor;
                let (fx_price, _) = yahoo_historical_price(&format!("EUR{fx_currency}=X"), &day_str)
                    .map_err(|_| {
                        PriceError::Message(format!(
                            "taux EUR{fx_currency} indisponible au {day_str} (ticker {ticker} OK)"
                        ))
                    })?;
                Ok(price / fx_price)
            } else {
                Ok(price)
            }
        }
        AssetKind::Crypto => get_price_from_binance(&symbol, time),
        AssetKind::Cash => Err(PriceError::Message(format!("devise cash non gérée: {symbol}"))),
    }
}
```

Point corrigé au passage : le message d'erreur de la conversion FX
(`EUR{fx_currency}=X`) est maintenant distinct de celui du fetch du
ticker lui-même — avant, les deux échecs produisaient le même
message `[WARN] Prix Yahoo indisponible pour {ticker}`, ce qui rendait
le diagnostic ambigu.

**Wrapper de compatibilité pour les sites d'appel qui ne doivent pas
bloquer** (typiquement le parsing, qui valorise une transaction au
fil de l'eau et ne doit pas faire échouer tout un import pour un prix
manquant) :

```rust
/// Compatibilité pour les sites d'appel qui ne peuvent pas se permettre
/// de propager une erreur (ex: parsing). Absorbe l'échec en 0.0 comme
/// avant — dette assumée, hors scope de ce fix.
pub fn historical_price_eur_or_zero(
    symbol: &str,
    time: DateTime<Utc>,
    kind: AssetKind,
    ticker: Option<&str>,
) -> f64 {
    historical_price_eur(symbol, time, kind, ticker).unwrap_or(0.0)
}
```

### 2. `parse/binance.rs`

Les 4 appels existants à `historical_price_eur(...)` (prix de base,
prix du fee, jambes sell/buy de `parse_converts`) sont remplacés par
`historical_price_eur_or_zero(...)` pour continuer à compiler sans
changer le comportement du parsing. **Hors scope de ce fix** — reste
une dette silencieuse à ce niveau, à traiter séparément si besoin.

### 3. `ledger/portfolio.rs`

`portfolio_snapshot_at` devient fallible, comme `compute_fifo` l'est
déjà. Pas de champ `incomplete` : la fallibilité est portée par le
`Result` lui-même, pas par une donnée annexe à vérifier.

```rust
pub fn portfolio_snapshot_at(
    tx_store: &TxStore,
    at: Option<DateTime<Utc>>,
) -> Result<PortfolioSnapshot, PriceError> {
    let at = at.unwrap_or_else(Utc::now);
    let holdings = holdings_at(tx_store, Some(at));
    let mut total_value_eur = 0.0;
    let mut details: Vec<AssetSnapshot> = Vec::new();

    for (symbol, quantity) in holdings {
        if quantity.abs() < 1e-12 {
            continue;
        }

        let asset = tx_store.assets.get(&symbol);
        let ticker = asset.and_then(|a| a.identifiers.ticker.clone());
        let kind = asset.map(|a| a.kind).unwrap_or(AssetKind::Crypto);

        let price_eur = historical_price_eur(&symbol, at, kind, ticker.as_deref())?;
        let value_eur = quantity * price_eur;
        total_value_eur += value_eur;

        details.push(AssetSnapshot { symbol, quantity, price_eur, value_eur, kind, ticker });
    }

    details.sort_by(|a, b| b.value_eur.partial_cmp(&a.value_eur).unwrap_or(std::cmp::Ordering::Equal));

    Ok(PortfolioSnapshot {
        date: at.format("%Y-%m-%d").to_string(),
        total_value_eur,
        assets: details,
    })
}
```

### 4. `history.rs`

Une semaine dont le calcul échoue est simplement sautée — elle reste
absente de `history.json` et sera retentée automatiquement au run
suivant, sans changement au mécanisme de backfill incrémental
existant (`already_done`).

```rust
while cursor <= last_complete_week_end {
    let key = cursor.format("%Y-%m-%d").to_string();

    if !already_done.contains(&key) {
        let at = Utc.from_utc_datetime(&cursor.and_hms_opt(23, 59, 59).unwrap());

        match portfolio_snapshot_at(tx_store, Some(at)) {
            Ok(snapshot) => {
                let cost_basis = compute_fifo(tx_store, Some(at))?;
                let total_cost_basis_eur: f64 =
                    snapshot.assets.iter().map(|a| cost_basis.open_cost_basis(&a.symbol)).sum();

                if computed == 0 {
                    println!("=== BACKFILL HISTORIQUE HEBDOMADAIRE ===");
                }
                println!("  {key} : {:.2} EUR", snapshot.total_value_eur);

                history.push(WeeklyHistoryEntry {
                    week_end: key,
                    total_value_eur: snapshot.total_value_eur,
                    total_cost_basis_eur,
                    total_pnl_eur: snapshot.total_value_eur - total_cost_basis_eur,
                });
                computed += 1;
            }
            Err(e) => {
                eprintln!("  [WARN] Semaine {key} non calculée ({e}) -- retentée au prochain run.");
            }
        }
    }

    cursor += Duration::weeks(1);
}
```

### 5. `main.rs`

L'appel devient fallible avec `?` — si un prix manque aujourd'hui,
`dashboard.json` **n'est pas réécrit du tout**, plutôt que réécrit
avec un 0 € caché. Changement de comportement assumé : on préfère un
échec visible (le run s'arrête, l'erreur s'affiche) à une donnée
fausse silencieuse.

```rust
let snapshot = portfolio_snapshot_at(&tx_store, None)?;
```

## Purge de l'historique existant

Les entrées déjà écrites dans `history.json` avec des prix à 0 ne
sont pas corrigées automatiquement par ce patch — `already_done` les
considère comme définitives. Une fonction de réparation ponctuelle
recalcule chaque semaine déjà présente et retire celles qui échouent
avec le code corrigé :

```rust
/// Réparation ponctuelle : recalcule chaque semaine déjà présente dans
/// history.json et retire celles dont le recalcul échoue (prix à 0.0
/// silencieux généré par l'ancien code). À lancer une fois, vérifier la
/// sortie, puis supprimer l'appel.
pub fn repair_weekly_history(tx_store: &TxStore, path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let history: Vec<WeeklyHistoryEntry> = serde_json::from_str(&raw)?;

    let mut kept = Vec::new();
    let mut removed = 0;

    for entry in history {
        let cursor = NaiveDate::parse_from_str(&entry.week_end, "%Y-%m-%d")?;
        let at = Utc.from_utc_datetime(&cursor.and_hms_opt(23, 59, 59).unwrap());

        match portfolio_snapshot_at(tx_store, Some(at)) {
            Ok(_) => kept.push(entry),
            Err(e) => {
                println!("  [PURGE] {} retirée ({e})", entry.week_end);
                removed += 1;
            }
        }
    }

    std::fs::write(path, serde_json::to_string_pretty(&kept)?)?;
    println!("({removed} semaine(s) retirée(s), {} conservée(s))", kept.len());
    Ok(())
}
```

À appeler une fois dans `main()`, juste avant `record_weekly_history`,
puis à retirer une fois le run de réparation confirmé propre :

```rust
history::repair_weekly_history(&tx_store, &PathBuf::from("./data/history.json"))?;
record_weekly_history(&tx_store, &PathBuf::from("./data/history.json"))?;
```

## Ce que ce patch ne couvre pas

- **Taux EUR/USD et EUR/GBP hardcodés** dans la branche `Cash` de
  `historical_price_eur` (`0.92`, `1.15`, constants quelle que soit la
  date). Ne peuvent jamais produire d'erreur, donc jamais détectés par
  ce mécanisme — dette distincte, à traiter séparément.
- **`parse/binance.rs`** continue d'avaler les échecs de prix via
  `historical_price_eur_or_zero` au moment du parsing — comportement
  inchangé, hors scope.

## Arbitrage à valider

Avec ce patch, un seul prix manquant (même sur un actif poussière)
fait échouer tout `portfolio_snapshot_at` — donc plus de
`dashboard.json` généré du tout ce jour-là, au lieu d'un dashboard
partiel avec un 0 € visible. Si un comportement plus tolérant est
souhaité pour l'affichage *courant* (mais toujours strict pour
l'historique), il faudra un traitement différencié entre `main.rs` et
`history.rs` plutôt qu'un `Result` uniforme.
