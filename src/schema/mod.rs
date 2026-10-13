//! Types partagés par tout le crate.
//!
//! - `types`     : plateformes, transactions, actifs (modèle du wallet)
//! - `dashboard` : structures écrites dans dashboard.json
//!
//! Tout est ré-exporté à plat : `crate::schema::Transaction`, `crate::schema::DashboardData`...

pub mod dashboard;
pub mod types;

pub use dashboard::*;
pub use types::*;
