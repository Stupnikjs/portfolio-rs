//! Portage de src/ledger/positions.py -- reconstruction des positions
//! (quantités détenues) à partir du log de transactions. Pattern
//! event-sourcing : serialized_tx.json ne stocke jamais de solde calculé,
//! seulement les transactions brutes. `holdings_at` est la seule source
//! de vérité pour "combien j'ai de X à telle date".

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::schema::TransactionKind;
use crate::store::serialize::TxStore;

fn sign(kind: TransactionKind) -> i8 {
    match kind {
        TransactionKind::Buy => 1,
        TransactionKind::Deposit => 1,
        TransactionKind::Sell => -1,
        TransactionKind::Withdraw => -1,
        TransactionKind::Fee => -1,
    }
}

/// Quantité détenue par symbole à la date `at` (incluse). `at = None`
/// signifie "maintenant" au sens de "toutes les transactions connues".
///
/// Les quantités quasi nulles issues d'arrondis flottants (< 1e-12) sont
/// ramenées à zéro pour éviter les faux positifs de type "je détiens
/// encore 0.0000000000003 BTC".
pub fn holdings_at(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> HashMap<String, f64> {
    let mut holdings: HashMap<String, f64> = HashMap::new();

    let mut sorted: Vec<&crate::schema::Transaction> = tx_store.transactions.iter().collect();
    sorted.sort_by_key(|tx| tx.time);

    for tx in sorted {
        if let Some(at) = at {
            if tx.time > at {
                break;
            }
        }

        let symbol = tx.asset.symbol.clone();
        let s = sign(tx.kind) as f64;
        *holdings.entry(symbol).or_insert(0.0) += s * tx.quantity;
    }

    for qty in holdings.values_mut() {
        if qty.abs() < 1e-12 {
            *qty = 0.0;
        }
    }

    holdings
}

/// Comme `holdings_at`, mais filtre les actifs à quantité nulle -- pratique
/// pour n'afficher que ce qui est réellement détenu.
pub fn non_zero_holdings_at(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> HashMap<String, f64> {
    holdings_at(tx_store, at)
        .into_iter()
        .filter(|(_, qty)| *qty != 0.0)
        .collect()
}
