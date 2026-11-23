//! Scientific calculator helper: runs Source/toolkit/calc.py (SymPy) in its own helper
//! process over newline-delimited JSON on stdin/stdout, plus fast in-process unit conversions
//! and the always-on-top pop-out calculator window.
//!
//! Port of Api.calc / calc_popout / calc_convert / CalcWindowApi in Source/swiftlane.py.
//! Security & reliability rules:
//! - Only the 16 methods in CALC_METHODS may ever be called on Calculator.
//! - Each call has a 10 s time limit; a runaway sum kills the helper process so Swiftlane
//!   never freezes, and the next sum starts a fresh helper with the saved mode.
//! - The pop-out window ("calculator") may only call calculator + clipboard + pin methods.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Mutex, Once};
use std::time::Duration;
use tauri::{AppHandle, Manager};

use super::{arg, command, program, trimmer};
use crate::api::AppState;
use crate::config;

const TIME_LIMIT: Duration = Duration::from_secs(10);
const START_LIMIT: Duration = Duration::from_secs(60);

const CALC_METHODS: &[&str] = &[
    "evaluate",
    "solve",
    "set_mode",
    "memory",
    "memory_add",
    "clear_memory",
    "base_n",
    "statistics",
    "distribution",
    "polynomial",
    "simultaneous",
    "inequality",
    "set_matrix",
    "matrix_eval",
    "table",
    "ratio",
];

pub fn is_allowed_method(method: &str) -> bool {
    CALC_METHODS.contains(&method)
}

/// Embedded stdin/stdout JSON-lines driver around unmodified Source/toolkit/calc.py.
const PY_BRIDGE: &str = r#"
import json, sys
for s in (sys.stdin, sys.stdout):
    try:
        s.reconfigure(encoding="utf-8")
    except Exception:
        pass
if sys.platform == "win32":
    try:
        import ctypes
        ctypes.windll.kernel32.SetPriorityClass(ctypes.windll.kernel32.GetCurrentProcess(), 0x4000)
    except Exception:
        pass
sys.path.insert(0, sys.argv[1])
import calc
ALLOWED = {
    "evaluate", "solve", "set_mode", "memory", "memory_add", "clear_memory", "base_n",
    "statistics", "distribution", "polynomial", "simultaneous", "inequality",
    "set_matrix", "matrix_eval", "table", "ratio",
}
c = calc.Calculator()
if len(sys.argv) > 2 and sys.argv[2] in ("deg", "rad", "gra"):
    c.set_mode(angle=sys.argv[2])
print(json.dumps({"ready": True, "memory": c.memory()}), flush=True)
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
        method = req.get("method")
        args = req.get("args") or {}
        if method not in ALLOWED or not isinstance(args, dict):
            res = {"error": "Unknown"}
        else:
            res = getattr(c, method)(**args)
    except calc.CalcError as e:
        res = {"error": str(e)}
    except Exception:
        res = {"error": "Math ERROR"}
    print(json.dumps({"result": res, "memory": c.memory()}), flush=True)
"#;

// ------------------------------------------------------------------ unit conversions & ClassWiz formatting

const CONVERSIONS: &[(&str, &str, &str, Option<f64>)] = &[
    ("in_cm", "in", "cm", Some(2.54)),
    ("ft_m", "ft", "m", Some(0.3048)),
    ("yd_m", "yd", "m", Some(0.9144)),
    ("mile_km", "mile", "km", Some(1.609344)),
    ("nmile_m", "n mile", "m", Some(1852.0)),
    ("pc_km", "pc", "km", Some(3.0856775814913673e13)),
    ("acre_m2", "acre", "m²", Some(4046.8564224)),
    ("galUS_L", "gal (US)", "L", Some(3.785411784)),
    ("galUK_L", "gal (UK)", "L", Some(4.54609)),
    ("kmh_ms", "km/h", "m/s", Some(1.0 / 3.6)),
    ("oz_g", "oz", "g", Some(28.349523125)),
    ("lb_kg", "lb", "kg", Some(0.45359237)),
    ("atm_Pa", "atm", "Pa", Some(101325.0)),
    ("mmHg_Pa", "mmHg", "Pa", Some(133.322387415)),
    ("hp_kW", "hp", "kW", Some(0.745699872)),
    ("kgfcm2_Pa", "kgf/cm²", "Pa", Some(98066.5)),
    ("kgfm_J", "kgf·m", "J", Some(9.80665)),
    ("lbfin2_kPa", "lbf/in²", "kPa", Some(6.894757293168)),
    ("J_cal", "J", "cal", Some(1.0 / 4.184)),
    ("kWh_MJ", "kWh", "MJ", Some(3.6)),
    ("F_C", "°F", "°C", None),
];

fn superscript(n: i32) -> String {
    n.to_string()
        .chars()
        .map(|c| match c {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '-' => '⁻',
            other => other,
        })
        .collect()
}

fn format_sci(x: f64, sig_digits: usize) -> String {
    let prec = sig_digits.saturating_sub(1);
    let raw = format!("{x:.prec$e}");
    let (mant_raw, exp_raw) = raw.split_once('e').unwrap_or((&raw, "0"));
    let mant = if mant_raw.contains('.') {
        mant_raw.trim_end_matches('0').trim_end_matches('.')
    } else {
        mant_raw
    };
    let exp: i32 = exp_raw.parse().unwrap_or(0);
    format!("{mant}×10{}", superscript(exp))
}

/// ClassWiz Norm 1 formatting (10 significant digits), matching Python's `format_number(x, "norm1")`.
pub fn format_norm1(x: f64) -> Result<String, String> {
    if !x.is_finite() {
        return Err("Math ERROR".into());
    }
    if x == 0.0 {
        return Ok("0".into());
    }
    let abs = x.abs();
    if (1e-2..1e10).contains(&abs) {
        let exp = abs.log10().floor() as i32;
        let frac = (9 - exp).max(0) as usize;
        let s = format!("{x:.frac$}");
        let trimmed = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        };
        if trimmed == "-0" {
            return Ok("0".into());
        }
        return Ok(trimmed);
    }
    Ok(format_sci(x, 10))
}

fn parse_num(v: &Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        n.is_finite().then_some(n)
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<f64>().ok().filter(|n| n.is_finite())
    } else {
        None
    }
}

pub fn convert_unit(value: &Value, conv_id: &str, reverse: bool) -> Result<Value, String> {
    let &(.., a, b, factor) = CONVERSIONS
        .iter()
        .find(|c| c.0 == conv_id)
        .ok_or_else(|| "Unknown conversion".to_string())?;
    let v = parse_num(value).ok_or_else(|| "Math ERROR".to_string())?;
    let result = match factor {
        None => {
            if !reverse {
                (v - 32.0) * 5.0 / 9.0
            } else {
                v * 9.0 / 5.0 + 32.0
            }
        }
        Some(f) => {
            if !reverse {
                v * f
            } else {
                v / f
            }
        }
    };
    let formatted = format_norm1(result)?;
    Ok(json!({
        "value": formatted,
        "from": if reverse { b } else { a },
        "to": if reverse { a } else { b },
    }))
}

// ------------------------------------------------------------------ Python calculator helper process

struct HelperProc {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<String>,
}

struct CalcState {
    proc: Option<HelperProc>,
    angle: String,
    fmt: Option<String>,
    digits: Option<i64>,
    memory: Value,
}

impl Default for CalcState {
    fn default() -> Self {
        Self {
            proc: None,
            angle: "deg".into(),
            fmt: None,
            digits: None,
            memory: json!({
                "angle": "deg",
                "fmt": "norm1",
                "digits": 10,
                "vars": { "A": "0", "B": "0", "C": "0", "D": "0", "E": "0", "F": "0", "M": "0", "x": "0", "y": "0" },
                "ans": "0",
            }),
        }
    }
}

static CALC: Mutex<Option<CalcState>> = Mutex::new(None);

fn find_calc_dir() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in ["", "toolkit", "tools", "Source/toolkit"] {
                let cand = if rel.is_empty() {
                    dir.to_path_buf()
                } else {
                    dir.join(rel)
                };
                if cand.join("calc.py").is_file() {
                    return Some(cand);
                }
            }
            // Dev checkout fallback: walk up from target/debug to repo root
            for anc in dir.ancestors().take(6) {
                let cand = anc.join("Source").join("toolkit");
                if cand.join("calc.py").is_file() {
                    return Some(cand);
                }
            }
        }
    }
    let manifest_cand = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("Source").join("toolkit"));
    if let Some(c) = manifest_cand.filter(|p| p.join("calc.py").is_file()) {
        return Some(c);
    }
    None
}

fn spawn_helper(angle: &str) -> Result<(HelperProc, Option<Value>), String> {
    let mut cmd = if let Some(bin) = program("swiftlane-calc") {
        let mut c = command(&bin);
        c.arg(angle);
        c
    } else {
        let calc_dir = find_calc_dir()
            .ok_or_else(|| "Calculator helper (calc.py) wasn't found next to Swiftlane.".to_string())?;
        let py = program("python")
            .or_else(|| program("python3"))
            .ok_or_else(|| "The scientific calculator needs Python with SymPy (or swiftlane-calc.exe next to Swiftlane).".to_string())?;
        let mut c = command(&py);
        c.current_dir(&calc_dir)
            .args(["-P", "-E", "-u", "-c"])
            .arg(PY_BRIDGE)
            .arg(&calc_dir)
            .arg(angle);
        c
    };

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|_| "Couldn't start the calculator helper.".to_string())?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Couldn't connect to the calculator helper.".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Couldn't connect to the calculator helper.".to_string())?;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { break };
            if tx.send(l).is_err() {
                break;
            }
        }
    });

    match rx.recv_timeout(START_LIMIT) {
        Ok(line) => {
            let v: Value = serde_json::from_str(&line)
                .map_err(|_| "The calculator helper sent an unexpected answer.".to_string())?;
            if v["ready"].as_bool() != Some(true) {
                trimmer::kill(&mut child, true);
                return Err("The calculator helper couldn't start (is SymPy installed?).".into());
            }
            let mem = v.get("memory").cloned();
            Ok((HelperProc { child, stdin, rx }, mem))
        }
        Err(_) => {
            trimmer::kill(&mut child, true);
            Err("The calculator helper didn't start in time (is SymPy installed?).".into())
        }
    }
}

fn kill_proc(st: &mut CalcState) {
    if let Some(mut p) = st.proc.take() {
        drop(p.stdin);
        trimmer::kill(&mut p.child, true);
    }
}

fn send_request(p: &mut HelperProc, method: &str, args: &Value) -> Result<Value, String> {
    let line = json!({ "method": method, "args": args }).to_string();
    p.stdin
        .write_all(line.as_bytes())
        .and_then(|_| p.stdin.write_all(b"\n"))
        .and_then(|_| p.stdin.flush())
        .map_err(|_| "disconnected".to_string())?;
    let raw = p
        .rx
        .recv_timeout(TIME_LIMIT)
        .map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => "timeout".to_string(),
            mpsc::RecvTimeoutError::Disconnected => "disconnected".to_string(),
        })?;
    serde_json::from_str(&raw).map_err(|_| "bad_json".to_string())
}

fn call_calc(app: &AppHandle, method: &str, args: &Value) -> Value {
    if !is_allowed_method(method) {
        return json!({ "error": "Unknown" });
    }
    let clean_args = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };

    let cfg_angle = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("calc_angle")
        .and_then(Value::as_str)
        .unwrap_or("deg")
        .to_string();

    let mut guard = CALC.lock().unwrap();
    let st = guard.get_or_insert_with(|| {
        let mut s = CalcState::default();
        s.angle = cfg_angle.clone();
        s.memory["angle"] = json!(cfg_angle);
        s
    });

    if st.proc.is_none() {
        match spawn_helper(&st.angle) {
            Ok((mut p, mem)) => {
                if let Some(m) = mem {
                    st.memory = m;
                }
                // Re-apply format/digits if this is a restart after a timed-out sum.
                if st.fmt.is_some() || st.digits.is_some() {
                    let mut mode = serde_json::Map::new();
                    if let Some(f) = &st.fmt {
                        mode.insert("fmt".into(), json!(f));
                    }
                    if let Some(d) = st.digits {
                        mode.insert("digits".into(), json!(d));
                    }
                    let _ = send_request(&mut p, "set_mode", &Value::Object(mode));
                }
                st.proc = Some(p);
            }
            Err(e) => return json!({ "error": e }),
        }
    }

    let p = st.proc.as_mut().unwrap();
    let resp = send_request(p, method, &clean_args);
    match resp {
        Ok(v) => {
            if let Some(m) = v.get("memory") {
                st.memory = m.clone();
            }
            if method == "set_mode" {
                if let Some(a) = clean_args.get("angle").and_then(Value::as_str) {
                    if matches!(a, "deg" | "rad" | "gra") {
                        st.angle = a.to_string();
                        let state = app.state::<AppState>();
                        let mut cfg = state.cfg.lock().unwrap();
                        cfg["calc_angle"] = json!(a);
                        config::save(&cfg);
                    }
                }
                if let Some(f) = clean_args.get("fmt").and_then(Value::as_str) {
                    if matches!(f, "norm1" | "norm2" | "fix" | "sci") {
                        st.fmt = Some(f.to_string());
                    }
                }
                if let Some(d) = clean_args.get("digits").and_then(Value::as_i64) {
                    st.digits = Some(d);
                }
            }
            v.get("result").cloned().unwrap_or_else(|| json!({ "error": "Math ERROR" }))
        }
        Err(reason) => {
            kill_proc(st);
            if reason == "timeout" {
                json!({ "error": "Time limit: this one is too hard to work out" })
            } else {
                json!({ "error": "Math ERROR" })
            }
        }
    }
}

pub fn memory(app: &AppHandle) -> Value {
    let cfg_angle = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("calc_angle")
        .and_then(Value::as_str)
        .unwrap_or("deg")
        .to_string();
    let mut guard = CALC.lock().unwrap();
    let st = guard.get_or_insert_with(|| {
        let mut s = CalcState::default();
        s.angle = cfg_angle.clone();
        s.memory["angle"] = json!(cfg_angle);
        s
    });
    st.memory.clone()
}

// ------------------------------------------------------------------ pop-out calculator window

static WATCH_MAIN: Once = Once::new();

fn watch_main_window(app: &AppHandle) {
    WATCH_MAIN.call_once(|| {
        if let Some(main) = app.get_webview_window("main") {
            let app = app.clone();
            main.on_window_event(move |e| {
                if matches!(e, tauri::WindowEvent::Destroyed) {
                    if let Some(w) = app.get_webview_window("calculator") {
                        let _ = w.destroy();
                    }
                    if let Ok(mut g) = CALC.lock() {
                        if let Some(st) = g.as_mut() {
                            kill_proc(st);
                        }
                    }
                }
            });
        }
    });
}

fn calc_popout(app: &AppHandle) -> Value {
    let on_top = app
        .state::<AppState>()
        .cfg
        .lock()
        .unwrap()
        .get("calc_on_top")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if let Some(w) = app.get_webview_window("calculator") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = w.set_always_on_top(on_top);
        return json!({ "ok": true });
    }

    let built = tauri::WebviewWindowBuilder::new(
        app,
        "calculator",
        tauri::WebviewUrl::App("desktop.html".into()),
    )
    .title("Calculator · Swiftlane")
    .inner_size(440.0, 760.0)
    .min_inner_size(300.0, 420.0)
    .background_color(tauri::window::Color(7, 7, 13, 255))
    .always_on_top(on_top)
    .build();

    match built {
        Ok(_) => {
            watch_main_window(app);
            json!({ "ok": true })
        }
        Err(e) => {
            config::log(&format!("Couldn't open the calculator window: {e}"));
            json!({ "error": "Couldn't open the calculator window." })
        }
    }
}

/// Calls from the pop-out calculator window ("calculator"): only calculator, clipboard, and pin.
pub fn popout_call(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    let st = app.state::<AppState>();
    Some(match method {
        "get_state" => {
            let cfg = st.cfg.lock().unwrap().clone();
            let lists: Value = serde_json::from_str(crate::api::TOOL_LISTS).unwrap_or_else(|_| json!({}));
            json!({
                "popout": "calculator",
                "version": crate::api::VERSION,
                "features": { "calculator": true },
                "language": cfg.get("language").cloned().unwrap_or_else(|| json!("en")),
                "theme": cfg["theme"],
                "calc": memory(app),
                "calc_angle": cfg.get("calc_angle").cloned().unwrap_or_else(|| json!("deg")),
                "calc_on_top": cfg.get("calc_on_top").and_then(Value::as_bool).unwrap_or(true),
                "constants": lists["constants"],
                "conversions": lists["conversions"],
                "regressions": lists["regressions"],
                "sidebar": null,
                "start_page": null,
                "explorer": null,
                "update": null,
                "updated": null,
            })
        }
        "get_background" => crate::api::get_background(&st),
        "calc" => call_calc(app, arg(args, 0).as_str().unwrap_or(""), arg(args, 1)),
        "calc_convert" => {
            let conv_id = arg(args, 1).as_str().unwrap_or("");
            let reverse = arg(args, 2).as_bool().unwrap_or(false);
            match convert_unit(arg(args, 0), conv_id, reverse) {
                Ok(v) => v,
                Err(e) => json!({ "error": e }),
            }
        }
        "copy_text" => json!(crate::sharing::copy_text(arg(args, 0).as_str().unwrap_or(""))),
        "paste_text" => crate::sharing::paste_text(),
        "set_on_top" => {
            let on = arg(args, 0).as_bool().unwrap_or(true);
            {
                let mut cfg = st.cfg.lock().unwrap();
                cfg["calc_on_top"] = json!(on);
                config::save(&cfg);
            }
            if let Some(w) = app.get_webview_window("calculator") {
                let _ = w.set_always_on_top(on);
            }
            json!(on)
        }
        _ => return None,
    })
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "calc" => call_calc(app, arg(args, 0).as_str().unwrap_or(""), arg(args, 1)),
        "calc_popout" => calc_popout(app),
        "calc_convert" => {
            let conv_id = arg(args, 1).as_str().unwrap_or("");
            let reverse = arg(args, 2).as_bool().unwrap_or(false);
            match convert_unit(arg(args, 0), conv_id, reverse) {
                Ok(v) => v,
                Err(e) => json!({ "error": e }),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_units_and_formats_like_classwiz() {
        let r = convert_unit(&json!(10), "in_cm", false).unwrap();
        assert_eq!(r["value"], "25.4");
        assert_eq!(r["from"], "in");
        assert_eq!(r["to"], "cm");

        let rev = convert_unit(&json!(25.4), "in_cm", true).unwrap();
        assert_eq!(rev["value"], "10");
        assert_eq!(rev["from"], "cm");
        assert_eq!(rev["to"], "in");

        let c = convert_unit(&json!(32), "F_C", false).unwrap();
        assert_eq!(c["value"], "0");
        let f = convert_unit(&json!(100), "F_C", true).unwrap();
        assert_eq!(f["value"], "212");

        assert_eq!(format_norm1(1.5e-5).unwrap(), "1.5×10⁻⁵");
        assert_eq!(format_norm1(2.5e12).unwrap(), "2.5×10¹²");
        assert_eq!(format_norm1(0.0).unwrap(), "0");
        assert!(format_norm1(f64::NAN).is_err());

        assert!(convert_unit(&json!(1), "bogus", false).is_err());
        assert!(convert_unit(&json!("abc"), "in_cm", false).is_err());
    }

    #[test]
    fn validates_calc_method_allowlist() {
        assert!(is_allowed_method("evaluate"));
        assert!(is_allowed_method("solve"));
        assert!(is_allowed_method("matrix_eval"));
        assert!(!is_allowed_method("__import__"));
        assert!(!is_allowed_method("exec"));
        assert!(!is_allowed_method(""));
    }
}
