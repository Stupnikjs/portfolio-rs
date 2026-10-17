//! Pipeline portefeuille : parse des exports (Binance, XTB, manuel), calculs
//! (cost basis FIFO, valorisation, historique, plus-values) et export vers le dashboard.
//!
//! Organisation de `src/` : seuls `main.rs`, `lib.rs` et `testutil.rs` sont à la racine ;
//! tout le reste vit dans un dossier par responsabilité :
//!
//! - `schema`  : types partagés (transactions, actifs, structures de dashboard.json) -- crate `model`
//! - `parse`   : lecture des exports (Binance, XTB, saisies manuelles) -- crate `importers`
//! - `store`   : wallet en mémoire (`TxStore`) et sa sérialisation -- crate `model`
//! - `ledger`  : tout ce qui se calcule à partir des transactions (cost basis, positions,
//!               valorisation, historique, trades, plus-values réalisées)
//! - `market`  : prix, tickers, corrélations (réseau + cache)

pub mod ledger;
pub mod market;
pub mod devtools;

// Les modules `schema`, `store` et `parse` vivent dans les crates `model` et `importers` ;
// ils restent accessibles sous les mêmes chemins (`crate::schema`, `core::parse`...).
pub use parse;
pub use model::{schema, store};

#[cfg(test)]
mod testutil;
