//! Cash disponible et apports nets XTB, par compte (CTO / PEA), à une date.
//!
//! Source : les opérations de cash importées par `parse_cash_operations`
//! (`amount` signé, `cash_flow` posé par le parseur). Le cash Binance n'est
//! pas suivi (pas de jambe cash sur les achats, dépôts non importés).
//!
//! Limite : les types listés dans `SKIP_CASH_TYPES` (ex. "PEA deposit") sont
//! ignorés à l'import, donc absents ici : comparer avec le solde affiché par XTB.

use chrono::{DateTime, Utc};

use crate::ledger::realized::{tax_account, TaxAccount};
use crate::schema::{AssetKind, CashFlowKind, CashPosition, CashSummary, Platform, Transaction};
use crate::store::serialize::TxStore;

fn is_xtb_cash(tx: &Transaction) -> bool {
    tx.platform == Platform::Xtb && tx.asset.kind == AssetKind::Cash
}

/// Cash et apports nets d'un compte XTB à la date `at` (incluse ; `None` = tout).
pub fn cash_position(tx_store: &TxStore, account: TaxAccount, at: Option<DateTime<Utc>>) -> CashPosition {
    let mut pos = CashPosition::default();
    let in_scope = |tx: &&Transaction| {
        is_xtb_cash(tx) && tax_account(tx) == account && at.map_or(true, |at| tx.time <= at)
    };
    for tx in tx_store.transactions.iter().filter(in_scope) {
        let signed = tx.amount.unwrap_or(0.0);
        pos.cash_eur += signed;
        if matches!(tx.cash_flow, Some(CashFlowKind::ExternalDeposit | CashFlowKind::ExternalWithdrawal)) {
            pos.net_contributions_eur += signed;
        }
    }
    pos
}

pub fn cash_summary(tx_store: &TxStore, at: Option<DateTime<Utc>>) -> CashSummary {
    let cto = cash_position(tx_store, TaxAccount::Cto, at);
    let pea = cash_position(tx_store, TaxAccount::Pea, at);
    let total = CashPosition {
        cash_eur: cto.cash_eur + pea.cash_eur,
        net_contributions_eur: cto.net_contributions_eur + pea.net_contributions_eur,
    };
    CashSummary { total, cto, pea }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TransactionKind::{self, Deposit, Fee, Withdraw};
    use crate::testutil::{store, tx, utc};
    use CashFlowKind::{ExternalDeposit, ExternalWithdrawal, Income, Tax, TradeLeg};

    const CTO_FILE: &str = "./data/raw/accounts/account.xlsx";
    const PEA_FILE: &str = "./data/raw/accounts/account_pea.xlsx";

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Opération de cash XTB : `signed` est le montant du relevé (négatif = sortie).
    fn op(file: &str, kind: TransactionKind, flow: CashFlowKind, signed: f64, time: &str, id: &str) -> Transaction {
        let mut t = tx(kind, "EUR", AssetKind::Cash, signed.abs(), signed.abs(), time, id);
        t.platform = Platform::Xtb;
        t.source_file = file.to_string();
        t.amount = Some(signed);
        t.cash_flow = Some(flow);
        t
    }

    #[test]
    fn cash_is_the_sum_of_signed_amounts_and_contributions_only_count_external_flows() {
        let s = store(vec![
            op(CTO_FILE, Deposit, ExternalDeposit, 1_000.0, "2025-01-01T09:00:00Z", "1"),
            op(CTO_FILE, Withdraw, TradeLeg, -400.0, "2025-01-02T09:00:00Z", "2"),
            op(CTO_FILE, Deposit, Income, 12.5, "2025-01-03T09:00:00Z", "3"),
            op(CTO_FILE, Fee, Tax, -3.0, "2025-01-03T09:05:00Z", "4"),
        ]);
        let p = cash_position(&s, TaxAccount::Cto, None);

        assert!(approx(p.cash_eur, 609.5));
        assert!(approx(p.net_contributions_eur, 1_000.0));
    }

    #[test]
    fn an_external_withdrawal_lowers_both_cash_and_contributions() {
        let s = store(vec![
            op(CTO_FILE, Deposit, ExternalDeposit, 1_000.0, "2025-01-01T09:00:00Z", "1"),
            op(CTO_FILE, Withdraw, ExternalWithdrawal, -300.0, "2025-02-01T09:00:00Z", "2"),
        ]);
        let p = cash_position(&s, TaxAccount::Cto, None);

        assert!(approx(p.cash_eur, 700.0));
        assert!(approx(p.net_contributions_eur, 700.0));
    }

    #[test]
    fn at_excludes_later_operations() {
        let s = store(vec![
            op(CTO_FILE, Deposit, ExternalDeposit, 1_000.0, "2025-01-01T09:00:00Z", "1"),
            op(CTO_FILE, Deposit, ExternalDeposit, 500.0, "2025-03-01T09:00:00Z", "2"),
        ]);
        let p = cash_position(&s, TaxAccount::Cto, Some(utc("2025-02-01T00:00:00Z")));

        assert!(approx(p.cash_eur, 1_000.0));
        assert!(approx(p.net_contributions_eur, 1_000.0));
    }

    #[test]
    fn cto_and_pea_are_separated_and_the_total_is_their_sum() {
        let s = store(vec![
            op(CTO_FILE, Deposit, ExternalDeposit, 1_000.0, "2025-01-01T09:00:00Z", "1"),
            op(PEA_FILE, Deposit, ExternalDeposit, 200.0, "2025-01-02T09:00:00Z", "2"),
        ]);
        let c = cash_summary(&s, None);

        assert!(approx(c.cto.cash_eur, 1_000.0));
        assert!(approx(c.pea.cash_eur, 200.0));
        assert!(approx(c.total.cash_eur, 1_200.0));
        assert!(approx(c.total.net_contributions_eur, 1_200.0));
    }

    #[test]
    fn non_xtb_cash_is_ignored() {
        // `tx` crée une transaction Manual : hors périmètre.
        let mut manual = tx(Deposit, "EUR", AssetKind::Cash, 999.0, 999.0, "2025-01-01T09:00:00Z", "m1");
        manual.amount = Some(999.0);
        manual.cash_flow = Some(ExternalDeposit);

        assert_eq!(cash_summary(&store(vec![manual]), None), CashSummary::default());
    }
}