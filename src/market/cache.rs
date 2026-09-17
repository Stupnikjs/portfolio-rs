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

    /// Point connu le plus proche AVANT ou égal à `ts`, sans limite d'âge.
    /// Correct pour une requête sur une date passée (immuable). À NE PAS
    /// utiliser pour un prix "maintenant" (voir `get_exact`) : sinon une
    /// vieille valeur en cache est resservie indéfiniment.
    fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.data.get(&symbol.to_uppercase())?.range(..=ts).next_back().map(|(_, &v)| v)
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

    pub(crate) fn get_closest(&self, symbol: &str, ts: i64) -> Option<f64> {
        self.cache.lock().unwrap().get_closest(symbol, self.resolution.align(ts))
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