//! Pipeline portefeuille : parse des exports (Binance, XTB, manuel), calculs
//! (cost basis FIFO, valorisation, historique, plus-values) et export vers le dashboard.
//!
//! Organisation de `src/` : seuls `main.rs`, `lib.rs` et `testutil.rs` sont à la racine ;
//! tout le reste vit dans un dossier par responsabilité :
//!
//! - `schema`  : types partagés (transactions, actifs, structures de dashboard.json)
//! - `parse`   : lecture des exports (Binance, XTB, saisies manuelles)
//! - `store`   : wallet en mémoire (`TxStore`) et sa sérialisation
//! - `ledger`  : tout ce qui se calcule à partir des transactions (cost basis, positions,
//!               valorisation, historique, trades, plus-values réalisées)
//! - `market`  : prix, tickers, corrélations (réseau + cache)

pub mod ledger;
pub mod market;
pub mod parse;
pub mod schema;
pub mod store;

#[cfg(test)]
mod testutil;
