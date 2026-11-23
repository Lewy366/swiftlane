//! Internet speed test against Cloudflare's free speed-test servers (speed.cloudflare.com).
//! Port of Source/toolkit/speedtest.py plus speed_test in Source/swiftlane.py.
//!
//! Measures ping/jitter, then download and upload with several parallel connections,
//! reporting live Mbps through "speed" events and the final result as "speed-result".

use serde_json::{json, Value};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::api::{push, AppState};
use crate::jobs::JobError;

const HOST: &str = "https://speed.cloudflare.com";
const STREAMS: usize = 4;
const PHASE_SECONDS: f64 = 8.0;
const WARMUP_SECONDS: f64 = 1.0;
const CHUNK: usize = 64 * 1024;
const UPLOAD_BYTES: usize = 4 * 1024 * 1024;
const RATE_LIMITED: &str = "Cloudflare is limiting speed tests from your connection right now (too many tests in a short time). Try again in a few minutes.";

fn city_for(colo: &str) -> &str {
    match colo {
        "TXL" | "BER" => "Berlin",
        "FRA" => "Frankfurt",
        "HAM" => "Hamburg",
        "MUC" => "Munich",
        "DUS" => "Düsseldorf",
        "STR" => "Stuttgart",
        "AMS" => "Amsterdam",
        "LHR" => "London",
        "CDG" => "Paris",
        "WAW" => "Warsaw",
        "MXP" => "Milan",
        "FCO" => "Rome",
        "VIE" => "Vienna",
        "ZRH" => "Zurich",
        "PRG" => "Prague",
        "CPH" => "Copenhagen",
        "BRU" => "Brussels",
        "MAD" => "Madrid",
        "BCN" => "Barcelona",
        "ARN" => "Stockholm",
        "OSL" => "Oslo",
        "HEL" => "Helsinki",
        "DUB" => "Dublin",
        other => other,
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("Swiftlane")
        .https_only(true)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Couldn't start speed test client: {e}"))
}

fn parse_trace(text: &str) -> Value {
    let mut colo = "";
    let mut loc: Option<&str> = None;
    let mut ip = "";
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "colo" => colo = v,
                "loc" => loc = Some(v),
                "ip" => ip = v,
                _ => {}
            }
        }
    }
    json!({
        "colo": colo,
        "city": city_for(colo),
        "country": loc,
        "ip_version": if ip.contains(':') { "IPv6" } else { "IPv4" },
    })
}

fn server_info() -> Result<Value, String> {
    let c = client()?;
    let resp = c
        .get(format!("{HOST}/cdn-cgi/trace"))
        .send()
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.take(32 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    Ok(parse_trace(&text))
}

fn parse_server_timing_dur(header: &str) -> Option<f64> {
    let rest = header.split_once("dur=")?.1;
    let raw = rest.split([',', ';']).next()?.trim();
    raw.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

fn jitter(values: &[f64]) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    let sum: f64 = values.windows(2).map(|w| (w[0] - w[1]).abs()).sum();
    sum / (values.len() - 1) as f64
}

fn measure_ping(count: usize, cancelled: &dyn Fn() -> bool) -> Result<(f64, f64), JobError> {
    let c = client().map_err(JobError::Failed)?;
    let mut times = Vec::with_capacity(count);
    let url = format!("{HOST}/__down?bytes=0");
    for i in 0..=count {
        if cancelled() {
            return Err(JobError::Cancelled);
        }
        let t0 = Instant::now();
        let resp = c
            .get(&url)
            .send()
            .map_err(|e| JobError::Failed(format!("Couldn't reach Cloudflare: {e}")))?;
        let status = resp.status().as_u16();
        let timing = resp
            .headers()
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut sink = [0u8; 1024];
        let mut r = resp;
        while r.read(&mut sink).unwrap_or(0) > 0 {}
        if status == 429 {
            return Err(JobError::Failed(RATE_LIMITED.to_string()));
        }
        let mut elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(server_ms) = parse_server_timing_dur(&timing) {
            elapsed -= server_ms;
        }
        if i > 0 {
            times.push(elapsed.max(0.1));
        }
    }
    Ok((median(&times), jitter(&times)))
}

struct CountingReader {
    data: Arc<Vec<u8>>,
    pos: usize,
    total: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let n = buf.len().min(CHUNK).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.total.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

enum WorkerError {
    RateLimited,
    Other(String),
}

fn transfer(
    direction: &str,
    progress: &dyn Fn(&str, Option<f64>, f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<f64, JobError> {
    let total = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let errors: Arc<Mutex<Vec<WorkerError>>> = Arc::new(Mutex::new(Vec::new()));

    let payload = if direction == "up" {
        let mut buf = vec![0u8; UPLOAD_BYTES];
        let _ = getrandom::getrandom(&mut buf);
        Some(Arc::new(buf))
    } else {
        None
    };

    for _ in 0..STREAMS {
        let total = Arc::clone(&total);
        let stop = Arc::clone(&stop);
        let errors = Arc::clone(&errors);
        let payload = payload.clone();
        let is_down = direction == "down";
        std::thread::spawn(move || {
            let Ok(c) = client() else {
                errors.lock().unwrap().push(WorkerError::Other("client error".into()));
                return;
            };
            while !stop.load(Ordering::Relaxed) {
                let res: Result<(), WorkerError> = if is_down {
                    match c.get(format!("{HOST}/__down?bytes=25000000")).send() {
                        Ok(mut resp) => {
                            let status = resp.status().as_u16();
                            if status == 429 {
                                Err(WorkerError::RateLimited)
                            } else if status != 200 {
                                Err(WorkerError::Other(format!("server answered {status}")))
                            } else {
                                let mut buf = vec![0u8; CHUNK];
                                loop {
                                    if stop.load(Ordering::Relaxed) {
                                        break Ok(());
                                    }
                                    match resp.read(&mut buf) {
                                        Ok(0) => break Ok(()),
                                        Ok(n) => {
                                            total.fetch_add(n as u64, Ordering::Relaxed);
                                        }
                                        Err(e) => break Err(WorkerError::Other(e.to_string())),
                                    }
                                }
                            }
                        }
                        Err(e) => Err(WorkerError::Other(e.to_string())),
                    }
                } else {
                    let data = payload.as_ref().unwrap();
                    let len = data.len() as u64;
                    let reader = CountingReader {
                        data: Arc::clone(data),
                        pos: 0,
                        total: Arc::clone(&total),
                        stop: Arc::clone(&stop),
                    };
                    match c
                        .post(format!("{HOST}/__up"))
                        .header("Content-Type", "application/octet-stream")
                        .body(reqwest::blocking::Body::sized(reader, len))
                        .send()
                    {
                        Ok(mut resp) => {
                            let status = resp.status().as_u16();
                            let mut sink = [0u8; 1024];
                            while resp.read(&mut sink).unwrap_or(0) > 0 {}
                            if status == 429 {
                                Err(WorkerError::RateLimited)
                            } else {
                                Ok(())
                            }
                        }
                        Err(e) => {
                            if stop.load(Ordering::Relaxed) {
                                Ok(())
                            } else {
                                Err(WorkerError::Other(e.to_string()))
                            }
                        }
                    }
                };

                if let Err(err) = res {
                    let rate_limited = matches!(err, WorkerError::RateLimited);
                    errors.lock().unwrap().push(err);
                    if rate_limited {
                        stop.store(true, Ordering::Relaxed);
                    }
                    break;
                }
            }
        });
    }

    let start = Instant::now();
    let mut base_bytes = 0u64;
    let mut base_time: Option<Instant> = None;
    let mut samples = Vec::new();

    while start.elapsed().as_secs_f64() < PHASE_SECONDS {
        if cancelled() {
            stop.store(true, Ordering::Relaxed);
            return Err(JobError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(250));
        let now = Instant::now();
        let done = total.load(Ordering::Relaxed);
        let elapsed = now.duration_since(start).as_secs_f64();
        if base_time.is_none() && elapsed >= WARMUP_SECONDS {
            base_bytes = done;
            base_time = Some(now);
        }
        if let Some(bt) = base_time {
            let dt = now.duration_since(bt).as_secs_f64();
            if dt > 0.0 {
                let mbps = (done.saturating_sub(base_bytes)) as f64 * 8.0 / dt / 1e6;
                samples.push(mbps);
                progress(direction, Some(mbps), (elapsed / PHASE_SECONDS).min(1.0));
            }
        }
    }
    stop.store(true, Ordering::Relaxed);

    let errs = errors.lock().unwrap();
    if errs.iter().any(|e| matches!(e, WorkerError::RateLimited)) {
        return Err(JobError::Failed(RATE_LIMITED.to_string()));
    }
    let moved = total.load(Ordering::Relaxed);
    if samples.is_empty() || moved == 0 {
        let msg = if !errs.is_empty() {
            "No data got through. Check your internet connection."
        } else {
            "The test was too short."
        };
        return Err(JobError::Failed(msg.to_string()));
    }
    Ok(*samples.last().unwrap())
}

fn run_speed_test(
    progress: &dyn Fn(&str, Option<f64>, f64),
    cancelled: &dyn Fn() -> bool,
) -> Result<Value, JobError> {
    let server = server_info().unwrap_or_else(|_| json!({}));
    progress("ping", None, 0.0);
    let (ping, jit) = measure_ping(10, cancelled)?;
    progress("ping", Some(ping), 1.0);
    let download = transfer("down", progress, cancelled)?;
    let upload = transfer("up", progress, cancelled)?;
    Ok(json!({
        "server": server,
        "ping": ping,
        "jitter": jit,
        "download": download,
        "upload": upload,
    }))
}

pub fn dispatch(app: &AppHandle, method: &str, _args: &[Value]) -> Option<Value> {
    if method != "speed_test" {
        return None;
    }
    let id = app
        .state::<AppState>()
        .jobs
        .start(app, "speed", "Speed test", false, move |ctx| {
            let progress = |phase: &str, value: Option<f64>, fraction: f64| {
                push(
                    ctx.app(),
                    "speed",
                    json!({ "phase": phase, "value": value, "fraction": fraction }),
                );
            };
            let cancelled = || ctx.cancelled();
            let result = run_speed_test(&progress, &cancelled)?;
            ctx.check()?;
            push(ctx.app(), "speed-result", result);
            Ok(Vec::new())
        });
    Some(json!(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trace_info() {
        let v4 = parse_trace("fl=12f\nh=speed.cloudflare.com\nip=1.2.3.4\nloc=DE\ncolo=FRA\n");
        assert_eq!(v4["colo"], "FRA");
        assert_eq!(v4["city"], "Frankfurt");
        assert_eq!(v4["country"], "DE");
        assert_eq!(v4["ip_version"], "IPv4");

        let v6 = parse_trace("ip=2001:db8::1\ncolo=XYZ\n");
        assert_eq!(v6["colo"], "XYZ");
        assert_eq!(v6["city"], "XYZ");
        assert!(v6["country"].is_null());
        assert_eq!(v6["ip_version"], "IPv6");
    }

    #[test]
    fn parses_server_timing_dur() {
        assert_eq!(
            parse_server_timing_dur("cfRequestDuration;dur=12.5, other;dur=1"),
            Some(12.5)
        );
        assert_eq!(parse_server_timing_dur("cfL4;desc=\"?proto=TCP\", cfRequestDuration;dur=3.25"), Some(3.25));
        assert_eq!(parse_server_timing_dur("no-dur-here"), None);
        assert_eq!(parse_server_timing_dur("dur=abc"), None);
    }

    #[test]
    fn computes_median_and_jitter() {
        assert!((median(&[10.0, 2.0, 6.0]) - 6.0).abs() < 1e-9);
        assert!((median(&[10.0, 2.0, 4.0, 8.0]) - 6.0).abs() < 1e-9);
        assert!((jitter(&[10.0, 12.0, 16.0]) - 3.0).abs() < 1e-9);
        assert_eq!(jitter(&[10.0]), 0.0);
    }

    #[test]
    fn counting_reader_tracks_bytes_and_stops() {
        let data = Arc::new(vec![7u8; 100]);
        let total = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut reader = CountingReader {
            data,
            pos: 0,
            total: Arc::clone(&total),
            stop: Arc::clone(&stop),
        };
        let mut out = [0u8; 40];
        assert_eq!(reader.read(&mut out).unwrap(), 40);
        assert_eq!(total.load(Ordering::Relaxed), 40);

        stop.store(true, Ordering::Relaxed);
        assert!(reader.read(&mut out).is_err());
    }
}
