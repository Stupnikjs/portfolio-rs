//! Retour sur investissement par tranche de cash apportée -- « méthode des
//! parts » (comme un fonds).
//!
//! Principe : le compte est vu comme un fonds. Chaque apport externe
//! « achète » des parts à la valeur de part (NAV) du moment ; chaque retrait
//! en détruit (FIFO, comme le reste du projet).
//!
//!   NAV            = valeur totale du compte (cash + titres) / parts émises
//!   parts émises   = apport / NAV du jour de l'apport
//!   ROI d'une tranche = NAV aujourd'hui / NAV au jour de l'apport - 1
//!
//! Conséquences à garder en tête :
//! - le retour ne dépend que de la DATE d'entrée, pas du montant : deux
//!   apports le même jour ont le même retour ;
//! - la tranche n'est pas rattachée à des achats précis (le cash est
//!   fongible) ; son retour est celui du compte depuis son entrée ;
//! - `time_weighted_return_pct` (NAV - 1) est le retour du compte
//!   indépendamment des apports ;
//! - la somme (réalisé + latent) des tranches = gain total du compte.
//!
//! Dépend de la qualité du cash : la valeur du compte inclut le solde EUR,
//! reconstruit depuis les `Cash Operations`. Un export incomplet fausse la
//! NAV ; les anomalies détectées sont listées dans `warnings`.
//!
//! Structure : `external_flows` (extraction) -> `build_report` (moteur pur,
//! testable avec une valorisation injectée) -> `tranche_report` (colle avec
//! TxStore et les prix).

use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::ledger::portfolio::{portfolio_snapshot_with, PriceFn};
use crate::ledger::realized::TaxAccount;
use crate::ledger::scope::scope_to_accounts;
use crate::schema::{CashFlowKind, Tranche, TrancheReport, Transaction};
use crate::store::serialize::TxStore;

const EPS: f64 = 1e-9;

/// En dessous, annualiser un retour n'a pas de sens.
const MIN_DAYS_FOR_ANNUALIZATION: i64 = 30;

/// Apport (> 0) ou retrait (< 0) externe, en EUR.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub time: DateTime<Utc>,
    pub amount_eur: f64,
}

/// Valeur d'un compte à une date, et actifs détenus dont le prix manque.
#[derive(Debug, Clone, Default)]
pub struct Valuation {
    pub value_eur: f64,
    pub missing_prices: Vec<String>,
}

/// Tranche en cours de calcul (avant conversion en `Tranche` sérialisable).
struct OpenTranche {
    date: NaiveDate,
    invested_eur: f64,
    nav_at_entry: f64,
    units: f64,
    realized_gain_eur: f64,
    /// Capital déjà présent avant le premier apport tracé (dépôts non exportés).
    inferred: bool,
}

// ---------------------------------------------------------------------
// Extraction des flux
// ---------------------------------------------------------------------

/// Apports/retraits externes d'un wallet (déjà restreint à un compte),
/// triés par date. Les mouvements à la même seconde sont fusionnés : la
/// valeur « juste avant » ne serait pas définie entre eux.
pub fn external_flows(scoped: &TxStore) -> Vec<Flow> {
    let mut txs: Vec<&Transaction> = scoped
        .transactions
        .iter()
        .filter(|t| matches!(t.cash_flow, Some(CashFlowKind::ExternalDeposit | CashFlowKind::ExternalWithdrawal)))
        .collect();
    txs.sort_by_key(|t| t.time);

    let mut flows: Vec<Flow> = Vec::new();
    for tx in txs {
        let signed = match tx.cash_flow {
            Some(CashFlowKind::ExternalDeposit) => tx.value_eur,
            _ => -tx.value_eur,
        };
        match flows.last_mut() {
            Some(last) if last.time == tx.time => last.amount_eur += signed,
            _ => flows.push(Flow { time: tx.time, amount_eur: signed }),
        }
    }
    flows.retain(|f| f.amount_eur.abs() > EPS);
    flows
}

// ---------------------------------------------------------------------
// Moteur (pur)
// ---------------------------------------------------------------------

/// Valeur d'une part. 1.0 tant qu'aucune part n'existe ; `None` si des
/// parts existent mais que le compte ne vaut plus rien (NAV indéfinie).
fn unit_value(value_eur: f64, units: f64) -> Option<f64> {
    if units <= EPS {
        Some(1.0)
    } else if value_eur <= EPS {
        None
    } else {
        Some(value_eur / units)
    }
}

fn total_units(tranches: &[OpenTranche]) -> f64 {
    tranches.iter().map(|t| t.units).sum()
}

/// Détruit `units` parts en commençant par la tranche la plus ancienne et
/// enregistre le gain réalisé correspondant.
fn burn_units(tranches: &mut [OpenTranche], mut units: f64, nav: f64) {
    for t in tranches.iter_mut() {
        if units <= EPS {
            break;
        }
        let burned = t.units.min(units);
        t.units -= burned;
        t.realized_gain_eur += burned * (nav - t.nav_at_entry);
        units -= burned;
    }
}

fn annualized_pct(ratio: f64, days: i64) -> Option<f64> {
    (days >= MIN_DAYS_FOR_ANNUALIZATION && ratio > 0.0).then(|| (ratio.powf(365.0 / days as f64) - 1.0) * 100.0)
}

fn note_missing_prices(warnings: &mut Vec<String>, valuation: &Valuation, at: DateTime<Utc>) {
    if !valuation.missing_prices.is_empty() {
        warnings.push(format!(
            "{} : prix manquant pour {} (valeur sous-estimée)",
            at.format("%Y-%m-%d"),
            valuation.missing_prices.join(", ")
        ));
    }
}

/// Construit le rapport. `value_at(t)` doit renvoyer la valeur du compte en
/// tenant compte de toutes les transactions de date <= t.
pub fn build_report(
    account: &str,
    flows: &[Flow],
    now: DateTime<Utc>,
    value_at: &mut dyn FnMut(DateTime<Utc>) -> Valuation,
) -> TrancheReport {
    let mut tranches: Vec<OpenTranche> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut net_contributions_eur = 0.0;
    let mut last_nav = 1.0;

    if flows.is_empty() {
        warnings.push("aucun apport externe détecté pour ce compte".to_string());
    }

    for flow in flows {
        let just_before = flow.time - Duration::seconds(1);
        let before = value_at(just_before);
        note_missing_prices(&mut warnings, &before, just_before);

        // Le compte contenait déjà du capital avant le premier apport tracé :
        // on le matérialise en tranche d'ouverture (NAV 1) plutôt que de lui
        // attribuer à tort le rendement du premier apport.
        if total_units(&tranches) <= EPS && before.value_eur > EPS {
            tranches.push(OpenTranche {
                date: just_before.date_naive(),
                invested_eur: before.value_eur,
                nav_at_entry: 1.0,
                units: before.value_eur,
                realized_gain_eur: 0.0,
                inferred: true,
            });
            net_contributions_eur += before.value_eur;
            warnings.push(format!(
                "{} : capital préexistant de {:.2} EUR avant le premier apport tracé (dépôts antérieurs absents de l'export)",
                just_before.format("%Y-%m-%d"),
                before.value_eur
            ));
        }

        let Some(nav) = unit_value(before.value_eur, total_units(&tranches)) else {
            warnings.push(format!(
                "{} : compte à valeur nulle avant ce mouvement, apport/retrait de {:+.2} EUR ignoré",
                just_before.format("%Y-%m-%d"),
                flow.amount_eur
            ));
            continue;
        };
        last_nav = nav;

        if flow.amount_eur > 0.0 {
            tranches.push(OpenTranche {
                date: flow.time.date_naive(),
                invested_eur: flow.amount_eur,
                nav_at_entry: nav,
                units: flow.amount_eur / nav,
                realized_gain_eur: 0.0,
                inferred: false,
            });
        } else {
            burn_units(&mut tranches, -flow.amount_eur / nav, nav);
        }
        net_contributions_eur += flow.amount_eur;
    }

    let current = value_at(now);
    note_missing_prices(&mut warnings, &current, now);

    let units = total_units(&tranches);
    let nav_now = if units > EPS { current.value_eur / units } else { last_nav };

    let rows: Vec<Tranche> = tranches
        .iter()
        .map(|t| {
            let ratio = nav_now / t.nav_at_entry;
            let days = (now.date_naive() - t.date).num_days();
            Tranche {
                date: t.date.format("%Y-%m-%d").to_string(),
                invested_eur: t.invested_eur,
                nav_at_entry: t.nav_at_entry,
                current_value_eur: t.units * nav_now,
                unrealized_gain_eur: t.units * (nav_now - t.nav_at_entry),
                realized_gain_eur: t.realized_gain_eur,
                roi_pct: (ratio - 1.0) * 100.0,
                annualized_pct: annualized_pct(ratio, days),
                inferred: t.inferred,
            }
        })
        .collect();

    TrancheReport {
        account: account.to_string(),
        net_contributions_eur,
        current_value_eur: current.value_eur,
        total_gain_eur: current.value_eur - net_contributions_eur,
        time_weighted_return_pct: (nav_now - 1.0) * 100.0,
        nav_now,
        tranches: rows,
        warnings,
    }
}

// ---------------------------------------------------------------------
// Colle avec le wallet et les prix
// ---------------------------------------------------------------------

fn valuation(scoped: &TxStore, at: DateTime<Utc>, price_fn: PriceFn) -> Valuation {
    let snapshot = portfolio_snapshot_with(scoped, Some(at), price_fn);
    let missing_prices = snapshot
        .assets
        .iter()
        .filter(|a| !(a.price_eur > 0.0 && a.price_eur.is_finite()))
        .map(|a| a.symbol.clone())
        .collect();
    Valuation { value_eur: snapshot.total_value_eur, missing_prices }
}

/// Rapport par tranche d'un ou plusieurs comptes fiscaux vus comme un seul.
/// `label` est le nom affiché dans dashboard.json.
///
/// Passer `&[Cto, Pea]` quand les virements CTO -> PEA sont ignorés par le
/// parser (`PEA deposit`) : pris séparément, le cash sorti du CTO y reste
/// comme du cash fantôme ; réunis, les deux jambes s'annulent.
pub fn tranche_report(
    tx_store: &TxStore,
    accounts: &[TaxAccount],
    label: &str,
    price_fn: PriceFn,
    now: DateTime<Utc>,
) -> TrancheReport {
    let scoped = scope_to_accounts(tx_store, accounts);
    let flows = external_flows(&scoped);
    let mut value_at = |at: DateTime<Utc>| valuation(&scoped, at, price_fn);
    build_report(label, &flows, now, &mut value_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::{Buy, Deposit, Withdraw};
    use crate::schema::{AssetKind, Platform};
    use crate::testutil::{store, tx, utc};

    const CTO_FILE: &str = "./data/raw/accounts/account.xlsx";
    const PEA_FILE: &str = "./data/raw/accounts/account_pea.xlsx";

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn flow(time: &str, amount_eur: f64) -> Flow {
        Flow { time: utc(time), amount_eur }
    }

    /// Valorisation « en escalier » : valeur du dernier point <= date demandée
    /// (0 avant le premier). Les points incluent l'effet des apports.
    fn step_values(points: &[(&str, f64)]) -> impl FnMut(DateTime<Utc>) -> Valuation {
        let pts: Vec<(DateTime<Utc>, f64)> = points.iter().map(|(t, v)| (utc(t), *v)).collect();
        move |at| Valuation {
            value_eur: pts.iter().rev().find(|(t, _)| *t <= at).map_or(0.0, |(_, v)| *v),
            missing_prices: Vec::new(),
        }
    }

    // --- build_report (moteur pur) ---

    #[test]
    fn a_single_tranche_earns_the_account_return() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0)];
        let mut values = step_values(&[("2025-01-01T10:00:00Z", 1_000.0), ("2025-07-01T00:00:00Z", 1_100.0)]);
        let r = build_report("T", &flows, utc("2025-07-02T00:00:00Z"), &mut values);

        let t = &r.tranches[0];
        assert!(approx(t.nav_at_entry, 1.0));
        assert!(approx(t.roi_pct, 10.0));
        assert!(approx(t.current_value_eur, 1_100.0));
        assert!(approx(t.unrealized_gain_eur, 100.0));
        assert!(approx(r.nav_now, 1.1));
        assert!(approx(r.time_weighted_return_pct, 10.0));
        assert!(approx(r.net_contributions_eur, 1_000.0));
        assert!(approx(r.total_gain_eur, 100.0));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn a_later_deposit_enters_at_the_higher_nav_and_earns_less() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0), flow("2025-06-01T10:00:00Z", 1_000.0)];
        let mut values = step_values(&[
            ("2025-01-01T10:00:00Z", 1_000.0),
            ("2025-05-31T00:00:00Z", 1_200.0), // +20 % avant le 2e apport
            ("2025-06-01T10:00:00Z", 2_200.0),
            ("2025-11-30T00:00:00Z", 2_640.0),
        ]);
        let r = build_report("T", &flows, utc("2025-12-01T00:00:00Z"), &mut values);

        assert!(approx(r.tranches[1].nav_at_entry, 1.2));
        assert!(approx(r.nav_now, 1.44));
        assert!(approx(r.tranches[0].roi_pct, 44.0));
        assert!(approx(r.tranches[1].roi_pct, 20.0));
    }

    #[test]
    fn tranche_gains_add_up_to_the_total_gain_of_the_account() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0), flow("2025-06-01T10:00:00Z", 1_000.0)];
        let mut values = step_values(&[
            ("2025-01-01T10:00:00Z", 1_000.0),
            ("2025-05-31T00:00:00Z", 1_200.0),
            ("2025-06-01T10:00:00Z", 2_200.0),
            ("2025-11-30T00:00:00Z", 2_640.0),
        ]);
        let r = build_report("T", &flows, utc("2025-12-01T00:00:00Z"), &mut values);

        let sum: f64 = r.tranches.iter().map(|t| t.realized_gain_eur + t.unrealized_gain_eur).sum();
        assert!(approx(sum, r.total_gain_eur));
        assert!(approx(r.total_gain_eur, 640.0));
    }

    #[test]
    fn a_withdrawal_burns_the_oldest_units_and_realizes_their_gain() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0), flow("2025-04-01T10:00:00Z", -750.0)];
        let mut values = step_values(&[
            ("2025-01-01T10:00:00Z", 1_000.0),
            ("2025-03-01T00:00:00Z", 1_500.0), // NAV 1.5 au moment du retrait
            ("2025-04-01T10:00:00Z", 750.0),
            ("2025-06-01T00:00:00Z", 900.0),
        ]);
        let r = build_report("T", &flows, utc("2025-07-01T00:00:00Z"), &mut values);

        let t = &r.tranches[0];
        assert!(approx(t.realized_gain_eur, 250.0)); // 500 parts x (1.5 - 1.0)
        assert!(approx(r.nav_now, 1.8)); // 900 / 500 parts
        assert!(approx(t.unrealized_gain_eur, 400.0)); // 500 x (1.8 - 1.0)
        assert!(approx(r.net_contributions_eur, 250.0));
        assert!(approx(r.total_gain_eur, 650.0));
        assert!(approx(t.realized_gain_eur + t.unrealized_gain_eur, r.total_gain_eur));
    }

    #[test]
    fn capital_present_before_the_first_tracked_deposit_becomes_an_inferred_opening_tranche() {
        let flows = [flow("2025-03-01T10:00:00Z", 500.0)];
        let mut values = step_values(&[
            ("2025-01-01T00:00:00Z", 2_000.0), // déjà 2000 EUR sur le compte
            ("2025-03-01T10:00:00Z", 2_500.0),
            ("2025-05-01T00:00:00Z", 2_750.0),
        ]);
        let r = build_report("T", &flows, utc("2025-06-01T00:00:00Z"), &mut values);

        assert_eq!(r.tranches.len(), 2);
        assert!(r.tranches[0].inferred);
        assert!(approx(r.tranches[0].invested_eur, 2_000.0));
        assert!(!r.tranches[1].inferred);
        assert!(approx(r.tranches[1].nav_at_entry, 1.0));
        assert!(approx(r.nav_now, 1.1));
        assert!(approx(r.net_contributions_eur, 2_500.0));
        assert!(r.warnings.iter().any(|w| w.contains("préexistant")));
    }

    #[test]
    fn a_deposit_into_a_worthless_account_with_existing_units_is_skipped_with_a_warning() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0), flow("2025-02-01T10:00:00Z", 500.0)];
        let mut values = step_values(&[("2025-01-01T10:00:00Z", 1_000.0), ("2025-01-15T00:00:00Z", 0.0)]);
        let r = build_report("T", &flows, utc("2025-03-01T00:00:00Z"), &mut values);

        assert_eq!(r.tranches.len(), 1);
        assert!(r.warnings.iter().any(|w| w.contains("valeur nulle")));
    }

    #[test]
    fn no_flows_gives_no_tranche_and_a_warning() {
        let mut values = step_values(&[("2025-01-01T00:00:00Z", 100.0)]);
        let r = build_report("T", &[], utc("2025-06-01T00:00:00Z"), &mut values);

        assert!(r.tranches.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("aucun apport")));
    }

    #[test]
    fn missing_prices_are_reported_as_warnings() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0)];
        let mut values = |_at: DateTime<Utc>| Valuation { value_eur: 1_000.0, missing_prices: vec!["AIR".to_string()] };
        let r = build_report("T", &flows, utc("2025-02-01T00:00:00Z"), &mut values);

        assert!(r.warnings.iter().any(|w| w.contains("AIR")));
    }

    #[test]
    fn annualized_return_needs_at_least_a_month_of_history() {
        let flows = [flow("2025-01-01T10:00:00Z", 1_000.0)];
        let points = [("2025-01-01T10:00:00Z", 1_000.0), ("2025-12-31T00:00:00Z", 1_100.0)];

        let mut v = step_values(&points);
        let one_year = build_report("T", &flows, utc("2026-01-01T00:00:00Z"), &mut v);
        assert!((one_year.tranches[0].annualized_pct.unwrap() - 10.0).abs() < 1e-6);

        let mut v = step_values(&[("2025-01-01T10:00:00Z", 1_000.0), ("2025-01-05T00:00:00Z", 1_100.0)]);
        let ten_days = build_report("T", &flows, utc("2025-01-11T00:00:00Z"), &mut v);
        assert_eq!(ten_days.tranches[0].annualized_pct, None);
    }

    // --- external_flows ---

    fn flagged(
        kind: crate::schema::TransactionKind,
        qty: f64,
        time: &str,
        id: &str,
        cash_flow: CashFlowKind,
    ) -> Transaction {
        let mut t = tx(kind, "EUR", AssetKind::Cash, qty, qty, time, id);
        t.cash_flow = Some(cash_flow);
        t
    }

    #[test]
    fn only_external_deposits_and_withdrawals_are_flows_and_same_second_ones_are_merged() {
        let s = store(vec![
            flagged(Deposit, 1_000.0, "2025-01-01T10:00:00Z", "d1", CashFlowKind::ExternalDeposit),
            flagged(Deposit, 500.0, "2025-01-01T10:00:00Z", "d2", CashFlowKind::ExternalDeposit),
            flagged(Deposit, 20.0, "2025-02-01T10:00:00Z", "i1", CashFlowKind::Income), // dividende
            flagged(Withdraw, 300.0, "2025-02-02T10:00:00Z", "t1", CashFlowKind::TradeLeg), // jambe d'achat
            flagged(Withdraw, 300.0, "2025-03-01T10:00:00Z", "w1", CashFlowKind::ExternalWithdrawal),
        ]);
        let flows = external_flows(&s);

        assert_eq!(flows.len(), 2);
        assert!(approx(flows[0].amount_eur, 1_500.0));
        assert!(approx(flows[1].amount_eur, -300.0));
    }

    // --- tranche_report (de bout en bout, sans réseau) ---

    fn xtb(
        file: &str,
        kind: crate::schema::TransactionKind,
        symbol: &str,
        asset_kind: AssetKind,
        qty: f64,
        value: f64,
        time: &str,
        id: &str,
        cash_flow: Option<CashFlowKind>,
    ) -> Transaction {
        let mut t = tx(kind, symbol, asset_kind, qty, value, time, id);
        t.platform = Platform::Xtb;
        t.source_file = file.to_string();
        t.cash_flow = cash_flow;
        t
    }

    #[test]
    fn end_to_end_values_cash_and_stocks_and_ignores_the_other_account() {
        let s = store(vec![
            xtb(CTO_FILE, Deposit, "EUR", AssetKind::Cash, 1_000.0, 1_000.0, "2025-01-01T10:00:00Z", "c1", Some(CashFlowKind::ExternalDeposit)),
            xtb(CTO_FILE, Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2025-01-02T10:00:00Z", "s1", None),
            xtb(CTO_FILE, Withdraw, "EUR", AssetKind::Cash, 1_000.0, 1_000.0, "2025-01-02T10:00:00Z", "c2", Some(CashFlowKind::TradeLeg)),
            // Apport sur le PEA : ne doit pas compter dans le CTO.
            xtb(PEA_FILE, Deposit, "EUR", AssetKind::Cash, 5_000.0, 5_000.0, "2025-01-03T10:00:00Z", "p1", Some(CashFlowKind::ExternalDeposit)),
        ]);
        let prices = |symbol: &str, _k: AssetKind, _t: Option<&str>, _at: DateTime<Utc>| match symbol {
            "EUR" => 1.0,
            "AIR" => 120.0,
            _ => 0.0,
        };

        let r = tranche_report(&s, &[TaxAccount::Cto], "CTO", &prices, utc("2025-06-01T00:00:00Z"));

        assert_eq!(r.account, "CTO");
        assert_eq!(r.tranches.len(), 1);
        assert!(approx(r.net_contributions_eur, 1_000.0));
        assert!(approx(r.current_value_eur, 1_200.0)); // 10 AIR x 120, cash à 0
        assert!(approx(r.tranches[0].roi_pct, 20.0));
        assert!(r.warnings.is_empty());
    }

    /// Virement CTO -> PEA ignoré par le parser : l'apport est sur le CTO, l'achat
    /// (et sa jambe cash) sur le PEA, sans entrée de cash correspondante sur le PEA.
    fn transfer_ignored_fixture() -> TxStore {
        store(vec![
            xtb(CTO_FILE, Deposit, "EUR", AssetKind::Cash, 1_000.0, 1_000.0, "2025-01-01T10:00:00Z", "c1", Some(CashFlowKind::ExternalDeposit)),
            xtb(PEA_FILE, Buy, "AIR", AssetKind::Stock, 10.0, 1_000.0, "2025-01-02T10:00:00Z", "s1", None),
            xtb(PEA_FILE, Withdraw, "EUR", AssetKind::Cash, 1_000.0, 1_000.0, "2025-01-02T10:00:00Z", "c2", Some(CashFlowKind::TradeLeg)),
        ])
    }

    fn air_at_120(symbol: &str, _k: AssetKind, _t: Option<&str>, _at: DateTime<Utc>) -> f64 {
        match symbol {
            "EUR" => 1.0,
            "AIR" => 120.0,
            _ => 0.0,
        }
    }

    #[test]
    fn cto_and_pea_grouped_cancel_the_ignored_transfer() {
        let s = transfer_ignored_fixture();
        let now = utc("2025-06-01T00:00:00Z");
        let r = tranche_report(&s, &[TaxAccount::Cto, TaxAccount::Pea], "XTB", &air_at_120, now);

        assert!(approx(r.current_value_eur, 1_200.0)); // cash 1000 - 1000 = 0, AIR 1200
        assert!(approx(r.tranches[0].roi_pct, 20.0));
    }

    #[test]
    fn cto_alone_keeps_phantom_cash_when_the_transfer_is_ignored() {
        // Documente le piège : séparé du PEA, le CTO garde 1000 EUR de cash fantôme.
        let s = transfer_ignored_fixture();
        let now = utc("2025-06-01T00:00:00Z");
        let r = tranche_report(&s, &[TaxAccount::Cto], "CTO", &air_at_120, now);

        assert!(approx(r.current_value_eur, 1_000.0));
        assert!(approx(r.tranches[0].roi_pct, 0.0));
    }
}
