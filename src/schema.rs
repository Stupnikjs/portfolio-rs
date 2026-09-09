//! Types de base, portage direct de src/schema.py.
//! Equivalent du crate pf-core évoqué dans le Python d'origine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Binance,
    Xtb,
    Manual,
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Binance => "Binance",
            Platform::Xtb => "Xtb",
            Platform::Manual => "Manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransactionKind {
    Buy,
    Sell,
    Fee,
    Deposit,
    Withdraw,
}

impl TransactionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransactionKind::Buy => "Buy",
            TransactionKind::Sell => "Sell",
            TransactionKind::Fee => "Fee",
            TransactionKind::Deposit => "Deposit",
            TransactionKind::Withdraw => "Withdraw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKind {
    Cash,
    Crypto,
    Stock,
}

impl AssetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetKind::Cash => "Cash",
            AssetKind::Crypto => "Crypto",
            AssetKind::Stock => "Stock",
        }
    }
}

/// Identifiants externes optionnels d'un actif (ISIN, ticker...).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetIdentifiers {
    pub isin: Option<String>,
    pub ticker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub symbol: String,
    pub name: String,
    pub kind: AssetKind,
    pub ref_currency: String,
    pub identifiers: AssetIdentifiers,
}

/// Transaction porte directement un `Asset` cloné (pas d'`asset_id`) --
/// la dédup canonique des Asset vit dans TxStore::find_or_create_asset,
/// comme dans le Python d'origine (suppression d'AssetRegistry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub platform: Platform,
    pub account_label: String,
    pub kind: TransactionKind,
    pub asset: Asset,
    pub quantity: f64,
    pub price: Option<f64>,
    /// EUR, immutable : la valeur qui fait autorité (voir cost_basis.rs).
    pub value_eur: f64,
    pub amount: Option<f64>,
    pub quote_currency: Option<String>,
    pub time: DateTime<Utc>,
    pub external_id: Option<String>,
    pub remark: Option<String>,
    pub source_file: String,
}
