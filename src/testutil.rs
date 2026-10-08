//! Helpers partagés par les tests unitaires (compilé uniquement en `cargo test`).

use chrono::{DateTime, Utc};

use crate::schema::{Asset, AssetIdentifiers, AssetKind, Platform, Transaction, TransactionKind};
use crate::store::serialize::TxStore;

pub fn utc(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&Utc)
}

pub fn asset(symbol: &str, kind: AssetKind) -> Asset {
    Asset {
        symbol: symbol.to_string(),
        name: symbol.to_string(),
        kind,
        ref_currency: "EUR".to_string(),
        identifiers: AssetIdentifiers {
            isin: None,
            ticker: (kind == AssetKind::Stock).then(|| format!("{symbol}.PA")),
        },
    }
}

/// `id` doit être unique (dédup par external_id dans TxStore).
pub fn tx(
    kind: TransactionKind,
    symbol: &str,
    asset_kind: AssetKind,
    quantity: f64,
    value_eur: f64,
    time: &str,
    id: &str,
) -> Transaction {
    Transaction {
        platform: Platform::Manual,
        account_label: "test".to_string(),
        kind,
        asset: asset(symbol, asset_kind),
        quantity,
        price: None,
        value_eur,
        amount: None,
        quote_currency: None,
        time: utc(time),
        external_id: Some(id.to_string()),
        remark: None,
        source_file: "test".to_string(),
    }
}

pub fn store(txs: Vec<Transaction>) -> TxStore {
    let mut s = TxStore::new();
    s.add_transactions(txs);
    s
}