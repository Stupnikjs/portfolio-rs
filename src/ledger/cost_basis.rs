//! Portage de src/ledger/cost_basis.py -- calcul du cost basis (prix de
//! revient) par méthode FIFO, et du P&L réalisé à la vente.
//!
//! FIFO plutôt que coût moyen pondéré : méthode retenue par défaut par
//! l'administration fiscale française pour les cessions de valeurs
//! mobilières.
//!
//! Le coût/produit de chaque transaction est dérivé de `value_eur` (champ
//! immutable qui fait autorité) plutôt que de `quantity * price` : `price`
//! est optionnel et parfois exprimé dans la devise locale de l'actif
//! plutôt qu'en EUR (cas XTB).

use std::collections::{HashMap, VecDeque};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use crate::schema::{TransactionKind};
use crate::store::serialize::TxStore;


const EPSILON: f64 = 1e-12;

fn unit_value_eur(quantity: f64, value_eur: f64) -> Result<f64> {
    if quantity <= 0.0 {
        return Err(anyhow!("quantity doit être positive, reçu {quantity}"));
    }
    Ok(value_eur / quantity)
}

/// Un lot FIFO : une tranche de quantité acquise à un prix donné.
#[derive(Debug, Clone)]
pub struct Lot {
    pub quantity: f64,
    pub unit_cost_eur: f64,
    pub acquired_at: DateTime<Utc>,
    pub external_id: Option<String>,
}

/// Résultat d'une vente rapprochée avec un ou plusieurs lots FIFO.
#[derive(Debug, Clone)]
pub struct RealizedGain {
    pub symbol: String,
    pub sell_time: DateTime<Utc>,
    pub quantity: f64,
    pub proceeds_eur: f64,
    pub cost_eur: f64,
    pub pnl_eur: f64,
    pub external_id: Option<String>,
    pub lots_consumed: Vec<Lot>,
    /// True si la vente dépasse les lots connus (transfert entrant non
    /// tracé, données historiques incomplètes...).
    pub incomplete: bool,
}

/// Sortie complète du calcul FIFO pour un wallet.
#[derive(Debug, Default)]
pub struct CostBasisResult {
    pub open_lots: HashMap<String, Vec<Lot>>,
    pub realized_gains: Vec<RealizedGain>,
    pub fees_eur_by_symbol: HashMap<String, f64>,
}

impl CostBasisResult {
    pub fn open_quantity(&self, symbol: &str) -> f64 {
        self.open_lots.get(symbol).map(|lots| lots.iter().map(|l| l.quantity).sum()).unwrap_or(0.0)
    }

    /// Coût de revient total des lots encore ouverts pour `symbol`.
    pub fn open_cost_basis(&self, symbol: &str) -> f64 {
        self.open_lots
            .get(symbol)
            .map(|lots| lots.iter().map(|l| l.quantity * l.unit_cost_eur).sum())
            .unwrap_or(0.0)
    }

    /// Prix de revient moyen par unité sur les lots ouverts (None si rien détenu).
    pub fn average_cost(&self, symbol: &str) -> Option<f64> {
        let qty = self.open_quantity(symbol);
        if qty <= 0.0 {
            return None;
        }
        Some(self.open_cost_basis(symbol) / qty)
    }

    pub fn total_realized_pnl(&self, symbol: Option<&str>) -> f64 {
        self.realized_gains
            .iter()
            .filter(|g| symbol.map_or(true, |s| g.symbol == s))
            .map(|g| g.pnl_eur)
            .sum()
    }

    pub fn total_fees(&self, symbol: Option<&str>) -> f64 {
        match symbol {
            Some(s) => *self.fees_eur_by_symbol.get(s).unwrap_or(&0.0),
            None => self.fees_eur_by_symbol.values().sum(),
        }
    }
}

/// Rejoue les transactions du wallet dans l'ordre chronologique et
/// applique la méthode FIFO : chaque SELL/WITHDRAW consomme les lots
/// BUY/DEPOSIT les plus anciens en premier.
///
/// `at = None` -- comme pour `holdings_at` -- rejoue toutes les
/// transactions connues.
///
/// Les FEE ne sont pas rattachés à un lot précis : accumulés séparément
/// par actif (`fees_eur_by_symbol`) -- à soustraire soi-même du P&L pour
/// un résultat net de frais.
///
/// Si une vente dépasse la quantité connue en lots ouverts, le reliquat
/// est valorisé à un coût de 0 et le `RealizedGain` correspondant est
/// marqué `incomplete = true` -- son `pnl_eur` est donc surestimé d'autant.
pub fn compute_fifo(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> Result<CostBasisResult> {
    let mut lots: HashMap<String, VecDeque<Lot>> = HashMap::new();
    let mut realized: Vec<RealizedGain> = Vec::new();
    let mut fees: HashMap<String, f64> = HashMap::new();

    let mut transactions: Vec<&crate::schema::Transaction> = tx_store.transactions.iter().collect();
    transactions.sort_by_key(|tx| tx.time);

    for tx in transactions {
        if let Some(at) = at {
            if tx.time > at {
                break;
            }
        }

        let symbol = tx.asset.symbol.clone();

        match tx.kind {
            TransactionKind::Buy | TransactionKind::Deposit => {
                let queue = lots.entry(symbol).or_default();
                queue.push_back(Lot {
                    quantity: tx.quantity,
                    unit_cost_eur: unit_value_eur(tx.quantity, tx.value_eur)?,
                    acquired_at: tx.time,
                    external_id: tx.external_id.clone(),
                });
            }
            TransactionKind::Sell | TransactionKind::Withdraw => {
                let queue = lots.entry(symbol.clone()).or_default();
                let mut remaining = tx.quantity;
                let mut consumed: Vec<Lot> = Vec::new();
                let mut cost_eur = 0.0;
                let mut incomplete = false;

                while remaining > EPSILON {
                    let Some(lot) = queue.front_mut() else { break };
                    let take = lot.quantity.min(remaining);

                    cost_eur += take * lot.unit_cost_eur;
                    consumed.push(Lot {
                        quantity: take,
                        unit_cost_eur: lot.unit_cost_eur,
                        acquired_at: lot.acquired_at,
                        external_id: lot.external_id.clone(),
                    });

                    lot.quantity -= take;
                    remaining -= take;
                    if lot.quantity <= EPSILON {
                        queue.pop_front();
                    }
                }

                if remaining > EPSILON {
                    // Pas de lot connu pour couvrir le reliquat : coût
                    // inconnu, traité comme 0 -- signalé via `incomplete`
                    // plutôt que silencieusement absorbé dans le P&L.
                    consumed.push(Lot {
                        quantity: remaining,
                        unit_cost_eur: 0.0,
                        acquired_at: tx.time,
                        external_id: None,
                    });
                    incomplete = true;
                }

                let proceeds_eur = tx.value_eur;
                realized.push(RealizedGain {
                    symbol,
                    sell_time: tx.time,
                    quantity: tx.quantity,
                    proceeds_eur,
                    cost_eur,
                    pnl_eur: proceeds_eur - cost_eur,
                    external_id: tx.external_id.clone(),
                    lots_consumed: consumed,
                    incomplete,
                });
            }
            TransactionKind::Fee => {
                *fees.entry(symbol).or_insert(0.0) += tx.value_eur;
            }
        }
    }

    let open_lots: HashMap<String, Vec<Lot>> = lots
        .into_iter()
        .filter(|(_, queue)| !queue.is_empty())
        .map(|(symbol, queue)| (symbol, queue.into_iter().collect()))
        .collect();

    Ok(CostBasisResult { open_lots, realized_gains: realized, fees_eur_by_symbol: fees })
}
