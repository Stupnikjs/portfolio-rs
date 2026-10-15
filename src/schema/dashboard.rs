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
