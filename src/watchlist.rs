//! Layer 2 au-dessus de `DashboardData` (seule source de vérité) : génère
//! un résumé texte "veille" -- symbole, type, tier de concentration,
//! corrélation la plus forte -- SANS aucune donnée de valeur (EUR, P&L,
//! quantité). Pensé pour être collé dans un prompt LLM externe.
//!
//! Ne recalcule rien : consomme les champs déjà produits par
//! `portfolio_snapshot_at` / `compute_fifo` / `compute_correlation_matrices`
//! dans main.rs.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::schema::DashboardData;

const GROUP_THRESHOLD_PCT: f64 = 2.0;
const CORR_WINDOW: &str = "1y";

type CorrelationMatrices = HashMap<String, HashMap<String, HashMap<String, Option<f64>>>>;

fn tier(pct: f64) -> &'static str {
    if pct >= 15.0 {
        "Position majeure"
    } else if pct >= 5.0 {
        "Position moyenne"
    } else {
        "Position mineure"
    }
}

/// Corrélation la plus forte (en valeur absolue) de `symbol` avec un autre
/// actif/benchmark, sur la fenêtre `CORR_WINDOW`. `None` si absent de la
/// matrice ou si aucun coefficient n'a pu être calculé.
fn top_correlation<'a>(symbol: &str, matrices: &'a CorrelationMatrices) -> Option<(&'a str, f64)> {
    let row = matrices.get(CORR_WINDOW)?.get(symbol)?;
    row.iter()
        .filter_map(|(other, coeff)| coeff.map(|c| (other.as_str(), c)))
        .filter(|(other, _)| *other != symbol)
        .max_by(|(_, a), (_, b)| a.abs().partial_cmp(&b.abs()).unwrap_or(std::cmp::Ordering::Equal))
}

/// Écrit le fichier de veille dans `path`. `dashboard` est la même
/// structure que celle sérialisée dans dashboard.json -- pas de recalcul,
/// juste une projection filtrée/anonymisée.
pub fn write_watchlist(dashboard: &DashboardData, path: &Path) -> Result<()> {
    // Cash exclu : pas un "actif" à surveiller. Total recalculé sur ce
    // sous-ensemble plutôt que de réutiliser dashboard.total_value_eur.
    let mut relevant: Vec<_> = dashboard.assets.iter().filter(|a| a.kind != "Cash" && a.value_eur > 0.0).collect();
    relevant.sort_by(|a, b| b.value_eur.partial_cmp(&a.value_eur).unwrap_or(std::cmp::Ordering::Equal));

    let total: f64 = relevant.iter().map(|a| a.value_eur).sum();
    if total <= 0.0 {
        return Ok(());
    }

    let mut out = String::from("Répartition du portefeuille pour veille (aucune donnée de valeur) :\n\n");
    let mut small_count = 0;

    for a in &relevant {
        let pct = a.value_eur / total * 100.0;
        if pct < GROUP_THRESHOLD_PCT {
            small_count += 1;
            continue;
        }

        let ticker_str = a.ticker.as_deref().map(|t| format!(" (ticker: {t})")).unwrap_or_default();
        let mut line = format!("- {}{} [{}] — {}", a.symbol, ticker_str, a.kind, tier(pct));

        if let Some((other, coeff)) = top_correlation(&a.symbol, &dashboard.correlation_matrices) {
            line.push_str(&format!(" — corrélation forte avec {other} ({coeff:+.2})"));
        }
        out.push_str(&line);
        out.push('\n');
    }

    if small_count > 0 {
        out.push_str(&format!("- + {small_count} position(s) mineure(s) (< {GROUP_THRESHOLD_PCT}% chacune)\n"));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out)?;
    Ok(())
}
