//! Native Rust packing & version management CLI for Swiftlane.
//!
//! Subcommands:
//!   swiftlane-pack version [X.Y.Z] [--check] [--root <dir>]
//!   swiftlane-pack payload <app_dir> <out_dir> [--version X.Y.Z]
//!   swiftlane-pack bundle <setup_stub.exe> <payload.zip> <version> <out_setup.exe>

use serde_json::Value;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const TRAILER_MAGIC: &[u8; 8] = b"SWLSETUP";
const TRAILER_VERSION_LEN: usize = 16;

const REQUIRED_FILES: &[&str] = &[
    "Swiftlane.exe",
    "ffmpeg.exe",
    "ffprobe.exe",
    "pdfium.dll",
    "yt-dlp.exe",
    "toolkit/calc.py",
    "uninstall.ps1",
    "airplay-install.ps1",
    "licenses/SOURCES.txt",
    "licenses/FluentUI-System-Icons-LICENSE.txt",
    "licenses/Geist-Font-OFL.txt",
];

const SKIP_NAMES: &[&str] = &["Swiftlane.exe.sha256", "SHA256SUMS.txt"];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        print_usage();
        return Err("Missing subcommand (expected: version, payload, or bundle).".into());
    };

    match cmd {
        "version" => cmd_version(&args[1..]),
        "payload" => cmd_payload(&args[1..]),
        "bundle" => cmd_bundle(&args[1..]),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        other => {
            print_usage();
            Err(format!("Unknown subcommand '{other}'."))
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n\
         \x20 swiftlane-pack version [X.Y.Z] [--check] [--root <dir>]\n\
         \x20 swiftlane-pack payload <app_dir> <out_dir> [--version X.Y.Z] [--root <dir>]\n\
         \x20 swiftlane-pack bundle <setup_stub.exe> <payload.zip> <version> <out_setup.exe>"
    );
}

fn is_valid_semver(v: &str) -> bool {
    let mut parts = v.split('.');
    let (Some(a), Some(b), Some(c), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && !c.is_empty()
        && a.bytes().all(|ch| ch.is_ascii_digit())
        && b.bytes().all(|ch| ch.is_ascii_digit())
        && c.bytes().all(|ch| ch.is_ascii_digit())
}

fn find_repo_root(override_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = override_root {
        return fs::canonicalize(root)
            .map_err(|e| format!("Invalid --root '{}': {e}", root.display()));
    }

    if let Ok(cwd) = env::current_dir() {
        if let Some(found) = walk_up_for_repo_root(&cwd) {
            return Ok(found);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            if let Some(found) = walk_up_for_repo_root(parent) {
                return Ok(found);
            }
        }
    }
    Err("Couldn't locate Swiftlane repository root (pass --root <dir>).".into())
}

fn walk_up_for_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join("Rust").join("Cargo.toml").is_file() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// Find the byte span `(start, end)` of the version value inside `[package]` of a `Cargo.toml`.
fn find_cargo_toml_version_span(text: &str) -> Option<(usize, usize)> {
    let mut in_package = false;
    let mut offset = 0usize;
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            in_package = trimmed == "[package]";
        } else if trimmed.starts_with("[[") {
            in_package = false;
        } else if in_package {
            if let Some((val_start, val_end)) = parse_toml_string_field_span(line, "version") {
                return Some((offset + val_start, offset + val_end));
            }
        }
        offset += segment.len();
    }
    None
}

fn parse_toml_string_field_span(line: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if !line[i..].starts_with(key) {
        return None;
    }
    i += key.len();
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'=' {
        return None;
    }
    i += 1;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
    let start = i + 1;
    let rel_end = line[start..].find('"')?;
    Some((start, start + rel_end))
}

fn read_cargo_toml_version(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let (s, e) = find_cargo_toml_version_span(&text)
        .ok_or_else(|| format!("Couldn't find [package] version in {}", path.display()))?;
    Ok(text[s..e].to_string())
}

fn write_cargo_toml_version(path: &Path, new_version: &str) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let (s, e) = find_cargo_toml_version_span(&text)
        .ok_or_else(|| format!("Couldn't update [package] version in {}", path.display()))?;
    let updated = format!("{}{}{}", &text[..s], new_version, &text[e..]);
    fs::write(path, updated)
        .map_err(|err| format!("Couldn't write {}: {err}", path.display()))
}

/// Find the byte span `(start, end)` of `version = "..."` in a `Cargo.lock` `[[package]]` entry
/// whose `name = "<pkg_name>"`.
fn find_cargo_lock_version_span(text: &str, pkg_name: &str) -> Option<(usize, usize)> {
    let mut in_pkg = false;
    let mut matched_name = false;
    let mut offset = 0usize;
    for segment in text.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_pkg = true;
            matched_name = false;
        } else if trimmed.starts_with('[') {
            in_pkg = false;
            matched_name = false;
        } else if in_pkg {
            if !matched_name {
                if let Some((ns, ne)) = parse_toml_string_field_span(line, "name") {
                    matched_name = &line[ns..ne] == pkg_name;
                }
            } else if let Some((vs, ve)) = parse_toml_string_field_span(line, "version") {
                return Some((offset + vs, offset + ve));
            }
        }
        offset += segment.len();
    }
    None
}

fn read_cargo_lock_version(path: &Path, pkg_name: &str) -> Result<Option<String>, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    Ok(find_cargo_lock_version_span(&text, pkg_name).map(|(s, e)| text[s..e].to_string()))
}

fn write_cargo_lock_version(
    path: &Path,
    pkg_name: &str,
    new_version: &str,
    required: bool,
) -> Result<bool, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let Some((s, e)) = find_cargo_lock_version_span(&text, pkg_name) else {
        if required {
            return Err(format!(
                "Couldn't find package '{pkg_name}' version in {}",
                path.display()
            ));
        }
        return Ok(false);
    };
    let updated = format!("{}{}{}", &text[..s], new_version, &text[e..]);
    fs::write(path, updated)
        .map_err(|err| format!("Couldn't write {}: {err}", path.display()))?;
    Ok(true)
}

fn find_json_version_span(text: &str) -> Option<(usize, usize)> {
    let key = "\"version\"";
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find(key) {
        let key_end = search_from + rel + key.len();
        let bytes = text.as_bytes();
        let mut i = key_end;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b':' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                let start = i + 1;
                if let Some(end_rel) = text[start..].find('"') {
                    return Some((start, start + end_rel));
                }
            }
        }
        search_from = key_end;
    }
    None
}

fn read_tauri_conf_version(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| format!("Invalid JSON in {}: {e}", path.display()))?;
    parsed
        .get("version")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| format!("Couldn't find version in {}", path.display()))
}

fn write_tauri_conf_version(path: &Path, new_version: &str) -> Result<(), String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let (s, e) = find_json_version_span(&text)
        .ok_or_else(|| format!("Couldn't update version in {}", path.display()))?;
    let updated = format!("{}{}{}", &text[..s], new_version, &text[e..]);
    let parsed: Value = serde_json::from_str(&updated)
        .map_err(|err| format!("Resulting JSON invalid for {}: {err}", path.display()))?;
    if parsed.get("version").and_then(Value::as_str) != Some(new_version) {
        return Err(format!(
            "Version replacement verification failed for {}",
            path.display()
        ));
    }
    fs::write(path, updated)
        .map_err(|err| format!("Couldn't write {}: {err}", path.display()))
}

fn cmd_version(args: &[String]) -> Result<(), String> {
    let mut new_version: Option<String> = None;
    let mut check = false;
    let mut root_override: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--check" => check = true,
            "--root" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "Missing path after --root.".to_string())?;
                root_override = Some(PathBuf::from(dir));
            }
            arg if arg.starts_with('-') => {
                return Err(format!("Unknown flag '{arg}' for version subcommand."));
            }
            val => {
                if new_version.is_some() {
                    return Err(format!("Unexpected extra argument '{val}'."));
                }
                new_version = Some(val.to_string());
            }
        }
        i += 1;
    }

    let root = find_repo_root(root_override.as_deref())?;
    let rust_cargo = root.join("Rust").join("Cargo.toml");
    let rust_lock = root.join("Rust").join("Cargo.lock");
    let rust_tauri = root.join("Rust").join("tauri.conf.json");
    let setup_cargo = root.join("Rust").join("setup").join("Cargo.toml");
    let setup_lock = root.join("Rust").join("setup").join("Cargo.lock");
    let setup_tauri = root.join("Rust").join("setup").join("tauri.conf.json");

    if let Some(ref ver) = new_version {
        if !is_valid_semver(ver) {
            return Err(format!(
                "Invalid version '{ver}': expected X.Y.Z (digits only, e.g. 2.3.3)."
            ));
        }
        write_cargo_toml_version(&rust_cargo, ver)?;
        write_cargo_lock_version(&rust_lock, "swiftlane", ver, true)?;
        let _ = write_cargo_lock_version(&rust_lock, "swiftlane-setup", ver, false)?;
        write_tauri_conf_version(&rust_tauri, ver)?;
        if setup_cargo.is_file() {
            write_cargo_toml_version(&setup_cargo, ver)?;
        }
        if setup_lock.is_file() {
            let _ = write_cargo_lock_version(&setup_lock, "swiftlane-setup", ver, false)?;
        }
        if setup_tauri.is_file() {
            write_tauri_conf_version(&setup_tauri, ver)?;
        }
    }

    let mut entries: Vec<(&str, String)> = Vec::new();
    entries.push(("Rust/Cargo.toml", read_cargo_toml_version(&rust_cargo)?));
    let lock_main = read_cargo_lock_version(&rust_lock, "swiftlane")?
        .ok_or_else(|| format!("Couldn't find swiftlane package version in {}", rust_lock.display()))?;
    entries.push(("Rust/Cargo.lock (swiftlane)", lock_main));
    if let Some(lock_setup) = read_cargo_lock_version(&rust_lock, "swiftlane-setup")? {
        entries.push(("Rust/Cargo.lock (swiftlane-setup)", lock_setup));
    }
    entries.push(("Rust/tauri.conf.json", read_tauri_conf_version(&rust_tauri)?));
    if setup_cargo.is_file() {
        entries.push(("Rust/setup/Cargo.toml", read_cargo_toml_version(&setup_cargo)?));
    }
    if setup_lock.is_file() {
        if let Some(v) = read_cargo_lock_version(&setup_lock, "swiftlane-setup")? {
            entries.push(("Rust/setup/Cargo.lock", v));
        }
    }
    if setup_tauri.is_file() {
        entries.push((
            "Rust/setup/tauri.conf.json",
            read_tauri_conf_version(&setup_tauri)?,
        ));
    }

    for (label, ver) in &entries {
        println!("{label:34} {ver}");
    }

    let first = &entries[0].1;
    let all_match = entries.iter().all(|(_, v)| v == first);
    if !all_match {
        if check {
            return Err("ERROR: Rust version files do not match.".into());
        }
        eprintln!("WARNING: Rust version files do not match.");
    }
    Ok(())
}

fn is_never_pack(rel_posix: &str) -> bool {
    let lower = rel_posix.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".log")
        || lower.ends_with(".pdb")
        || lower.contains("signing")
        || file_name == "config.json"
        || file_name == "shares.json"
        || file_name == "setup-choices.json"
        || lower.ends_with("config.json")
        || lower.ends_with("shares.json")
        || lower.ends_with("setup-choices.json")
}

fn collect_files_recursive(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let rd = fs::read_dir(dir)
        .map_err(|e| format!("Couldn't read directory {}: {e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("Directory entry error in {}: {e}", dir.display()))?;
        let path = entry.path();
        let meta = entry
            .metadata()
            .map_err(|e| format!("Metadata error for {}: {e}", path.display()))?;
        if meta.is_dir() {
            collect_files_recursive(base, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| format!("Relative path error for {}: {e}", path.display()))?;
            let rel_posix = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel_posix, path));
        }
    }
    Ok(())
}

fn pack_payload(app_dir: &Path, out_dir: &Path, version: &str) -> Result<(PathBuf, PathBuf, usize), String> {
    if !is_valid_semver(version) {
        return Err(format!("Invalid version '{version}': expected X.Y.Z."));
    }
    if !app_dir.is_dir() {
        return Err(format!("App folder doesn't exist: {}", app_dir.display()));
    }

    let mut missing = Vec::new();
    for rel in REQUIRED_FILES {
        let p = app_dir.join(rel);
        let ok = fs::metadata(&p)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !ok {
            missing.push(*rel);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Incomplete Rust app folder; missing or empty: {}",
            missing.join(", ")
        ));
    }

    let mut files = Vec::new();
    collect_files_recursive(app_dir, app_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Validate before creating output artifacts so a refused file leaves no partial zip.
    let mut to_pack = Vec::with_capacity(files.len());
    for (rel, path) in files {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if SKIP_NAMES.contains(&file_name) {
            continue;
        }
        if is_never_pack(&rel) {
            return Err(format!("Refusing to pack sensitive/debug file: {rel}"));
        }
        to_pack.push((rel, path));
    }

    fs::create_dir_all(out_dir)
        .map_err(|e| format!("Couldn't create output dir {}: {e}", out_dir.display()))?;
    let ver_path = out_dir.join("version.txt");
    let zip_path = out_dir.join("payload.zip");

    fs::write(&ver_path, version.trim())
        .map_err(|e| format!("Couldn't write {}: {e}", ver_path.display()))?;

    let zip_file = File::create(&zip_path)
        .map_err(|e| format!("Couldn't create {}: {e}", zip_path.display()))?;
    let mut zw = ZipWriter::new(BufWriter::new(zip_file));
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));

    let count = to_pack.len();
    for (rel, path) in to_pack {
        zw.start_file(&rel, opts)
            .map_err(|e| format!("Zip start_file failed for '{rel}': {e}"))?;
        let mut src = BufReader::new(
            File::open(&path)
                .map_err(|e| format!("Couldn't open {}: {e}", path.display()))?,
        );
        io::copy(&mut src, &mut zw)
            .map_err(|e| format!("Zip write failed for '{rel}': {e}"))?;
    }

    let mut inner = zw
        .finish()
        .map_err(|e| format!("Couldn't finalize payload.zip: {e}"))?;
    inner
        .flush()
        .map_err(|e| format!("Couldn't flush payload.zip: {e}"))?;
    drop(inner);

    let size = fs::metadata(&zip_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let mb = size as f64 / (1024.0 * 1024.0);
    println!("payload.zip: Swiftlane {version} (Rust), {count} files, {mb:.1} MB");
    Ok((zip_path, ver_path, count))
}

fn cmd_payload(args: &[String]) -> Result<(), String> {
    let mut positional: Vec<String> = Vec::new();
    let mut version_override: Option<String> = None;
    let mut root_override: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "Missing version after --version.".to_string())?;
                version_override = Some(v.clone());
            }
            "--root" => {
                i += 1;
                let r = args
                    .get(i)
                    .ok_or_else(|| "Missing directory after --root.".to_string())?;
                root_override = Some(PathBuf::from(r));
            }
            arg if arg.starts_with('-') => {
                return Err(format!("Unknown flag '{arg}' for payload subcommand."));
            }
            val => positional.push(val.to_string()),
        }
        i += 1;
    }

    if positional.len() != 2 {
        return Err("Usage: swiftlane-pack payload <app_dir> <out_dir> [--version X.Y.Z] [--root <dir>]".into());
    }

    let version = match version_override {
        Some(v) => v,
        None => {
            let root = find_repo_root(root_override.as_deref())?;
            read_cargo_toml_version(&root.join("Rust").join("Cargo.toml"))?
        }
    };

    pack_payload(Path::new(&positional[0]), Path::new(&positional[1]), &version)?;
    Ok(())
}

fn build_trailer(payload_offset: u64, payload_len: u64, version: &str) -> Result<[u8; 40], String> {
    if version.is_empty() {
        return Err("Version string must not be empty.".into());
    }
    let ver_bytes = version.as_bytes();
    if ver_bytes.len() > TRAILER_VERSION_LEN {
        return Err(format!(
            "Version '{version}' is {} bytes (max {TRAILER_VERSION_LEN} UTF-8 bytes).",
            ver_bytes.len()
        ));
    }

    let mut trailer = [0u8; 40];
    trailer[0..8].copy_from_slice(TRAILER_MAGIC);
    trailer[8..16].copy_from_slice(&payload_offset.to_le_bytes());
    trailer[16..24].copy_from_slice(&payload_len.to_le_bytes());
    trailer[24..24 + ver_bytes.len()].copy_from_slice(ver_bytes);
    Ok(trailer)
}

fn bundle_setup(
    stub_path: &Path,
    payload_path: &Path,
    version: &str,
    out_path: &Path,
) -> Result<u64, String> {
    let stub_meta = fs::metadata(stub_path)
        .map_err(|e| format!("Couldn't read setup stub {}: {e}", stub_path.display()))?;
    if !stub_meta.is_file() || stub_meta.len() == 0 {
        return Err(format!("Setup stub is missing or empty: {}", stub_path.display()));
    }
    let payload_meta = fs::metadata(payload_path)
        .map_err(|e| format!("Couldn't read payload {}: {e}", payload_path.display()))?;
    if !payload_meta.is_file() || payload_meta.len() == 0 {
        return Err(format!("Payload is missing or empty: {}", payload_path.display()));
    }

    let payload_offset = stub_meta.len();
    let payload_len = payload_meta.len();
    let trailer = build_trailer(payload_offset, payload_len, version)?;

    let out_dir = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("Couldn't create output directory {}: {e}", out_dir.display()))?;

    let file_name = out_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Swiftlane Setup.exe");
    let tmp_path = out_dir.join(format!(".{file_name}.tmp.{}", std::process::id()));

    let write_result = (|| -> Result<u64, String> {
        let out_file = File::create(&tmp_path)
            .map_err(|e| format!("Couldn't create temp file {}: {e}", tmp_path.display()))?;
        let mut writer = BufWriter::new(out_file);

        let mut stub_reader = BufReader::new(
            File::open(stub_path)
                .map_err(|e| format!("Couldn't open {}: {e}", stub_path.display()))?,
        );
        let copied_stub = io::copy(&mut stub_reader, &mut writer)
            .map_err(|e| format!("Failed copying setup stub: {e}"))?;
        if copied_stub != payload_offset {
            return Err(format!(
                "Setup stub size changed during copy (expected {payload_offset}, copied {copied_stub})."
            ));
        }

        let mut payload_reader = BufReader::new(
            File::open(payload_path)
                .map_err(|e| format!("Couldn't open {}: {e}", payload_path.display()))?,
        );
        let copied_payload = io::copy(&mut payload_reader, &mut writer)
            .map_err(|e| format!("Failed appending payload.zip: {e}"))?;
        if copied_payload != payload_len {
            return Err(format!(
                "Payload size changed during copy (expected {payload_len}, copied {copied_payload})."
            ));
        }

        writer
            .write_all(&trailer)
            .map_err(|e| format!("Failed writing 40-byte trailer: {e}"))?;
        writer
            .flush()
            .map_err(|e| format!("Failed flushing output file: {e}"))?;
        let inner = writer
            .into_inner()
            .map_err(|e| format!("Failed finalizing output buffer: {e}"))?;
        inner
            .sync_all()
            .map_err(|e| format!("Failed syncing temp file: {e}"))?;
        drop(inner);

        fs::rename(&tmp_path, out_path).map_err(|e| {
            format!(
                "Failed moving {} -> {}: {e}",
                tmp_path.display(),
                out_path.display()
            )
        })?;
        Ok(payload_offset + payload_len + 40)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    let total_size = write_result?;
    let mb = total_size as f64 / (1024.0 * 1024.0);
    println!("Bundled: {} ({mb:.1} MB)", out_path.display());
    Ok(total_size)
}

fn cmd_bundle(args: &[String]) -> Result<(), String> {
    if args.len() != 4 {
        return Err(
            "Usage: swiftlane-pack bundle <setup_stub.exe> <payload.zip> <version> <out_setup.exe>"
                .into(),
        );
    }
    bundle_setup(
        Path::new(&args[0]),
        Path::new(&args[1]),
        &args[2],
        Path::new(&args[3]),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = env::temp_dir().join(format!(
                "swiftlane-pack-test-{prefix}-{}-{n}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_mock_repo(root: &Path, ver: &str, with_setup: bool) {
        let rust = root.join("Rust");
        fs::create_dir_all(&rust).unwrap();
        fs::write(
            rust.join("Cargo.toml"),
            format!("[package]\nname = \"swiftlane\"\nversion = \"{ver}\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        let mut lock = format!(
            "version = 4\n\n[[package]]\nname = \"other\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"swiftlane\"\nversion = \"{ver}\"\n"
        );
        if with_setup {
            lock.push_str(&format!(
                "\n[[package]]\nname = \"swiftlane-setup\"\nversion = \"{ver}\"\n"
            ));
        }
        fs::write(rust.join("Cargo.lock"), lock).unwrap();
        fs::write(
            rust.join("tauri.conf.json"),
            format!("{{\n  \"productName\": \"Swiftlane\",\n  \"version\": \"{ver}\"\n}}\n"),
        )
        .unwrap();

        if with_setup {
            let setup = rust.join("setup");
            fs::create_dir_all(&setup).unwrap();
            fs::write(
                setup.join("Cargo.toml"),
                format!("[package]\nname = \"swiftlane-setup\"\nversion = \"{ver}\"\nedition = \"2021\"\n"),
            )
            .unwrap();
            fs::write(
                setup.join("tauri.conf.json"),
                format!("{{\n  \"productName\": \"Swiftlane Setup\",\n  \"version\": \"{ver}\"\n}}\n"),
            )
            .unwrap();
        }
    }

    fn populate_valid_app_dir(app_dir: &Path) {
        for rel in REQUIRED_FILES {
            let p = app_dir.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, format!("dummy content for {rel}")).unwrap();
        }
    }

    #[test]
    fn version_read_write_and_check_with_setup() {
        let tmp = TempDir::new("version");
        make_mock_repo(tmp.path(), "2.3.2", true);

        let root_str = tmp.path().to_string_lossy().into_owned();
        // Initial check passes
        cmd_version(&["--check".into(), "--root".into(), root_str.clone()]).unwrap();

        // Reject invalid semver
        assert!(cmd_version(&["2.3".into(), "--root".into(), root_str.clone()]).is_err());
        assert!(cmd_version(&["2.3.2-rc1".into(), "--root".into(), root_str.clone()]).is_err());

        // Bump to 2.4.0
        cmd_version(&[
            "2.4.0".into(),
            "--check".into(),
            "--root".into(),
            root_str.clone(),
        ])
        .unwrap();

        assert_eq!(
            read_cargo_toml_version(&tmp.path().join("Rust/Cargo.toml")).unwrap(),
            "2.4.0"
        );
        assert_eq!(
            read_cargo_lock_version(&tmp.path().join("Rust/Cargo.lock"), "swiftlane").unwrap(),
            Some("2.4.0".into())
        );
        assert_eq!(
            read_cargo_lock_version(&tmp.path().join("Rust/Cargo.lock"), "swiftlane-setup")
                .unwrap(),
            Some("2.4.0".into())
        );
        assert_eq!(
            read_tauri_conf_version(&tmp.path().join("Rust/tauri.conf.json")).unwrap(),
            "2.4.0"
        );
        assert_eq!(
            read_cargo_toml_version(&tmp.path().join("Rust/setup/Cargo.toml")).unwrap(),
            "2.4.0"
        );
        assert_eq!(
            read_tauri_conf_version(&tmp.path().join("Rust/setup/tauri.conf.json")).unwrap(),
            "2.4.0"
        );

        // Introduce mismatch in setup tauri.conf.json -> --check fails
        write_tauri_conf_version(&tmp.path().join("Rust/setup/tauri.conf.json"), "2.3.9").unwrap();
        assert!(cmd_version(&["--check".into(), "--root".into(), root_str]).is_err());
    }

    #[test]
    fn payload_packs_required_and_skips_checksums() {
        let tmp = TempDir::new("payload-ok");
        let app = tmp.path().join("app");
        let out = tmp.path().join("out");
        populate_valid_app_dir(&app);
        fs::write(app.join("Swiftlane.exe.sha256"), "abc").unwrap();
        fs::write(app.join("SHA256SUMS.txt"), "def").unwrap();

        let (zip_path, ver_path, count) = pack_payload(&app, &out, "2.3.2").unwrap();
        assert_eq!(count, REQUIRED_FILES.len());
        assert_eq!(fs::read_to_string(&ver_path).unwrap(), "2.3.2");

        let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        assert_eq!(archive.len(), REQUIRED_FILES.len());
        for rel in REQUIRED_FILES {
            let mut f = archive.by_name(rel).unwrap();
            let mut s = String::new();
            f.read_to_string(&mut s).unwrap();
            assert_eq!(s, format!("dummy content for {rel}"));
        }
        assert!(archive.by_name("Swiftlane.exe.sha256").is_err());
        assert!(archive.by_name("SHA256SUMS.txt").is_err());
    }

    #[test]
    fn payload_rejects_missing_or_empty_required_file() {
        let tmp = TempDir::new("payload-missing");
        let app = tmp.path().join("app");
        let out = tmp.path().join("out");
        populate_valid_app_dir(&app);
        fs::remove_file(app.join("licenses/Geist-Font-OFL.txt")).unwrap();

        let err = pack_payload(&app, &out, "2.3.2").unwrap_err();
        assert!(err.contains("licenses/Geist-Font-OFL.txt"), "err={err}");

        // Empty file also rejected
        fs::write(app.join("licenses/Geist-Font-OFL.txt"), b"").unwrap();
        let err = pack_payload(&app, &out, "2.3.2").unwrap_err();
        assert!(err.contains("licenses/Geist-Font-OFL.txt"), "err={err}");
    }

    #[test]
    fn payload_refuses_never_pack_patterns() {
        for bad_rel in [
            "secret.pem",
            "priv.KEY",
            "signing-note.txt",
            "sub/config.json",
            "shares.json",
            "setup-choices.json",
            "debug.log",
            "Swiftlane.pdb",
        ] {
            let tmp = TempDir::new("payload-bad");
            let app = tmp.path().join("app");
            let out = tmp.path().join("out");
            populate_valid_app_dir(&app);
            let bad_path = app.join(bad_rel);
            if let Some(parent) = bad_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&bad_path, b"sensitive").unwrap();

            let err = pack_payload(&app, &out, "2.3.2").unwrap_err();
            assert!(
                err.contains("Refusing to pack"),
                "expected refusal for {bad_rel}, got: {err}"
            );
            assert!(!out.join("payload.zip").exists());
        }
    }

    #[test]
    fn bundle_writes_exact_40_byte_trailer_and_replaces_existing() {
        let tmp = TempDir::new("bundle");
        let stub = tmp.path().join("swiftlane-setup.exe");
        let payload = tmp.path().join("payload.zip");
        let out = tmp.path().join("Swiftlane Setup.exe");

        let stub_bytes = b"MZ\x90\x00FAKE_STUB_BINARY_BYTES";
        let payload_bytes = b"PK\x03\x04FAKE_ZIP_PAYLOAD_DATA_1234567890";
        fs::write(&stub, stub_bytes).unwrap();
        fs::write(&payload, payload_bytes).unwrap();
        fs::write(&out, b"old setup file").unwrap();

        let total = bundle_setup(&stub, &payload, "2.3.2", &out).unwrap();
        assert_eq!(total, (stub_bytes.len() + payload_bytes.len() + 40) as u64);

        let data = fs::read(&out).unwrap();
        assert_eq!(data.len(), total as usize);
        assert_eq!(&data[..stub_bytes.len()], stub_bytes);
        let trailer = &data[data.len() - 40..];
        assert_eq!(&trailer[0..8], b"SWLSETUP");
        let offset = u64::from_le_bytes(trailer[8..16].try_into().unwrap());
        let len = u64::from_le_bytes(trailer[16..24].try_into().unwrap());
        assert_eq!(offset, stub_bytes.len() as u64);
        assert_eq!(len, payload_bytes.len() as u64);
        assert_eq!(
            &data[offset as usize..(offset + len) as usize],
            payload_bytes
        );

        let mut expected_ver = [0u8; 16];
        expected_ver[..5].copy_from_slice(b"2.3.2");
        assert_eq!(&trailer[24..40], &expected_ver);

        // Reject version > 16 bytes
        assert!(bundle_setup(&stub, &payload, "12345678901234567", &out).is_err());
    }
}
