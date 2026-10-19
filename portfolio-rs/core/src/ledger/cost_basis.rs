//! Portage de src/ledger/cost_basis.py -- calcul du cost basis (prix de
//! revient) par méthode FIFO, et du P&L réalisé à la vente.
//!
//! ⚠ FIFO = convention d'ANALYSE de ce projet (P&L latent / réalisé). Ce
//! n'est pas la règle fiscale des titres (prix moyen pondéré, PMP) : la
//! déclaration des plus-values du CTO doit passer par un calcul dédié.
//!
//! Le coût/produit de chaque transaction est dérivé de `value_eur` (champ
//! immutable qui fait autorité) plutôt que de `quantity * price` : `price`
//! est optionnel et parfois exprimé dans la devise locale de l'actif
//! plutôt qu'en EUR (cas XTB).
//!
//! Structure du fichier :
//! - types de résultat (`Lot`, `RealizedGain`, `CostBasisResult`) ;
//! - `consume_lots` : brique pure (une file de lots, une quantité) ;
//! - `compute_fifo` : rejoue les transactions et orchestre les briques.

use std::collections::{HashMap, VecDeque};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use crate::schema::{Transaction, TransactionKind};
use crate::store::serialize::TxStore;

const EPSILON: f64 = 1e-12;

// ---------------------------------------------------------------------
// Types de résultat
// ---------------------------------------------------------------------

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
     /// Coût de revient (EUR) des lots déjà vendus : dénominateur du rendement réalisé.
    pub fn total_realized_cost(&self, symbol: Option<&str>) -> f64 {
        self.realized_gains
            .iter()
            .filter(|g| symbol.map_or(true, |s| g.symbol == s))
            .map(|g| g.cost_eur)
            .sum()
    }

    pub fn total_fees(&self, symbol: Option<&str>) -> f64 {
        match symbol {
            Some(s) => *self.fees_eur_by_symbol.get(s).unwrap_or(&0.0),
            None => self.fees_eur_by_symbol.values().sum(),
        }
    }

    /// Cessions de TITRES uniquement (ni crypto, ni cash). Point d'entrée
    /// unique : remplace les filtres `stock_symbols` dupliqués dans
    /// main.rs, history.rs et realized.rs.
    ///
    /// Pourquoi exclure la crypto : les WITHDRAW (transferts entre
    /// wallets) sont vus comme des ventes FIFO et fausseraient le résultat.
    pub fn stock_realized_gains<'a>(&'a self, tx_store: &'a TxStore) -> impl Iterator<Item = &'a RealizedGain> + 'a {
        self.realized_gains.iter().filter(move |g| tx_store.is_stock(&g.symbol))
    }

    /// P&L réalisé total sur les titres.
    pub fn total_realized_pnl_stocks(&self, tx_store: &TxStore) -> f64 {
        self.stock_realized_gains(tx_store).map(|g| g.pnl_eur).sum()
    }
}

// ---------------------------------------------------------------------
// Briques pures
// ---------------------------------------------------------------------

fn unit_value_eur(quantity: f64, value_eur: f64) -> Result<f64> {
    if quantity <= 0.0 {
        return Err(anyhow!("quantity doit être positive, reçu {quantity}"));
    }
    Ok(value_eur / quantity)
}

/// Lot ouvert par un BUY / DEPOSIT.
fn open_lot(tx: &Transaction) -> Result<Lot> {
    Ok(Lot {
        quantity: tx.quantity,
        unit_cost_eur: unit_value_eur(tx.quantity, tx.value_eur)?,
        acquired_at: tx.time,
        external_id: tx.external_id.clone(),
    })
}

/// Ce qu'a coûté la consommation de `quantity` dans une file de lots.
#[derive(Debug)]
struct Consumption {
    /// Tranches réellement prélevées (une par lot touché).
    consumed: Vec<Lot>,
    cost_eur: f64,
    /// Quantité que les lots connus n'ont pas pu couvrir.
    uncovered: f64,
}

/// Prélève `quantity` dans `queue` en commençant par le lot le plus
/// ancien (FIFO). Pure : ne connaît ni les transactions ni les symboles.
fn consume_lots(queue: &mut VecDeque<Lot>, quantity: f64) -> Consumption {
    let mut remaining = quantity;
    let mut consumed = Vec::new();
    let mut cost_eur = 0.0;

    while remaining > EPSILON {
        let Some(lot) = queue.front_mut() else { break };
        let take = lot.quantity.min(remaining);

        cost_eur += take * lot.unit_cost_eur;
        consumed.push(Lot { quantity: take, ..lot.clone() });

        lot.quantity -= take;
        remaining -= take;
        if lot.quantity <= EPSILON {
            queue.pop_front();
        }
    }

    Consumption { consumed, cost_eur, uncovered: remaining.max(0.0) }
}

/// Transforme un SELL / WITHDRAW en plus-value réalisée.
///
/// Si la vente dépasse les lots connus, le reliquat est valorisé à un
/// coût de 0 et le résultat est marqué `incomplete` -- son `pnl_eur` est
/// donc surestimé d'autant (signalé plutôt qu'absorbé silencieusement).
fn realize(tx: &Transaction, queue: &mut VecDeque<Lot>) -> RealizedGain {
    let Consumption { mut consumed, cost_eur, uncovered } = consume_lots(queue, tx.quantity);

    let incomplete = uncovered > EPSILON;
    if incomplete {
        consumed.push(Lot { quantity: uncovered, unit_cost_eur: 0.0, acquired_at: tx.time, external_id: None });
    }

    RealizedGain {
        symbol: tx.asset.symbol.clone(),
        sell_time: tx.time,
        quantity: tx.quantity,
        proceeds_eur: tx.value_eur,
        cost_eur,
        pnl_eur: tx.value_eur - cost_eur,
        external_id: tx.external_id.clone(),
        lots_consumed: consumed,
        incomplete,
    }
}

// ---------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------

/// Rejoue les transactions du wallet dans l'ordre chronologique et
/// applique la méthode FIFO : chaque SELL/WITHDRAW consomme les lots
/// BUY/DEPOSIT les plus anciens en premier.
///
/// `at = None` -- comme pour `holdings_at` -- rejoue toutes les
/// transactions connues.
///
/// Les FEE consomment de la quantité (frais payés dans l'actif) mais ne
/// sont pas rattachés à un lot précis : accumulés séparément par actif
/// (`fees_eur_by_symbol`) -- à soustraire soi-même du P&L pour un
/// résultat net de frais. Un FEE qui dépasse les lots connus est ignoré
/// (edge case qui ne devrait pas arriver en pratique).
pub fn compute_fifo(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> Result<CostBasisResult> {
    let mut lots: HashMap<String, VecDeque<Lot>> = HashMap::new();
    let mut realized: Vec<RealizedGain> = Vec::new();
    let mut fees: HashMap<String, f64> = HashMap::new();

    let mut transactions: Vec<&Transaction> = tx_store.transactions.iter().collect();
    transactions.sort_by_key(|tx| tx.time);

    let in_scope = |tx: &&Transaction| at.map_or(true, |at| tx.time <= at);

    for tx in transactions.into_iter().take_while(in_scope) {
        let queue = lots.entry(tx.asset.symbol.clone()).or_default();

        match tx.kind {
            TransactionKind::Buy | TransactionKind::Deposit => queue.push_back(open_lot(tx)?),
            TransactionKind::Sell | TransactionKind::Withdraw => realized.push(realize(tx, queue)),
            TransactionKind::Fee => {
                consume_lots(queue, tx.quantity);
                *fees.entry(tx.asset.symbol.clone()).or_insert(0.0) += tx.value_eur;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AssetKind;
    use crate::schema::TransactionKind::{Buy, Deposit, Fee, Sell, Withdraw};
    use crate::testutil::{store, tx, utc};

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn lot(quantity: f64, unit_cost_eur: f64) -> Lot {
        Lot { quantity, unit_cost_eur, acquired_at: utc("2025-01-01T00:00:00Z"), external_id: None }
    }

    // --- consume_lots (pure, sans TxStore) ---

    #[test]
    fn consume_lots_takes_the_oldest_lot_first_and_splits_across_lots() {
        let mut queue: VecDeque<Lot> = vec![lot(10.0, 100.0), lot(10.0, 200.0)].into();
        let c = consume_lots(&mut queue, 15.0);

        assert!(approx(c.cost_eur, 10.0 * 100.0 + 5.0 * 200.0));
        assert_eq!(c.consumed.len(), 2);
        assert!(approx(c.uncovered, 0.0));
        assert_eq!(queue.len(), 1);
        assert!(approx(queue[0].quantity, 5.0)); // reste du 2e lot
    }

    #[test]
    fn consume_lots_reports_the_uncovered_quantity() {
        let mut queue: VecDeque<Lot> = vec![lot(5.0, 100.0)].into();
        let c = consume_lots(&mut queue, 8.0);

        assert!(approx(c.cost_eur, 500.0));
        assert!(approx(c.uncovered, 3.0));
        assert!(queue.is_empty());
    }

    #[test]
    fn consume_lots_on_an_empty_queue_covers_nothing() {
        let mut queue: VecDeque<Lot> = VecDeque::new();
        let c = consume_lots(&mut queue, 2.0);

        assert!(c.consumed.is_empty());
        assert!(approx(c.cost_eur, 0.0));
        assert!(approx(c.uncovered, 2.0));
    }

    // --- compute_fifo ---

    #[test]
    fn partial_sale_leaves_the_remainder_open_at_its_original_cost() {
        let s = store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2025-01-01T10:00:00Z", "a1"),
            tx(Sell, "AIR", AssetKind::Stock, 4.0, 600.0, "2025-02-01T10:00:00Z", "a2"),
        ]);
        let r = compute_fifo(&s, None).unwrap();

        assert_eq!(r.realized_gains.len(), 1);
        assert!(approx(r.realized_gains[0].cost_eur, 400.0));
        assert!(approx(r.realized_gains[0].pnl_eur, 200.0));
        assert!(approx(r.open_quantity("AIR"), 6.0));
        assert!(approx(r.open_cost_basis("AIR"), 600.0));
        assert!(approx(r.average_cost("AIR").unwrap(), 100.0));
    }

    #[test]
    fn a_sale_beyond_known_lots_is_flagged_incomplete_with_zero_cost_remainder() {
        let s = store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 5.0, 500.0, "2025-01-01T10:00:00Z", "a1"),
            tx(Sell, "AIR", AssetKind::Stock, 10.0, 1_200.0, "2025-02-01T10:00:00Z", "a2"),
        ]);
        let g = &compute_fifo(&s, None).unwrap().realized_gains[0];

        assert!(g.incomplete);
        assert!(approx(g.pnl_eur, 700.0)); // 1200 - 500
        assert_eq!(g.lots_consumed.len(), 2); // lot réel + reliquat à coût 0
    }

    #[test]
    fn withdraw_is_treated_like_a_sale() {
        let s = store(vec![
            tx(Deposit, "BTC", AssetKind::Crypto, 1.0, 10_000.0, "2025-01-01T10:00:00Z", "d1"),
            tx(Withdraw, "BTC", AssetKind::Crypto, 1.0, 12_000.0, "2025-02-01T10:00:00Z", "w1"),
        ]);
        let r = compute_fifo(&s, None).unwrap();

        assert!(approx(r.total_realized_pnl(Some("BTC")), 2_000.0));
        assert!(r.open_lots.is_empty());
    }

    #[test]
    fn fee_consumes_quantity_and_accumulates_fees_without_realizing_a_gain() {
        let s = store(vec![
            tx(Buy, "BNB", AssetKind::Crypto, 10.0, 1_000.0, "2025-01-01T10:00:00Z", "b1"),
            tx(Fee, "BNB", AssetKind::Crypto, 1.0, 2.0, "2025-01-02T10:00:00Z", "f1"),
        ]);
        let r = compute_fifo(&s, None).unwrap();

        assert!(r.realized_gains.is_empty());
        assert!(approx(r.open_quantity("BNB"), 9.0));
        assert!(approx(r.total_fees(Some("BNB")), 2.0));
        assert!(approx(r.total_fees(None), 2.0));
    }

    #[test]
    fn at_excludes_later_transactions() {
        let s = store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2025-01-01T10:00:00Z", "a1"),
            tx(Sell, "AIR", AssetKind::Stock, 10.0, 1_500.0, "2025-03-01T10:00:00Z", "a2"),
        ]);
        let r = compute_fifo(&s, Some(utc("2025-02-01T00:00:00Z"))).unwrap();

        assert!(r.realized_gains.is_empty());
        assert!(approx(r.open_quantity("AIR"), 10.0));
    }

    #[test]
    fn transactions_are_replayed_chronologically_whatever_their_insertion_order() {
        // La vente est insérée AVANT l'achat : le tri chronologique doit la rapprocher du bon lot.
        let s = store(vec![
            tx(Sell, "AIR", AssetKind::Stock, 5.0, 700.0, "2025-02-01T10:00:00Z", "a2"),
            tx(Buy, "AIR", AssetKind::Stock, 5.0, 500.0, "2025-01-01T10:00:00Z", "a1"),
        ]);
        let g = &compute_fifo(&s, None).unwrap().realized_gains[0];

        assert!(!g.incomplete);
        assert!(approx(g.pnl_eur, 200.0));
    }

    #[test]
    fn a_buy_with_a_non_positive_quantity_is_an_error() {
        let s = store(vec![tx(Buy, "AIR", AssetKind::Stock, 0.0, 10.0, "2025-01-01T10:00:00Z", "a1")]);
        assert!(compute_fifo(&s, None).is_err());
    }

    // --- filtres titres ---

    #[test]
    fn stock_helpers_ignore_crypto_sales() {
        let s = store(vec![
            tx(Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2025-01-01T10:00:00Z", "a1"),
            tx(Sell, "AIR", AssetKind::Stock, 10.0, 1_200.0, "2025-02-01T10:00:00Z", "a2"), // +200
            tx(Buy, "BTC", AssetKind::Crypto, 1.0, 20_000.0, "2025-01-01T10:00:00Z", "b1"),
            tx(Sell, "BTC", AssetKind::Crypto, 1.0, 50_000.0, "2025-02-01T10:00:00Z", "b2"), // +30000
        ]);
        let r = compute_fifo(&s, None).unwrap();

        assert_eq!(r.stock_realized_gains(&s).count(), 1);
        assert!(approx(r.total_realized_pnl_stocks(&s), 200.0));
        assert!(approx(r.total_realized_pnl(None), 30_200.0));
    }
}
