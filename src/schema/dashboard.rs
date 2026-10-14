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
    /// Retour par tranche de cash apportée (aujourd'hui un seul rapport : CTO + PEA regroupés).
    pub tranches: Vec<TrancheReport>,
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

/// Une tranche de cash apportée (voir `ledger/tranches.rs`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Tranche {
    pub date: String,                 // "YYYY-MM-DD" de l'apport
    pub invested_eur: f64,            // montant apporté
    pub nav_at_entry: f64,            // valeur de la part ce jour-là
    pub current_value_eur: f64,       // valeur actuelle des parts restantes
    pub unrealized_gain_eur: f64,
    pub realized_gain_eur: f64,       // gain déjà encaissé via des retraits
    pub roi_pct: f64,                 // retour depuis l'apport
    pub annualized_pct: Option<f64>,  // null si < 30 jours
    pub inferred: bool,               // capital déduit, apports antérieurs non exportés
}

/// Rapport d'un compte : totaux + une ligne par tranche.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrancheReport {
    pub account: String,              // ex. "XTB (CTO + PEA)"
    pub net_contributions_eur: f64,   // apports - retraits
    pub current_value_eur: f64,
    pub total_gain_eur: f64,          // valeur - apports nets
    pub time_weighted_return_pct: f64,
    pub nav_now: f64,
    pub tranches: Vec<Tranche>,
    pub warnings: Vec<String>,
}