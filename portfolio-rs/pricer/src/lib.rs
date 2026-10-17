pub mod cache;
pub mod client;

pub use client::{MarketDataClient, PriceError, normalize_currency_for_fx};

/// Représentation neutre du type d'actif pour le crate market_data.
/// Évite de dépendre de la schema de portfolio_rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarketKind {
    Cash,
    Crypto,
    Stock,
}
