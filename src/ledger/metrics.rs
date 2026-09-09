use std::collections::HashMap;

use crate::schema::{AssetKind};
use crate::market::prices::fetch_daily_closes;
use crate::store::serialize::TxStore;

/// Calcule la corrélation de Pearson entre deux séries temporelles
fn pearson_correlation(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len().min(y.len()) as f64;
    if n < 5.0 {
        return 0.0; // Pas assez de données
    }

    // On découpe les séries à la plus courte longueur
    let x = &x[x.len() - n as usize..];
    let y = &y[y.len() - n as usize..];

    let mean_x: f64 = x.iter().sum::<f64>() / n;
    let mean_y: f64 = y.iter().sum::<f64>() / n;

    let mut num = 0.0;
    let mut den_x = 0.0;
    let mut den_y = 0.0;

    for (xi, yi) in x.iter().zip(y.iter()) {
        let dx = xi - mean_x;
        let dy = yi - mean_y;
        num += dx * dy;
        den_x += dx * dx;
        den_y += dy * dy;
    }

    let denom = den_x.sqrt() * den_y.sqrt();
    if denom == 0.0 {
        return 0.0;
    }
    num / denom
}

/// Calcule les rendements journaliers d'une série de prix
fn daily_returns(prices: &[f64]) -> Vec<f64> {
    prices
        .windows(2)
        .map(|w| (w[1] - w[0]) / w[0])
        .collect()
}

/// Construit la matrice de corrélation pour le dashboard
pub fn compute_correlation_matrix(tx_store: &TxStore, days: i64) -> HashMap<String, HashMap<String, f64>> {
    // 1. On filtre les actifs pertinents (non-cash, valeur > 10€)
    let symbols: Vec<String> = tx_store.assets.keys()
        .filter(|s| tx_store.assets.get(*s).map_or(false, |a| a.kind != AssetKind::Cash))
        .cloned()
        .collect();

    // 2. On fetch les historiques de prix et on calcule les rendements
    let mut returns_map: HashMap<String, Vec<f64>> = HashMap::new();

    for symbol in &symbols {
        let asset = tx_store.assets.get(symbol).unwrap();
        let ticker = asset.identifiers.ticker.as_deref();
        
        let prices = fetch_daily_closes(symbol, asset.kind, ticker, days);
        if prices.len() > 5 {
            returns_map.insert(symbol.clone(), daily_returns(&prices));
        }
    }

    // 3. On calcule la matrice NxN
    let mut matrix: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let valid_symbols: Vec<String> = returns_map.keys().cloned().collect();

    for sym1 in &valid_symbols {
        let mut row: HashMap<String, f64> = HashMap::new();
        let ret1 = returns_map.get(sym1).unwrap();
        
        for sym2 in &valid_symbols {
            if sym1 == sym2 {
                row.insert(sym2.clone(), 1.0); // Corrélation avec soi-même = 1
            } else {
                let ret2 = returns_map.get(sym2).unwrap();
                let corr = pearson_correlation(ret1, ret2);
                row.insert(sym2.clone(), corr);
            }
        }
        matrix.insert(sym1.clone(), row);
    }

    matrix
}