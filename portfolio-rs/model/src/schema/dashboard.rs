//! Structures sérialisées dans dashboard.json (lues par le dashboard Python).
//! Lecture seule côté Python : tout changement de champ ici doit être répercuté
//! dans le dashboard (dossier `dashboard/`).

#[derive(serde::Serialize)]
pub struct DashboardAsset {
    pub symbol: String,
    pub kind: String,
    pub ticker: Option<String>,
    pub quantity: f64,
    pub price_eur: f64,
    pub value_eur: f64,
    pub cost_basis_eur: f64,
    pub pnl_eur: f64,
    pub pnl_pct: f64,
    pub realized_pnl_eur: f64,
    pub realized_cost_eur: f64,
}

#[derive(serde::Serialize)]
pub struct DashboardData {
    pub total_value_eur: f64,
    pub total_cost_basis_eur: f64,
    pub total_pnl_eur: f64,
    pub realized_pnl_eur: f64,
    pub trades: Vec<DashboardTrade>,
    pub trade_frequency: TradeFrequency,
    pub assets: Vec<DashboardAsset>,
    pub correlation_matrices: std::collections::HashMap<String, std::collections::HashMap<String, std::collections::HashMap<String, Option<f64>>>>,
    pub holdings: Vec<HoldingStats>,
    pub closed_sales: Vec<ClosedSale>,

}

#[derive(serde::Serialize)]
pub struct DashboardTrade {
    pub time: String,          // RFC3339 UTC
    pub symbol: String,
    pub kind: String,          // "Buy" | "Sell"
    pub platform: String,
    pub quantity: f64,
    pub unit_price_eur: f64,
    pub value_eur: f64,
}

#[derive(serde::Serialize, Default)]
pub struct PeriodCount {
    pub period: String,        // "YYYY-MM" (mois) ou "YYYY-MM-DD" (dimanche de fin de semaine)
    pub trades: u32,
    pub buys: u32,
    pub sells: u32,
    pub volume_eur: f64,
}

impl PeriodCount {
    pub fn new(period: String) -> Self {
        Self { period, ..Default::default() }
    }
}

#[derive(serde::Serialize, Default)]
pub struct TradeFrequency {
    pub total_trades: u32,
    pub avg_per_month: f64,
    pub avg_per_week: f64,
    pub by_month: Vec<PeriodCount>,
    pub by_week: Vec<PeriodCount>,
}



/// Un lot encore détenu (voir `ledger/holding.rs`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenLotStat {
    pub days: f64,
    pub cost_eur: f64,
    /// Valeur actuelle du lot ; None si le prix de l'actif est inconnu.
    pub value_eur: Option<f64>,
}

/// Durée de détention d'un actif (voir `ledger/holding.rs`). Durées en jours.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HoldingStats {
    pub symbol: String,
    pub kind: String,
    pub first_acquired: Option<String>,
    pub open_quantity: f64,
    pub open_cost_eur: f64,
    pub open_avg_days: Option<f64>,
    pub open_oldest_days: Option<f64>,
    pub open_lots: Vec<OpenLotStat>,
    pub closed_lots: u32,
    pub closed_avg_days: Option<f64>,
    pub closed_min_days: Option<f64>,
    pub closed_max_days: Option<f64>,
    pub closed_incomplete: bool,
}

/// Une vente de titres rapprochée de ses lots FIFO (une ligne par vente).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClosedSale {
    pub symbol: String,
    pub sell_date: String, // "YYYY-MM-DD"
    pub days: Option<f64>,
    pub cost_eur: f64,
    pub proceeds_eur: f64,
    pub pnl_eur: f64,
    pub pnl_pct: Option<f64>,
    pub incomplete: bool,
}
