//! Currency converter using the European Central Bank's daily reference rates
//! (via the free Frankfurter API, no account). Port of Source/toolkit/currency.py
//! plus currency_status / currency_convert in Source/swiftlane.py.
//!
//! The last rates are kept on disk in %APPDATA%\Swiftlane\rates.json (same file and
//! format as the Python version) so it still works offline; rates refresh at most
//! every six hours unless forced.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use super::arg;
use crate::config;

const API: &str = "https://api.frankfurter.dev/v1";
const REFRESH_SECONDS: f64 = 6.0 * 3600.0;
const MAX_RESPONSE_BYTES: u64 = 512 * 1024;

const DEFAULT_NAMES: &[(&str, &str)] = &[
    ("EUR", "Euro"),
    ("USD", "US Dollar"),
    ("GBP", "British Pound"),
    ("CHF", "Swiss Franc"),
    ("JPY", "Japanese Yen"),
    ("CNY", "Chinese Yuan"),
    ("CAD", "Canadian Dollar"),
    ("AUD", "Australian Dollar"),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CacheData {
    #[serde(default)]
    rates: BTreeMap<String, f64>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    names: BTreeMap<String, String>,
    #[serde(default)]
    fetched: f64,
}

pub struct Rates {
    cache_file: PathBuf,
    data: CacheData,
    error: Option<String>,
}

fn default_names() -> BTreeMap<String, String> {
    DEFAULT_NAMES
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("Swiftlane")
        .https_only(true)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "ClientError".to_string())
}

fn fetch_json(c: &reqwest::blocking::Client, path: &str) -> Result<Value, String> {
    let url = format!("{API}/{path}");
    if !url.starts_with("https://") {
        return Err("InvalidUrl".to_string());
    }
    let resp = c.get(&url).send().map_err(|e| {
        if e.is_timeout() {
            "Timeout".to_string()
        } else {
            "ConnectionError".to_string()
        }
    })?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut buf = Vec::new();
    resp.take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "ReadError".to_string())?;
    if buf.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("ResponseTooLarge".to_string());
    }
    serde_json::from_slice(&buf).map_err(|_| "InvalidJson".to_string())
}

fn parse_latest(latest: &Value) -> Result<(BTreeMap<String, f64>, Option<String>), String> {
    let rates_obj = latest
        .get("rates")
        .and_then(Value::as_object)
        .ok_or_else(|| "MissingRates".to_string())?;
    let mut rates = BTreeMap::new();
    rates.insert("EUR".to_string(), 1.0);
    for (code, val) in rates_obj {
        if let Some(num) = val.as_f64().filter(|v| v.is_finite() && *v > 0.0) {
            rates.insert(code.clone(), num);
        }
    }
    let date = latest.get("date").and_then(Value::as_str).map(String::from);
    Ok((rates, date))
}

fn parse_names(val: &Value) -> Option<BTreeMap<String, String>> {
    let obj = val.as_object()?;
    let mut names = BTreeMap::new();
    for (code, name) in obj {
        if let Some(s) = name.as_str() {
            names.insert(code.clone(), s.to_string());
        }
    }
    if names.is_empty() { None } else { Some(names) }
}

fn parse_amount(v: &Value) -> Option<f64> {
    let num = if let Some(n) = v.as_f64() {
        Some(n)
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<f64>().ok()
    } else {
        None
    }?;
    num.is_finite().then_some(num)
}

impl Rates {
    pub fn new(cache_file: PathBuf) -> Self {
        let data = Self::load(&cache_file);
        Self { cache_file, data, error: None }
    }

    fn load(path: &Path) -> CacheData {
        let Ok(text) = std::fs::read_to_string(path) else {
            return CacheData::default();
        };
        let Ok(mut parsed) = serde_json::from_str::<CacheData>(&text) else {
            return CacheData::default();
        };
        parsed.rates.retain(|_, v| v.is_finite() && *v > 0.0);
        if !parsed.fetched.is_finite() {
            parsed.fetched = 0.0;
        }
        parsed
    }

    fn save(&self) {
        if let Some(parent) = self.cache_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(text) = serde_json::to_string(&self.data) else {
            return;
        };
        let mut tmp = self.cache_file.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if std::fs::write(&tmp, text).is_ok() {
            if std::fs::rename(&tmp, &self.cache_file).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Get fresh rates if the saved ones are old. Returns status() either way.
    pub fn refresh(&mut self, force: bool) -> Value {
        let fresh = (now_secs() - self.data.fetched) < REFRESH_SECONDS;
        if fresh && !force && !self.data.rates.is_empty() {
            return self.status();
        }
        match self.fetch_and_apply() {
            Ok(()) => {
                self.error = None;
                self.save();
            }
            Err(reason) => {
                self.error = Some(if !self.data.rates.is_empty() {
                    format!("Couldn't update rates ({reason}); using saved ones.")
                } else {
                    "No internet connection and no saved rates yet.".to_string()
                });
            }
        }
        self.status()
    }

    fn fetch_and_apply(&mut self) -> Result<(), String> {
        let c = client()?;
        let latest = fetch_json(&c, "latest?base=EUR")?;
        let (rates, date) = parse_latest(&latest)?;
        let names = fetch_json(&c, "currencies")
            .ok()
            .and_then(|v| parse_names(&v))
            .unwrap_or_else(|| {
                if !self.data.names.is_empty() {
                    self.data.names.clone()
                } else {
                    default_names()
                }
            });
        self.data = CacheData {
            rates,
            date,
            names,
            fetched: now_secs(),
        };
        Ok(())
    }

    pub fn status(&self) -> Value {
        let fallback = default_names();
        let names = if self.data.names.is_empty() { &fallback } else { &self.data.names };
        let currencies: Vec<Value> = self
            .data
            .rates
            .keys()
            .map(|code| {
                let name = names.get(code).map(String::as_str).unwrap_or(code.as_str());
                json!({ "code": code, "name": name })
            })
            .collect();
        json!({
            "date": self.data.date,
            "error": self.error,
            "currencies": currencies,
        })
    }

    pub fn convert(&self, amount: f64, src: &str, dst: &str) -> Result<Value, String> {
        if !amount.is_finite() {
            return Err("Invalid amount".to_string());
        }
        let src_rate = self.data.rates.get(src).copied().filter(|r| *r > 0.0);
        let dst_rate = self.data.rates.get(dst).copied().filter(|r| *r > 0.0);
        let (Some(src_rate), Some(dst_rate)) = (src_rate, dst_rate) else {
            return Err("Unknown currency".to_string());
        };
        let value = amount / src_rate * dst_rate;
        let rate = dst_rate / src_rate;
        Ok(json!({
            "value": value,
            "rate": rate,
            "date": self.data.date,
        }))
    }
}

static RATES: OnceLock<Mutex<Rates>> = OnceLock::new();

fn rates() -> &'static Mutex<Rates> {
    RATES.get_or_init(|| Mutex::new(Rates::new(config::config_dir().join("rates.json"))))
}

pub fn dispatch(_app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "currency_status" => {
            let force = arg(args, 0).as_bool().unwrap_or(false);
            rates().lock().unwrap().refresh(force)
        }
        "currency_convert" => {
            let Some(amount) = parse_amount(arg(args, 0)) else {
                return Some(json!({ "error": "Enter a number" }));
            };
            let src = arg(args, 1).as_str().unwrap_or("");
            let dst = arg(args, 2).as_str().unwrap_or("");
            match rates().lock().unwrap().convert(amount, src, dst) {
                Ok(v) => v,
                Err(_) => json!({ "error": "Enter a number" }),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir() -> PathBuf {
        let mut b = [0u8; 6];
        getrandom::getrandom(&mut b).unwrap();
        let dir = std::env::temp_dir().join(format!("swl-currency-test-{}", hex::encode(b)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn converts_and_sorts_cached_rates() {
        let dir = fresh_dir();
        let cache = dir.join("rates.json");
        let saved = json!({
            "rates": { "USD": 1.1, "EUR": 1.0, "GBP": 0.85, "BAD": 0.0 },
            "date": "2026-11-23",
            "names": { "USD": "US Dollar", "EUR": "Euro" },
            "fetched": now_secs(),
        });
        std::fs::write(&cache, saved.to_string()).unwrap();

        let mut r = Rates::new(cache);
        let st = r.refresh(false); // fresh cache: no network call
        assert_eq!(st["date"], "2026-11-23");
        assert!(st["error"].is_null());
        let codes: Vec<&str> = st["currencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, vec!["EUR", "GBP", "USD"]);
        // GBP missing from saved names falls back to its code (matching Python names.get(c, c))
        assert_eq!(st["currencies"][1]["name"], "GBP");

        let res = r.convert(110.0, "USD", "EUR").unwrap();
        assert!((res["value"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        assert!((res["rate"].as_f64().unwrap() - (1.0 / 1.1)).abs() < 1e-9);
        assert_eq!(res["date"], "2026-11-23");

        assert!(r.convert(10.0, "XXX", "EUR").is_err());
        assert!(r.convert(f64::NAN, "USD", "EUR").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_frankfurter_payloads_and_amounts() {
        let latest = json!({
            "amount": 1.0,
            "base": "EUR",
            "date": "2026-11-20",
            "rates": { "USD": 1.08, "JPY": 165.5, "ZERO": 0.0, "NEG": -2.0 }
        });
        let (rates, date) = parse_latest(&latest).unwrap();
        assert_eq!(date.as_deref(), Some("2026-11-20"));
        assert_eq!(rates.get("EUR"), Some(&1.0));
        assert_eq!(rates.get("USD"), Some(&1.08));
        assert_eq!(rates.get("JPY"), Some(&165.5));
        assert!(!rates.contains_key("ZERO"));
        assert!(!rates.contains_key("NEG"));

        let names = parse_names(&json!({ "EUR": "Euro", "USD": "US Dollar" })).unwrap();
        assert_eq!(names.get("USD").map(String::as_str), Some("US Dollar"));

        assert_eq!(parse_amount(&json!(42.5)), Some(42.5));
        assert_eq!(parse_amount(&json!(" 19.99 ")), Some(19.99));
        assert_eq!(parse_amount(&json!("abc")), None);
        assert_eq!(parse_amount(&Value::Null), None);
    }

    #[test]
    fn handles_missing_and_corrupt_cache() {
        let dir = fresh_dir();
        let missing = Rates::new(dir.join("does-not-exist.json"));
        assert_eq!(missing.status()["currencies"].as_array().unwrap().len(), 0);

        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, "{not valid json").unwrap();
        let loaded = Rates::new(corrupt);
        assert_eq!(loaded.status()["currencies"].as_array().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
