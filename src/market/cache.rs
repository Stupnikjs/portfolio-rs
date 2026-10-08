//! src/market/cache.rs — cache de prix persistant (bincode), par résolution
//! horaire ou journalière. Isolé de prices.rs pour séparer "stockage" et
//! "récupération / logique métier".

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use chrono::{TimeZone, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Hour,
    Day,
}

impl Resolution {
    pub(crate) fn seconds(self) -> i64 {
        match self {
            Resolution::Hour => 3_600,
            Resolution::Day => 86_400,
        }
    }
    pub(crate) fn align(self, ts: i64) -> i64 {
        ts - ts.rem_euclid(self.seconds())
    }
}

/// True si `aligned_ts` correspond au bucket courant pour `resolution` --
/// sert à distinguer une requête "prix maintenant" d'une requête sur une
/// date passée (immuable).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_live_bucket(aligned_ts: i64, resolution: Resolution) -> bool {
    aligned_ts == resolution.align(Utc::now().timestamp())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PriceCache {
    data: HashMap<String, BTreeMap<i64, f64>>,
}

impl PriceCache {
    fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|b| bincode::deserialize(&b).ok()).unwrap_or_default()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, bincode::serialize(self).expect("bincode serialize"))
    }

    /// Point connu le plus proche AVANT ou égal à `ts`, SANS LIMITE D'ÂGE.
    ///
    /// ATTENTION : ne pas utiliser pour valoriser un actif à une date. Le
    /// cache est creux (un point par requête passée) : le point "le plus
    /// proche avant" peut dater de plusieurs semaines/mois, et il est
    /// resservi à la place d'un vrai appel API -> prix figés dans
    /// history.json et value_eur Binance faux. Utiliser `get_exact` (prix)
    /// ou `get_within` (taux de change, avec une tolérance bornée).
    #[cfg_attr(not(test), allow(dead_code))]
    fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.data.get(&symbol.to_uppercase())?.range(..=ts).next_back().map(|(_, &v)| v)
    }

    /// Point connu dans la fenêtre `[ts - max_age_secs, ts]` : comme
    /// `get_closest` mais un point plus vieux que `max_age_secs` est ignoré.
    fn get_within(&self, symbol: &str, ts: i64, max_age_secs: i64) -> Option<f64> {
        let lower = ts.saturating_sub(max_age_secs.max(0));
        self.data.get(&symbol.to_uppercase())?.range(lower..=ts).next_back().map(|(_, &v)| v)
    }

    /// Point connu exactement au bucket `ts`. Seul moyen sûr de savoir si
    /// on a déjà un prix FRAIS pour "maintenant".
    fn get_exact(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.data.get(&symbol.to_uppercase())?.get(&ts).copied()
    }

    fn latest_ts(&self, symbol: &str) -> Option<i64> {
        self.data.get(&symbol.to_uppercase())?.keys().next_back().copied()
    }

    fn earliest_ts(&self, symbol: &str) -> Option<i64> {
        self.data.get(&symbol.to_uppercase())?.keys().next().copied()
    }

    fn range_since(&self, symbol: &str, since_ts: i64) -> Vec<(i64, f64)> {
        self.data
            .get(&symbol.to_uppercase())
            .map(|m| m.range(since_ts..).map(|(&k, &v)| (k, v)).collect())
            .unwrap_or_default()
    }

    fn insert(&mut self, symbol: &str, ts: i64, price: f64) {
        self.data.entry(symbol.to_uppercase()).or_default().insert(ts, price);
    }
}

pub(crate) struct ResolutionCache {
    resolution: Resolution,
    cache: Mutex<PriceCache>,
}

impl ResolutionCache {
    fn empty(resolution: Resolution) -> Self {
        Self { resolution, cache: Mutex::new(PriceCache::default()) }
    }

    fn init(&self, path: &Path) {
        *self.cache.lock().unwrap() = PriceCache::load(path);
    }

    fn save(&self, path: &Path) {
        if let Err(e) = self.cache.lock().unwrap().save(path) {
            eprintln!("Erreur sauvegarde cache: {e}");
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_closest(symbol, self.resolution.align(ts))
    }

    pub(crate) fn get_within(&self, symbol: &str, ts: i64, max_age_secs: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_within(symbol, self.resolution.align(ts), max_age_secs)
    }

    pub(crate) fn get_exact(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_exact(symbol, self.resolution.align(ts))
    }

    pub(crate) fn latest(&self, symbol: &str) -> Option<i64> {
        self.cache.lock().unwrap().latest_ts(symbol)
    }

    pub(crate) fn earliest(&self, symbol: &str) -> Option<i64> {
        self.cache.lock().unwrap().earliest_ts(symbol)
    }

    pub(crate) fn range_days(&self, symbol: &str, days: i64) -> Vec<(chrono::NaiveDate, f64)> {
        let cutoff = self.resolution.align((Utc::now() - chrono::Duration::days(days)).timestamp());
        self.cache
            .lock()
            .unwrap()
            .range_since(symbol, cutoff)
            .into_iter()
            .map(|(ts, price)| (Utc.timestamp_opt(ts, 0).unwrap().date_naive(), price))
            .collect()
    }

    pub(crate) fn insert(&self, symbol: &str, ts: i64, price: f64) {
        self.cache.lock().unwrap().insert(symbol, self.resolution.align(ts), price);
    }

    pub(crate) fn insert_date(&self, symbol: &str, date: chrono::NaiveDate, price: f64) {
        let ts = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()).timestamp();
        self.insert(symbol, ts, price);
    }
}

pub(crate) static CACHE_1H: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Hour));
pub(crate) static CACHE_1D: Lazy<ResolutionCache> = Lazy::new(|| ResolutionCache::empty(Resolution::Day));

/// Charge les caches persistés sur disque. À appeler une fois au
/// démarrage, avant tout appel aux fonctions de `prices.rs`.
pub fn init_price_caches(dir: &Path) {
    CACHE_1H.init(&dir.join("price_cache_1h.bin"));
    CACHE_1D.init(&dir.join("price_cache_1d.bin"));
}

/// Persiste les caches sur disque. À appeler avant de quitter le
/// programme pour ne pas reperdre les prix récupérés durant le run.
pub fn save_price_caches(dir: &Path) {
    CACHE_1H.save(&dir.join("price_cache_1h.bin"));
    CACHE_1D.save(&dir.join("price_cache_1d.bin"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn hours(n: i64) -> i64 {
        n * 3_600
    }

    fn days(n: i64) -> i64 {
        n * 86_400
    }

    // --- get_exact vs get_closest : le coeur du bug "prix figé" ---

    #[test]
    fn get_exact_only_matches_the_same_bucket() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        let ts0 = 0; // bucket 0

        cache.insert("BTC", ts0, 100.0);

        assert_eq!(cache.get_exact("BTC", ts0), Some(100.0));
        // Une heure plus tard -> bucket différent -> get_exact ne doit RIEN
        // renvoyer (sinon on ressert indéfiniment une vieille valeur pour
        // un prix "maintenant", c'était le bug initial).
        assert_eq!(cache.get_exact("BTC", ts0 + hours(1)), None);
    }

    #[test]
    fn get_closest_reuses_the_last_known_value_across_buckets() {
        // Documente le comportement VOULU de get_closest : correct pour une
        // date passée immuable, mais surtout pas pour "maintenant" (voir
        // get_exact pour ce cas).
        let cache = ResolutionCache::empty(Resolution::Hour);
        let ts0 = 0;

        cache.insert("BTC", ts0, 100.0);

        assert_eq!(cache.get_closest("BTC", ts0 + hours(5)), Some(100.0));
    }

    #[test]
    fn get_closest_never_returns_a_future_point() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        cache.insert("BTC", hours(10), 100.0);

        // Rien de connu au ou avant ts=0, même si un point existe plus tard.
        assert_eq!(cache.get_closest("BTC", 0), None);
    }

    #[test]
    fn symbol_lookup_is_case_insensitive() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        cache.insert("btc", 0, 42.0);

        assert_eq!(cache.get_exact("BTC", 0), Some(42.0));
        assert_eq!(cache.get_closest("Btc", 0), Some(42.0));
    }

    #[test]
    fn insert_aligns_timestamps_to_the_resolution_bucket() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        // Deux insertions dans le même bucket horaire -> la seconde écrase
        // la première (une seule entrée par bucket).
        cache.insert("BTC", 0, 100.0);
        cache.insert("BTC", 1_800, 200.0); // 30 min plus tard, même bucket

        assert_eq!(cache.get_exact("BTC", 0), Some(200.0));
    }

    // --- get_within : remplace get_closest pour valoriser à une date ---

    #[test]
    fn get_within_never_serves_a_point_older_than_max_age() {
        // Régression du bug "prix figés" : un prix connu à la semaine 1 ne
        // doit JAMAIS être resservi pour la semaine 2 (get_closest le faisait).
        let cache = ResolutionCache::empty(Resolution::Hour);
        let week1 = days(10);
        let week2 = week1 + days(7);
        cache.insert("BTC", week1, 100.0);

        assert_eq!(cache.get_closest("BTC", week2), Some(100.0)); // le piège
        assert_eq!(cache.get_within("BTC", week2, hours(3)), None); // corrigé
    }

    #[test]
    fn get_within_returns_a_recent_enough_point() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        cache.insert("USD", hours(100), 1.1);

        assert_eq!(cache.get_within("USD", hours(102), hours(3)), Some(1.1));
        assert_eq!(cache.get_within("USD", hours(103), hours(3)), Some(1.1)); // borne incluse
        assert_eq!(cache.get_within("USD", hours(104), hours(3)), None);
    }

    #[test]
    fn get_within_never_returns_a_future_point_and_zero_age_is_exact() {
        let cache = ResolutionCache::empty(Resolution::Hour);
        cache.insert("USD", hours(10), 1.1);

        assert_eq!(cache.get_within("USD", hours(5), hours(100)), None);
        assert_eq!(cache.get_within("USD", hours(10), 0), Some(1.1));
        assert_eq!(cache.get_within("USD", hours(11), 0), None);
        assert_eq!(cache.get_within("USD", hours(11), -5), None); // âge négatif toléré
    }

    // --- is_live_bucket ---

    #[test]
    fn is_live_bucket_detects_the_current_hour_only() {
        let now_aligned = Resolution::Hour.align(Utc::now().timestamp());

        assert!(is_live_bucket(now_aligned, Resolution::Hour));
        assert!(!is_live_bucket(now_aligned - hours(2), Resolution::Hour));
    }

    // --- earliest / latest : nécessaires pour détecter un historique tronqué ---

    #[test]
    fn earliest_and_latest_track_the_full_inserted_range() {
        let cache = ResolutionCache::empty(Resolution::Day);
        cache.insert("AAPL", days(10), 1.0);
        cache.insert("AAPL", days(20), 2.0);
        cache.insert("AAPL", days(15), 1.5);

        assert_eq!(cache.earliest("AAPL"), Some(days(10)));
        assert_eq!(cache.latest("AAPL"), Some(days(20)));
    }

    #[test]
    fn range_days_excludes_points_older_than_the_window() {
        let cache = ResolutionCache::empty(Resolution::Day);
        let today = Utc::now().date_naive();

        cache.insert_date("AAPL", today - chrono::Duration::days(100), 1.0);
        cache.insert_date("AAPL", today - chrono::Duration::days(5), 2.0);

        let range = cache.range_days("AAPL", 30);
        let dates: Vec<NaiveDate> = range.iter().map(|(d, _)| *d).collect();

        assert!(!dates.contains(&(today - chrono::Duration::days(100))));
        assert!(dates.contains(&(today - chrono::Duration::days(5))));
    }

    #[test]
    fn unknown_symbol_returns_none_everywhere() {
        let cache = ResolutionCache::empty(Resolution::Hour);

        assert_eq!(cache.get_exact("DOES_NOT_EXIST", 0), None);
        assert_eq!(cache.get_closest("DOES_NOT_EXIST", 0), None);
        assert_eq!(cache.latest("DOES_NOT_EXIST"), None);
        assert_eq!(cache.earliest("DOES_NOT_EXIST"), None);
    }
}