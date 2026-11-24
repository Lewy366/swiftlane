//! BIOS update USB: detect the motherboard, inspect a downloaded BIOS archive or image, stage it
//! safely without Zip-Slip or zip-bomb risks, check USB drives, copy to FAT32, verify with an
//! unbuffered hardware read-back (FILE_FLAG_NO_BUFFERING), and safely eject without admin.
//!
//! Page calls:
//!   biosusb_detect(), biosusb_open_url(url), biosusb_open_diskmgmt(), biosusb_pick_file(),
//!   biosusb_inspect(path, board_vendor?, board_model?, entry_name?),
//!   biosusb_drives(), biosusb_format(letter), biosusb_prepare(opts), biosusb_eject(drive).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

use super::arg;

const MIN_BIOS_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB
const WARN_BIOS_BYTES: u64 = 65 * 1024 * 1024; // 65 MiB
const MAX_BIOS_BYTES: u64 = 130 * 1024 * 1024; // 130 MiB
const HEAD_INSPECT_BYTES: usize = 256 * 1024; // 256 KiB
const FREE_SPACE_MARGIN: u64 = 1_048_576; // 1 MiB safety headroom on FAT32
const MAX_STAGED_KEEP: usize = 4;

const SPI_FLASH_SIZES: &[u64] = &[
    4 * 1024 * 1024,
    8 * 1024 * 1024,
    16 * 1024 * 1024,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
    128 * 1024 * 1024,
];
const SPI_SIZE_TOLERANCE: u64 = 64 * 1024;

const JUNK_EXTS: &[&str] = &[
    "txt", "pdf", "exe", "bat", "cmd", "dll", "html", "htm", "url", "md", "ini", "inf", "cat",
    "sys", "png", "jpg", "jpeg", "gif", "docx", "rtf", "nfo", "log", "msi", "ps1", "vbs", "sh",
    "com", "scr", "xml", "json", "csv", "xls", "xlsx", "doc", "bmp", "ico", "svg", "webp",
];

#[derive(Clone, Copy)]
struct VendorMeta {
    id: &'static str,
    label: &'static str,
    support_url: Option<&'static str>,
    in_bios_tool: &'static str,
    flashback_name: Option<&'static str>,
    flashback_filename: Option<&'static str>,
    bios_key: &'static str,
}

const DIY_VENDORS: &[VendorMeta] = &[
    VendorMeta {
        id: "asus",
        label: "ASUS",
        support_url: Some("https://www.asus.com/support/download-center/"),
        in_bios_tool: "EZ Flash 3",
        flashback_name: Some("BIOS FlashBack"),
        flashback_filename: None,
        bios_key: "Del or F2",
    },
    VendorMeta {
        id: "msi",
        label: "MSI",
        support_url: Some("https://www.msi.com/support/download"),
        in_bios_tool: "M-FLASH",
        flashback_name: Some("Flash BIOS Button"),
        flashback_filename: Some("MSI.ROM"),
        bios_key: "Del",
    },
    VendorMeta {
        id: "gigabyte",
        label: "Gigabyte",
        support_url: Some("https://www.gigabyte.com/Support/Motherboard"),
        in_bios_tool: "Q-Flash",
        flashback_name: Some("Q-Flash Plus"),
        flashback_filename: Some("GIGABYTE.bin"),
        bios_key: "Del",
    },
    VendorMeta {
        id: "asrock",
        label: "ASRock",
        support_url: Some("https://www.asrock.com/support/index.asp?cat=BIOS"),
        in_bios_tool: "Instant Flash",
        flashback_name: Some("BIOS Flashback"),
        flashback_filename: Some("CREATIVE.ROM"),
        bios_key: "F2 or Del",
    },
    VendorMeta {
        id: "biostar",
        label: "Biostar",
        support_url: Some("https://www.biostar.com.tw/app/en/support/download.php"),
        in_bios_tool: "BIOS-FLASHER",
        flashback_name: Some("Smart BIOS Update"),
        flashback_filename: None,
        bios_key: "Del",
    },
];

const OEM_VENDORS: &[VendorMeta] = &[
    VendorMeta {
        id: "dell",
        label: "Dell",
        support_url: Some("https://www.dell.com/support/home"),
        in_bios_tool: "BIOS Flash Update (F12)",
        flashback_name: None,
        flashback_filename: None,
        bios_key: "F2 or F12",
    },
    VendorMeta {
        id: "hp",
        label: "HP",
        support_url: Some("https://support.hp.com/drivers"),
        in_bios_tool: "HP BIOS Update",
        flashback_name: None,
        flashback_filename: None,
        bios_key: "F10 or Esc",
    },
    VendorMeta {
        id: "lenovo",
        label: "Lenovo",
        support_url: Some("https://pcsupport.lenovo.com/"),
        in_bios_tool: "Lenovo BIOS Update",
        flashback_name: None,
        flashback_filename: None,
        bios_key: "F1 or F2",
    },
];

const OTHER_VENDOR: VendorMeta = VendorMeta {
    id: "other",
    label: "Other",
    support_url: None,
    in_bios_tool: "Built-in BIOS updater",
    flashback_name: None,
    flashback_filename: None,
    bios_key: "Del or F2",
};

const ALLOWED_SUPPORT_URLS: &[&str] = &[
    "https://www.asus.com/support/download-center/",
    "https://www.msi.com/support/download",
    "https://www.gigabyte.com/Support/Motherboard",
    "https://www.asrock.com/support/index.asp?cat=BIOS",
    "https://www.biostar.com.tw/app/en/support/download.php",
    "https://www.dell.com/support/home",
    "https://support.hp.com/drivers",
    "https://pcsupport.lenovo.com/",
];

#[derive(Clone, Debug)]
struct StagedBios {
    path: PathBuf,
    entry_name: String,
    size: u64,
    sha256: String,
    order: u64,
}

#[derive(Default)]
struct StagedStore {
    next_order: u64,
    items: HashMap<String, StagedBios>,
}

static STAGED: OnceLock<Mutex<StagedStore>> = OnceLock::new();

fn staged_store() -> &'static Mutex<StagedStore> {
    STAGED.get_or_init(|| Mutex::new(StagedStore::default()))
}

fn biosusb_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Swiftlane")
        .join("biosusb")
}

/// Removes Swiftlane's private BIOS USB staging folder and forgets staged tokens.
/// Called at startup and exit.
pub fn wipe_temp() {
    if let Ok(mut store) = staged_store().lock() {
        store.items.clear();
    }
    let dir = biosusb_dir();
    if dir.file_name().is_some_and(|n| n == "biosusb") {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "biosusb_detect" => detect(),
        "biosusb_open_url" => {
            let url = arg(args, 0).as_str().unwrap_or("");
            match open_allowed_url(url) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "error": e }),
            }
        }
        "biosusb_open_diskmgmt" => match open_diskmgmt() {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "error": e }),
        },
        "biosusb_pick_file" => pick_file(app),
        "biosusb_inspect" => {
            let path = arg(args, 0).as_str().unwrap_or("");
            let board_vendor = arg(args, 1).as_str().filter(|s| !s.trim().is_empty());
            let board_model = arg(args, 2).as_str().filter(|s| !s.trim().is_empty());
            let entry_index = arg(args, 3).as_u64().map(|n| n as usize);
            let p = Path::new(path);
            if let Err(e) = check_path_syntax(p) {
                return Some(json!({ "error": e }));
            }
            if !was_offered(p) {
                return Some(json!({
                    "error": "Pick the file with the Browse button or drop it onto Swiftlane."
                }));
            }
            match inspect(p, board_vendor, board_model, entry_index) {
                Ok(v) => v,
                Err(e) => json!({ "error": e }),
            }
        }
        "biosusb_drives" => list_drives(),
        "biosusb_format" => {
            let letter = arg(args, 0).as_str().unwrap_or("");
            match format_drive(app, letter) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "error": e }),
            }
        }
        "biosusb_prepare" => match prepare(arg(args, 0)) {
            Ok(v) => v,
            Err(e) => json!({ "error": e }),
        },
        "biosusb_eject" => {
            let drive = arg(args, 0).as_str().unwrap_or("");
            match eject_drive(drive) {
                Ok(also) => {
                    let also: Vec<String> = also.iter().map(|c| format!("{c}:")).collect();
                    json!({ "ok": true, "also_ejected": also })
                }
                Err(e) => json!({ "error": e }),
            }
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ vendor & board detection

fn vendor_meta(id: &str) -> VendorMeta {
    DIY_VENDORS
        .iter()
        .chain(OEM_VENDORS.iter())
        .copied()
        .find(|v| v.id == id)
        .unwrap_or(OTHER_VENDOR)
}

fn is_placeholder_text(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    lower.is_empty()
        || matches!(
            lower.as_str(),
            "default string"
                | "to be filled by o.e.m."
                | "to be filled by o.e.m"
                | "system manufacturer"
                | "system product name"
                | "baseboard manufacturer"
                | "not specified"
                | "none"
                | "n/a"
                | "unknown"
        )
}

fn map_vendor(maker: &str, system_maker: &str) -> &'static str {
    let pick = |raw: &str| -> Option<&'static str> {
        let s = raw.trim().to_ascii_lowercase();
        if s.is_empty() || is_placeholder_text(&s) {
            return None;
        }
        if s.contains("asus") {
            Some("asus")
        } else if s.contains("msi") || s.contains("micro-star") || s.contains("microstar") {
            Some("msi")
        } else if s.contains("gigabyte") || s.contains("aorus") {
            Some("gigabyte")
        } else if s.contains("asrock") {
            Some("asrock")
        } else if s.contains("biostar") {
            Some("biostar")
        } else if s.contains("dell") || s.contains("alienware") {
            Some("dell")
        } else if s.contains("hewlett-packard")
            || s == "hp"
            || s.starts_with("hp ")
            || s.contains("hp inc")
        {
            Some("hp")
        } else if s.contains("lenovo") || s.contains("ibm") {
            Some("lenovo")
        } else {
            None
        }
    };
    pick(maker).or_else(|| pick(system_maker)).unwrap_or("other")
}

fn is_revision_uncertain(revision: &str, vendor: &str) -> bool {
    if vendor == "gigabyte" {
        return true;
    }
    let lower = revision.trim().to_ascii_lowercase();
    lower.is_empty()
        || matches!(
            lower.as_str(),
            "default string"
                | "to be filled by o.e.m."
                | "to be filled by o.e.m"
                | "x.x"
                | "1.xx"
                | "rev 1.xx"
                | "rev x.0x"
                | "n/a"
                | "none"
                | "not specified"
                | "unknown"
                | "0"
                | "0.0"
        )
}

fn is_oem_maker_hint(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    if lower.is_empty() || is_placeholder_text(&lower) {
        return false;
    }
    const OEM_BRANDS: &[&str] = &[
        "dell",
        "alienware",
        "hewlett-packard",
        "hp inc",
        "lenovo",
        "microsoft",
        "apple",
        "acer",
        "razer",
        "framework",
        "samsung",
        "lg electronics",
        "huawei",
    ];
    lower == "hp"
        || lower.starts_with("hp ")
        || lower == "lg"
        || lower.starts_with("lg ")
        || OEM_BRANDS.iter().any(|b| lower.contains(b))
}

fn is_laptop_product_hint(product: &str) -> bool {
    let lower = product.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    const HINTS: &[&str] = &[
        "zenbook",
        "vivobook",
        "zephyrus",
        "strix g",
        "tuf gaming a15",
        "tuf gaming f15",
        "tuf gaming a16",
        "tuf gaming f16",
        "tuf gaming a17",
        "tuf gaming f17",
        "tuf dash",
        "rog flow",
        "rog ally",
        "expertbook",
        "studiobook",
        "thinkpad",
        "ideapad",
        "legion ",
        "yoga ",
        "macbook",
        "surface laptop",
        "surface book",
        "surface pro",
        "xps ",
        "latitude",
        "inspiron",
        "spectre",
        "envy",
        "elitebook",
        "probook",
        "victus",
        "razer blade",
        "matebook",
        "gram ",
        "swift ",
        "predator helios",
        "nitro 5",
    ];
    HINTS.iter().any(|h| lower.contains(h))
}

#[derive(Default)]
struct RawBoardInfo {
    maker: String,
    model: String,
    revision: String,
    bios_version: String,
    bios_date: String,
    system_maker: String,
    system_product: String,
}

fn detect() -> Value {
    let raw = platform::read_board_info();
    let vendor = map_vendor(&raw.maker, &raw.system_maker);
    let meta = vendor_meta(vendor);
    let has_battery = platform::has_battery();
    let product_laptop =
        is_laptop_product_hint(&raw.system_product) || is_laptop_product_hint(&raw.model);
    let is_laptop = has_battery || product_laptop;
    let is_oem_prebuilt = is_oem_maker_hint(&raw.maker)
        || is_oem_maker_hint(&raw.system_maker)
        || product_laptop;
    let revision_uncertain = is_revision_uncertain(&raw.revision, vendor);
    let (bl_status, bl_raw) = platform::detect_bitlocker();

    let vendors: Vec<Value> = DIY_VENDORS
        .iter()
        .map(|v| {
            json!({
                "id": v.id,
                "label": v.label,
                "support_url": v.support_url,
                "in_bios_tool": v.in_bios_tool,
                "flashback_name": v.flashback_name,
                "flashback_filename": v.flashback_filename,
                "bios_key": v.bios_key,
            })
        })
        .collect();

    json!({
        "maker": raw.maker,
        "vendor": vendor,
        "vendor_label": meta.label,
        "model": raw.model,
        "revision": raw.revision,
        "revision_uncertain": revision_uncertain,
        "bios_version": raw.bios_version,
        "bios_date": raw.bios_date,
        "system_maker": raw.system_maker,
        "system_product": raw.system_product,
        "is_laptop": is_laptop,
        "is_oem_prebuilt": is_oem_prebuilt,
        "bitlocker": { "status": bl_status, "raw": bl_raw },
        "support_url": meta.support_url,
        "in_bios_tool": meta.in_bios_tool,
        "flashback_name": meta.flashback_name,
        "flashback_filename": meta.flashback_filename,
        "bios_key": meta.bios_key,
        "vendors": vendors,
    })
}

// ------------------------------------------------------------------ URL & Disk Management

fn open_allowed_url(url: &str) -> Result<(), String> {
    if !ALLOWED_SUPPORT_URLS.contains(&url) {
        return Err("Only official motherboard support links can be opened from here.".into());
    }
    platform::shell_open_str(url)
}

fn open_diskmgmt() -> Result<(), String> {
    platform::open_diskmgmt()
}

const MAX_OFFERED_PATHS: usize = 32;
const MAX_ZIP_ENTRIES: usize = 10_000;

static OFFERED: OnceLock<Mutex<VecDeque<PathBuf>>> = OnceLock::new();

fn offered_store() -> &'static Mutex<VecDeque<PathBuf>> {
    OFFERED.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Remembers a path that Rust itself handed out (file picker or native drop event). The page
/// can only ask to inspect paths remembered here. The list is small and process-local.
pub fn offer_path(path: &Path) {
    if let Ok(mut q) = offered_store().lock() {
        q.retain(|p| p != path);
        q.push_back(path.to_path_buf());
        while q.len() > MAX_OFFERED_PATHS {
            q.pop_front();
        }
    }
}

pub(crate) fn was_offered(path: &Path) -> bool {
    offered_store().lock().is_ok_and(|q| q.iter().any(|p| p == path))
}

/// Rejects network (UNC) and device paths, which are never a downloaded BIOS file.
fn check_path_syntax(path: &Path) -> Result<(), String> {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\") || s.starts_with("//") {
        return Err("Network and device paths aren't supported. Copy the file to a local folder first.".into());
    }
    Ok(())
}

fn pick_file(app: &AppHandle) -> Value {
    let picked = app
        .dialog()
        .file()
        .add_filter(
            "BIOS archives & images",
            &["zip", "cap", "bin", "rom", "bio", "fd", "wph", "efi"],
        )
        .add_filter("All files", &["*"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok());
    match picked {
        Some(path) => {
            if let Err(e) = check_path_syntax(&path) {
                return json!({ "error": e });
            }
            offer_path(&path);
            json!({ "path": path.to_string_lossy() })
        }
        None => json!({ "cancelled": true }),
    }
}

// ------------------------------------------------------------------ BIOS file & ZIP inspection

fn random_hex_token() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|_| "Couldn't generate a staging token.".to_string())?;
    Ok(hex::encode(b))
}

fn has_zip_slip(raw_name: &str) -> bool {
    if raw_name.is_empty()
        || raw_name.starts_with('/')
        || raw_name.starts_with('\\')
        || raw_name.contains(':')
        || raw_name.contains('\0')
    {
        return true;
    }
    raw_name.split(['/', '\\']).any(|seg| seg == "..")
}

fn is_macos_junk(raw_name: &str, basename: &str) -> bool {
    raw_name
        .split(['/', '\\'])
        .any(|seg| seg.eq_ignore_ascii_case("__MACOSX"))
        || basename.starts_with("._")
}

fn is_junk_extension(filename: &str) -> bool {
    let Some(ext) = Path::new(filename).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lower = ext.to_ascii_lowercase();
    JUNK_EXTS.contains(&lower.as_str())
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Soft heuristic: looks for common UEFI / AMI / Intel flash signatures in the first 256 KB
/// or a file size within 64 KiB of a standard power-of-2 SPI flash chip size (4..128 MiB).
pub fn looks_like_bios(head: &[u8], size: u64) -> bool {
    // AMI Aptio UEFI capsule GUID: 4A3CA68B-7723-48FB-803D-578CC1FEC44D (little-endian)
    const APTIO_CAPSULE_GUID: &[u8] = &[
        0x8B, 0xA6, 0x3C, 0x4A, 0x23, 0x77, 0xFB, 0x48, 0x80, 0x3D, 0x57, 0x8C, 0xC1, 0xFE, 0xC4,
        0x4D,
    ];
    // Intel SPI Flash Descriptor signature (0x0FF0A55A in little-endian)
    const INTEL_FLASH_DESC: &[u8] = &[0x5A, 0xA5, 0xF0, 0x0F];

    if contains_subslice(head, b"_FVH")
        || contains_subslice(head, b"$FID")
        || contains_subslice(head, b"APTIO")
        || contains_subslice(head, b"AMIBIOS")
        || contains_subslice(head, APTIO_CAPSULE_GUID)
        || contains_subslice(head, INTEL_FLASH_DESC)
    {
        return true;
    }

    SPI_FLASH_SIZES
        .iter()
        .any(|&target| size.abs_diff(target) <= SPI_SIZE_TOLERANCE)
}

fn extract_vendor_from_filename(upper: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let has_tok = |list: &[&str]| tokens.iter().any(|t| list.contains(t));
    let has_sub = |list: &[&str]| list.iter().any(|s| upper.contains(s));

    if has_tok(&["ASUS", "ROG", "STRIX", "TUF", "PRIME", "CROSSHAIR", "MAXIMUS", "PROART"])
        || has_sub(&["ASUS"])
    {
        return Some("asus");
    }
    if has_tok(&["MSI", "TOMAHAWK", "MORTAR", "GODLIKE", "UNIFY", "CARBON", "BAZOOKA", "TORPEDO"])
        || has_sub(&["MSI"])
    {
        return Some("msi");
    }
    if has_tok(&["GIGABYTE", "AORUS"]) || has_sub(&["GIGABYTE", "AORUS"]) {
        return Some("gigabyte");
    }
    if has_tok(&["ASROCK", "TAICHI", "RIPTIDE", "LIVEMIXER", "VELOCITA"])
        || has_sub(&["ASROCK", "STEELLEGEND"])
    {
        return Some("asrock");
    }
    if has_tok(&["BIOSTAR", "VALKYRIE"]) || has_sub(&["BIOSTAR"]) {
        return Some("biostar");
    }
    None
}

const KNOWN_CHIPSETS: &[&str] = &[
    // AMD
    "A320", "B350", "X370", "A520", "B450", "X470", "B550", "X570", "A620", "B650", "X670",
    "B840", "B850", "X870",
    // Intel
    "H110", "B150", "H170", "Z170", "B250", "H270", "Z270", "H310", "B360", "B365", "H370",
    "Z370", "Z390", "H410", "B460", "H470", "Z490", "H510", "B560", "H570", "Z590", "H610",
    "B660", "H670", "Z690", "B760", "H770", "Z790", "H810", "B860", "Z890",
];

fn extract_chipset(text: &str) -> Option<&'static str> {
    let bytes = text.as_bytes();
    let upper = text.to_ascii_uppercase();
    let ub = upper.as_bytes();
    if ub.len() < 4 {
        return None;
    }
    for i in 0..=ub.len() - 4 {
        let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !prev_ok {
            continue;
        }
        let slice = &upper[i..i + 4];
        if let Some(&chip) = KNOWN_CHIPSETS.iter().find(|&&c| c == slice) {
            // Allow end-of-string, non-alphanumeric, or single board suffix letter (M/I/E/S/A/P)
            // followed by non-digit so e.g. B650M, B650E, Z790I match, but B6500 does not.
            let next_ok = match ub.get(i + 4) {
                None => true,
                Some(&b) if !b.is_ascii_alphanumeric() => true,
                Some(&b) if matches!(b, b'M' | b'I' | b'E' | b'S' | b'A' | b'P') => {
                    !ub.get(i + 5).is_some_and(u8::is_ascii_digit)
                }
                _ => false,
            };
            if next_ok {
                return Some(chip);
            }
        }
    }
    None
}

/// Soft warning when the BIOS filename clearly points at a different motherboard vendor or chipset.
pub fn detect_board_mismatch(
    source_name: &str,
    entry_name: &str,
    board_vendor: Option<&str>,
    board_model: Option<&str>,
) -> Option<String> {
    let combined = format!("{source_name} {entry_name}").to_ascii_uppercase();

    if let Some(bv) = board_vendor.map(|s| s.trim().to_ascii_lowercase()) {
        if DIY_VENDORS.iter().any(|v| v.id == bv) {
            if let Some(fv) = extract_vendor_from_filename(&combined) {
                if fv != bv {
                    return Some(format!(
                        "This file looks like a {} BIOS, but your board is {}. Double-check before flashing.",
                        vendor_meta(fv).label,
                        vendor_meta(&bv).label
                    ));
                }
            }
        }
    }

    if let Some(model) = board_model {
        if let (Some(board_chip), Some(file_chip)) =
            (extract_chipset(model), extract_chipset(&combined))
        {
            if board_chip != file_chip {
                return Some(format!(
                    "This file mentions {file_chip}, but your board model mentions {board_chip}. Make sure it matches your exact motherboard."
                ));
            }
        }
    }

    None
}

fn stream_to_staged<R: Read>(
    reader: R,
    staging_dir: &Path,
) -> Result<(String, PathBuf, u64, String, Vec<u8>), String> {
    std::fs::create_dir_all(staging_dir)
        .map_err(|e| format!("Couldn't create staging folder ({e})."))?;
    let token = random_hex_token()?;
    let dest_path = staging_dir.join(format!("{token}.bin"));

    let res = (|| -> Result<(u64, String, Vec<u8>), String> {
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest_path)
            .map_err(|e| format!("Couldn't create staged BIOS file ({e})."))?;
        let mut limited = reader.take(MAX_BIOS_BYTES + 1);
        let mut hasher = Sha256::new();
        let mut head = Vec::with_capacity(HEAD_INSPECT_BYTES);
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;

        loop {
            let n = limited
                .read(&mut buf)
                .map_err(|e| format!("Couldn't read the BIOS file ({e})."))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_BIOS_BYTES {
                return Err(
                    "The extracted file exceeded 130 MB and was stopped (too large for a motherboard BIOS)."
                        .into(),
                );
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("Couldn't write staged BIOS file ({e})."))?;
            hasher.update(&buf[..n]);
            if head.len() < HEAD_INSPECT_BYTES {
                let take = (HEAD_INSPECT_BYTES - head.len()).min(n);
                head.extend_from_slice(&buf[..take]);
            }
        }
        out.flush()
            .map_err(|e| format!("Couldn't flush staged BIOS file ({e})."))?;
        if total < MIN_BIOS_BYTES {
            return Err(format!(
                "This file is only {:.2} MB. Motherboard BIOS images are at least 4 MB.",
                total as f64 / (1024.0 * 1024.0)
            ));
        }
        Ok((total, hex::encode(hasher.finalize()), head))
    })();

    match res {
        Ok((size, sha256, head)) => Ok((token, dest_path, size, sha256, head)),
        Err(e) => {
            let _ = std::fs::remove_file(&dest_path);
            Err(e)
        }
    }
}

fn inspect(
    path: &Path,
    board_vendor: Option<&str>,
    board_model: Option<&str>,
    entry_index: Option<usize>,
) -> Result<Value, String> {
    let dir = biosusb_dir();
    let (val, staged) = inspect_into(path, board_vendor, board_model, entry_index, &dir)?;

    let token = val["token"].as_str().unwrap_or("").to_string();
    let mut store = staged_store().lock().unwrap();
    store.next_order += 1;
    let mut item = staged;
    item.order = store.next_order;
    store.items.insert(token, item);

    // Keep only the most recent few staged files to avoid cluttering %LOCALAPPDATA%.
    while store.items.len() > MAX_STAGED_KEEP {
        if let Some(oldest_key) = store
            .items
            .iter()
            .min_by_key(|(_, v)| v.order)
            .map(|(k, _)| k.clone())
        {
            if let Some(removed) = store.items.remove(&oldest_key) {
                let _ = std::fs::remove_file(&removed.path);
            }
        } else {
            break;
        }
    }

    Ok(val)
}

fn inspect_into(
    path: &Path,
    board_vendor: Option<&str>,
    board_model: Option<&str>,
    requested_index: Option<usize>,
    staging_dir: &Path,
) -> Result<(Value, StagedBios), String> {
    if path.as_os_str().is_empty() {
        return Err("Pick a downloaded BIOS ZIP or image file first.".into());
    }
    check_path_syntax(path)?;
    let meta = std::fs::metadata(path)
        .map_err(|_| "Swiftlane can't find that file. It may have been moved or deleted.".to_string())?;
    if !meta.is_file() {
        return Err("That isn't a file. Pick the downloaded BIOS ZIP or BIOS image.".into());
    }

    let source_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bios.bin".into());
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));

    if is_zip {
        let file =
            File::open(path).map_err(|e| format!("Couldn't open that ZIP archive ({e})."))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|_| "That doesn't look like a valid ZIP archive.".to_string())?;

        struct ZipCand {
            index: usize,
            entry: String,
            size: u64,
        }

        if archive.len() > MAX_ZIP_ENTRIES {
            return Err(format!(
                "This ZIP has too many files ({}). A BIOS download contains only a few.",
                archive.len()
            ));
        }

        let mut candidates: Vec<ZipCand> = Vec::new();
        let mut has_bios_renamer = false;
        let mut saw_too_small = false;
        let mut saw_too_large = false;
        let mut saw_encrypted = false;
        let mut largest_skipped: u64 = 0;

        for i in 0..archive.len() {
            // Raw access reads only the entry header; nothing is decompressed while scanning.
            let entry = archive
                .by_index_raw(i)
                .map_err(|e| format!("Couldn't read an entry in this ZIP archive ({e})."))?;
            let raw_name = entry
                .name()
                .map_err(|e| format!("Couldn't read a filename in this ZIP archive ({e})."))?
                .into_owned();
            if entry.is_dir() || raw_name.ends_with('/') || raw_name.ends_with('\\') {
                continue;
            }
            if has_zip_slip(&raw_name) {
                return Err(
                    "This ZIP archive contains an unsafe path (Zip-Slip) and was rejected.".into(),
                );
            }
            let basename = raw_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .to_string();
            if basename.is_empty() || is_macos_junk(&raw_name, &basename) {
                continue;
            }
            if basename.to_ascii_lowercase().ends_with("biosrenamer.exe") {
                has_bios_renamer = true;
                continue;
            }
            if is_junk_extension(&basename) {
                continue;
            }
            if entry.encrypted() {
                saw_encrypted = true;
                continue;
            }
            let size = entry.size();
            if size < MIN_BIOS_BYTES {
                saw_too_small = true;
                largest_skipped = largest_skipped.max(size);
                continue;
            }
            if size > MAX_BIOS_BYTES {
                saw_too_large = true;
                continue;
            }
            candidates.push(ZipCand { index: i, entry: basename, size });
        }

        if candidates.is_empty() {
            if saw_encrypted {
                return Err(
                    "The BIOS file in this ZIP is password-protected, which Swiftlane can't open. Extract it yourself and pick the extracted file."
                        .into(),
                );
            }
            if saw_too_large {
                return Err(
                    "The file inside this ZIP is over 130 MB, which is too large for a motherboard BIOS."
                        .into(),
                );
            }
            if saw_too_small {
                return Err(format!(
                    "No BIOS image (4–130 MB) was found in this ZIP (largest candidate was {:.2} MB).",
                    largest_skipped as f64 / (1024.0 * 1024.0)
                ));
            }
            return Err(
                "No BIOS image file was found inside this ZIP archive. Make sure you downloaded the BIOS update, not a driver or manual."
                    .into(),
            );
        }

        candidates.sort_by(|a, b| b.size.cmp(&a.size));

        // Entries are chosen by their position in the archive (names can repeat across folders),
        // and the position must be one of the candidates found above.
        let chosen = if let Some(req) = requested_index {
            candidates
                .iter()
                .find(|c| c.index == req)
                .ok_or_else(|| "That file isn't one of the BIOS files in this ZIP archive.".to_string())?
        } else {
            &candidates[0]
        };

        let chosen_index = chosen.index;
        let chosen_entry_name = chosen.entry.clone();
        let candidates_json: Vec<Value> = candidates
            .iter()
            .map(|c| json!({ "index": c.index, "entry": c.entry, "size": c.size }))
            .collect();

        let zip_entry = archive
            .by_index(chosen_index)
            .map_err(|e| format!("Couldn't decompress “{chosen_entry_name}” ({e})."))?;
        if zip_entry.encrypted() {
            return Err("That file in the ZIP is password-protected, which Swiftlane can't open.".into());
        }
        let (token, staged_path, actual_size, sha256, head) =
            stream_to_staged(zip_entry, staging_dir)?;

        let looks_valid = looks_like_bios(&head, actual_size);
        let size_warning = actual_size > WARN_BIOS_BYTES;
        let header_warning = !looks_valid;
        let board_mismatch =
            detect_board_mismatch(&source_name, &chosen_entry_name, board_vendor, board_model);

        let staged = StagedBios {
            path: staged_path,
            entry_name: chosen_entry_name.clone(),
            size: actual_size,
            sha256: sha256.clone(),
            order: 0,
        };

        let val = json!({
            "token": token,
            "source_kind": "zip",
            "source_name": source_name,
            "entry_name": chosen_entry_name,
            "entry_index": chosen_index,
            "candidates": candidates_json,
            "size": actual_size,
            "sha256": sha256,
            "looks_valid": looks_valid,
            "size_warning": size_warning,
            "header_warning": header_warning,
            "board_mismatch": board_mismatch,
            "has_bios_renamer": has_bios_renamer,
        });
        Ok((val, staged))
    } else {
        let size = meta.len();
        if size < MIN_BIOS_BYTES {
            return Err(format!(
                "This file is only {:.2} MB. Motherboard BIOS images are at least 4 MB.",
                size as f64 / (1024.0 * 1024.0)
            ));
        }
        if size > MAX_BIOS_BYTES {
            return Err(format!(
                "This file is {:.1} MB, which is over the 130 MB limit for a motherboard BIOS.",
                size as f64 / (1024.0 * 1024.0)
            ));
        }
        let file = File::open(path).map_err(|e| format!("Couldn't open that file ({e})."))?;
        let (token, staged_path, actual_size, sha256, head) = stream_to_staged(file, staging_dir)?;
        let looks_valid = looks_like_bios(&head, actual_size);
        let size_warning = actual_size > WARN_BIOS_BYTES;
        let header_warning = !looks_valid;
        let board_mismatch =
            detect_board_mismatch(&source_name, &source_name, board_vendor, board_model);

        let staged = StagedBios {
            path: staged_path,
            entry_name: source_name.clone(),
            size: actual_size,
            sha256: sha256.clone(),
            order: 0,
        };

        let val = json!({
            "token": token,
            "source_kind": "file",
            "source_name": source_name,
            "entry_name": staged.entry_name,
            "entry_index": 0,
            "candidates": [{ "index": 0, "entry": staged.entry_name, "size": actual_size }],
            "size": actual_size,
            "sha256": sha256,
            "looks_valid": looks_valid,
            "size_warning": size_warning,
            "header_warning": header_warning,
            "board_mismatch": board_mismatch,
            "has_bios_renamer": false,
        });
        Ok((val, staged))
    }
}

// ------------------------------------------------------------------ filename & Flashback rules

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates a BIOS filename that will be written to the root of a FAT32 USB drive.
pub fn validate_target_filename(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("Filename cannot be empty.".into());
    }
    if name.len() > 32 {
        return Err("Filename must be 32 characters or fewer.".into());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(
            "Filename may only contain letters, digits, dots, hyphens, and underscores (no spaces or slashes)."
                .into(),
        );
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("Filename cannot contain '..'.".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("Filename cannot start or end with a dot.".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(format!("“{name}” is a reserved Windows device name."));
    }
    Ok(name.to_string())
}

/// Resolves the target filename on the USB stick for `in_bios` vs `flashback` modes.
pub fn resolve_target_filename(
    mode: &str,
    vendor: &str,
    entry_name: &str,
    custom_name: &str,
) -> Result<String, String> {
    match mode {
        "in_bios" => validate_target_filename(entry_name),
        "flashback" => {
            let trimmed = custom_name.trim();
            if !trimmed.is_empty() {
                return validate_target_filename(trimmed);
            }
            match vendor.trim().to_ascii_lowercase().as_str() {
                "msi" => Ok("MSI.ROM".into()),
                "gigabyte" => Ok("GIGABYTE.bin".into()),
                "asrock" => Ok("CREATIVE.ROM".into()),
                "asus" => Err(
                    "ASUS BIOS FlashBack requires a board-specific filename (for example, SZ690E.CAP). Enter the exact name shown on your motherboard's ASUS support page."
                        .into(),
                ),
                _ => Err(
                    "Enter the exact Flashback filename required by your motherboard manual."
                        .into(),
                ),
            }
        }
        _ => Err("Choose either In-BIOS update or Flashback mode.".into()),
    }
}

// ------------------------------------------------------------------ drives, format, prepare, eject

fn parse_drive_letter(input: &str) -> Result<char, String> {
    let trimmed = input.trim();
    let bytes = trimmed.as_bytes();
    let letter = match bytes {
        [c] if c.is_ascii_alphabetic() => (*c as char).to_ascii_uppercase(),
        [c, b':'] if c.is_ascii_alphabetic() => (*c as char).to_ascii_uppercase(),
        [c, b':', b'\\'] | [c, b':', b'/'] if c.is_ascii_alphabetic() => {
            (*c as char).to_ascii_uppercase()
        }
        _ => return Err("Pick a valid USB drive letter.".into()),
    };
    Ok(letter)
}

fn list_drives() -> Value {
    let drives = platform::enumerate_usb_drives();
    let list: Vec<Value> = drives
        .into_iter()
        .map(|d| {
            json!({
                "letter": format!("{}:", d.letter),
                "root": format!("{}:\\", d.letter),
                "label": d.label,
                "fs": d.fs,
                "total_bytes": d.total_bytes,
                "free_bytes": d.free_bytes,
                "over_32gb": d.total_bytes > 32u64 * 1024 * 1024 * 1024,
                "fat32_ok": d.fs.eq_ignore_ascii_case("FAT32"),
                "can_gui_format_fat32": d.total_bytes <= 32u64 * 1024 * 1024 * 1024,
                "partition_style": d.partition_style,
                "file_count": d.file_count,
            })
        })
        .collect();
    json!({ "drives": list })
}

fn format_drive(app: &AppHandle, letter_str: &str) -> Result<(), String> {
    let letter = parse_drive_letter(letter_str)?;
    platform::verify_removable_usb_drive(letter)?;
    platform::format_drive_dialog(app, letter)
}

/// Ejects the stick; returns the other drive letters that went with it (same USB device).
fn eject_drive(letter_str: &str) -> Result<Vec<char>, String> {
    let letter = parse_drive_letter(letter_str)?;
    platform::verify_removable_usb_drive(letter)?;
    platform::eject_usb_drive(letter)
}

fn prepare(opts: &Value) -> Result<Value, String> {
    let token = opts["token"].as_str().unwrap_or("").trim();
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Inspect a BIOS file first before preparing a USB stick.".into());
    }
    let staged = {
        let store = staged_store().lock().unwrap();
        store.items.get(token).cloned()
    }
    .ok_or_else(|| "That staged BIOS file is no longer available. Inspect the file again.".to_string())?;

    // Confirm staged file still lives inside our private biosusb folder.
    let expected_dir = std::fs::canonicalize(biosusb_dir())
        .map_err(|_| "Staged BIOS folder is missing. Inspect the file again.".to_string())?;
    let actual_path = std::fs::canonicalize(&staged.path)
        .map_err(|_| "Staged BIOS file is missing. Inspect the file again.".to_string())?;
    if !actual_path.starts_with(&expected_dir) {
        return Err("Staged BIOS path failed verification.".into());
    }

    let drive_str = opts["drive"].as_str().unwrap_or("");
    let letter = parse_drive_letter(drive_str)?;
    let drive_info = platform::verify_removable_usb_drive(letter)?;
    if !drive_info.fs.eq_ignore_ascii_case("FAT32") {
        let shown_fs = if drive_info.fs.is_empty() {
            "unformatted"
        } else {
            &drive_info.fs
        };
        return Err(format!(
            "Drive {letter}: is formatted as {shown_fs}. Format it as FAT32 before copying the BIOS file."
        ));
    }

    let mode = opts["mode"].as_str().unwrap_or("in_bios");
    let vendor = opts["vendor"].as_str().unwrap_or("");
    let custom_name = opts["custom_name"].as_str().unwrap_or("");
    let overwrite = opts["overwrite"].as_bool().unwrap_or(false);

    let target_name = resolve_target_filename(mode, vendor, &staged.entry_name, custom_name)?;

    let required = staged
        .size
        .checked_add(FREE_SPACE_MARGIN)
        .ok_or_else(|| "File size overflow.".to_string())?;
    if drive_info.free_bytes < required {
        return Err(format!(
            "Not enough free space on drive {letter}: (need {:.1} MB free, only {:.1} MB available).",
            required as f64 / (1024.0 * 1024.0),
            drive_info.free_bytes as f64 / (1024.0 * 1024.0)
        ));
    }

    let root = PathBuf::from(format!("{letter}:\\"));
    copy_and_verify_to_dir(&staged, &root, &target_name, overwrite)
}

fn copy_and_verify_to_dir(
    staged: &StagedBios,
    dest_dir: &Path,
    target_name: &str,
    overwrite: bool,
) -> Result<Value, String> {
    let target_name = validate_target_filename(target_name)?;
    let target_path = dest_dir.join(&target_name);

    if target_path.exists() {
        if target_path.is_dir() {
            return Err(format!(
                "A folder named “{target_name}” already exists on that USB drive."
            ));
        }
        if !overwrite {
            return Ok(json!({
                "needs_overwrite": true,
                "target_name": target_name,
            }));
        }
    }

    // Stream copy + sync_all(), then close handle before unbuffered read-back.
    {
        let mut src = File::open(&staged.path)
            .map_err(|e| format!("Couldn't read staged BIOS file ({e})."))?;
        let mut dst = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&target_path)
            .map_err(|e| format!("Couldn't create “{target_name}” on the USB stick ({e})."))?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut written: u64 = 0;
        loop {
            let n = src
                .read(&mut buf)
                .map_err(|e| format!("Error reading staged BIOS file ({e})."))?;
            if n == 0 {
                break;
            }
            if let Err(e) = dst.write_all(&buf[..n]) {
                drop(dst);
                let _ = std::fs::remove_file(&target_path);
                return Err(format!("Couldn't write to the USB stick ({e})."));
            }
            written += n as u64;
        }
        if written != staged.size {
            drop(dst);
            let _ = std::fs::remove_file(&target_path);
            return Err(
                "Written size did not match the staged BIOS file. The partial file was removed."
                    .into(),
            );
        }
        if let Err(e) = dst.flush().and_then(|()| dst.sync_all()) {
            drop(dst);
            let _ = std::fs::remove_file(&target_path);
            return Err(format!("Couldn't flush data to the USB stick ({e})."));
        }
    }

    // Real hardware read-back verification (bypasses Windows RAM page cache).
    match verify_file_sha256_unbuffered(&target_path, staged.size, &staged.sha256) {
        Ok(verified_unbuffered) => Ok(json!({
            "ok": true,
            "target_path": target_path.to_string_lossy(),
            "target_name": target_name,
            "size": staged.size,
            "sha256": staged.sha256,
            "verified_unbuffered": verified_unbuffered,
        })),
        Err(e) => {
            let _ = std::fs::remove_file(&target_path);
            Err(e)
        }
    }
}

/// Re-opens `path` with `FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN` using a 4096-byte
/// aligned 64 KiB buffer, reads the entire file straight from storage (bypassing RAM cache),
/// and verifies both exact file length and SHA-256 hash.
pub fn verify_file_sha256_unbuffered(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool, String> {
    let (actual_hash, unbuffered) = platform::read_sha256_unbuffered(path, expected_size)?;
    if actual_hash != expected_sha256 {
        return Err(
            "Hardware read-back verification failed: the USB stick returned different bytes than written. The corrupted file was deleted — try a different USB stick."
                .into(),
        );
    }
    Ok(unbuffered)
}

struct UsbDriveInfo {
    letter: char,
    label: String,
    fs: String,
    total_bytes: u64,
    free_bytes: u64,
    partition_style: String,
    file_count: u64,
}

// ------------------------------------------------------------------ platform (Windows)

#[cfg(windows)]
mod platform {
    use super::{RawBoardInfo, UsbDriveInfo};
    use crate::tools::command;
    use sha2::{Digest, Sha256};
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::fs::OpenOptions;
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tauri::{AppHandle, Manager};
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        FILE_FLAG_NO_BUFFERING, FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    use winreg::RegKey;

    const DRIVE_REMOVABLE: u32 = 2;
    const ERROR_NOT_READY: u32 = 21;
    const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
    const ERROR_UNRECOGNIZED_VOLUME: u32 = 1005;

    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
    const IOCTL_DISK_GET_PARTITION_INFO_EX: u32 = 0x0007_0048;

    const BUS_TYPE_USB: u32 = 7;

    const SHFMT_ID_DEFAULT: u32 = 0xFFFF;
    const SHFMT_ERROR: u32 = 0xFFFF_FFFF;
    const SHFMT_CANCEL: u32 = 0xFFFF_FFFE;
    const SHFMT_NOFORMAT: u32 = 0xFFFF_FFFD;

    const DIGCF_PRESENT: u32 = 0x0000_0002;
    const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
    const CR_SUCCESS: u32 = 0;
    const CR_REMOVE_VETOED: u32 = 0x17;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const GUID_DEVINTERFACE_DISK: Guid = Guid {
        data1: 0x53F5_6307,
        data2: 0xB6BF,
        data3: 0x11D0,
        data4: [0x94, 0xF2, 0x00, 0xA0, 0xC9, 0x1E, 0xFB, 0x8B],
    };

    #[repr(C)]
    struct StoragePropertyQuery {
        property_id: u32,
        query_type: u32,
        additional_parameters: [u8; 1],
    }

    #[repr(C)]
    struct StorageDeviceDescriptorHeader {
        version: u32,
        size: u32,
        device_type: u8,
        device_type_modifier: u8,
        removable_media: u8,
        command_queueing: u8,
        vendor_id_offset: u32,
        product_id_offset: u32,
        product_revision_offset: u32,
        serial_number_offset: u32,
        bus_type: u32,
        raw_properties_length: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct StorageDeviceNumber {
        device_type: u32,
        device_number: u32,
        partition_number: u32,
    }

    #[repr(C)]
    struct SpDeviceInterfaceData {
        cb_size: u32,
        interface_class_guid: Guid,
        flags: u32,
        reserved: usize,
    }

    #[repr(C)]
    struct SpDevinfoData {
        cb_size: u32,
        class_guid: Guid,
        dev_inst: u32,
        reserved: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            h_device: HANDLE,
            dw_io_control_code: u32,
            lp_in_buffer: *const c_void,
            n_in_buffer_size: u32,
            lp_out_buffer: *mut c_void,
            n_out_buffer_size: u32,
            lp_bytes_returned: *mut u32,
            lp_overlapped: *mut c_void,
        ) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHFormatDrive(hwnd: *mut c_void, drive: u32, fmt_id: u32, options: u32) -> u32;
    }

    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            class_guid: *const Guid,
            enumerator: *const u16,
            hwnd_parent: *mut c_void,
            flags: u32,
        ) -> *mut c_void;
        fn SetupDiEnumDeviceInterfaces(
            device_info_set: *mut c_void,
            device_info_data: *const SpDevinfoData,
            interface_class_guid: *const Guid,
            member_index: u32,
            device_interface_data: *mut SpDeviceInterfaceData,
        ) -> i32;
        fn SetupDiGetDeviceInterfaceDetailW(
            device_info_set: *mut c_void,
            device_interface_data: *const SpDeviceInterfaceData,
            device_interface_detail_data: *mut c_void,
            device_interface_detail_data_size: u32,
            required_size: *mut u32,
            device_info_data: *mut SpDevinfoData,
        ) -> i32;
        fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    }

    #[link(name = "cfgmgr32")]
    extern "system" {
        fn CM_Get_Parent(pdn_dev_inst: *mut u32, dn_dev_inst: u32, ul_flags: u32) -> u32;
        fn CM_Get_Device_IDW(
            dn_dev_inst: u32,
            buffer: *mut u16,
            buffer_len: u32,
            ul_flags: u32,
        ) -> u32;
        fn CM_Request_Device_EjectW(
            dn_dev_inst: u32,
            p_veto_type: *mut u32,
            psz_veto_name: *mut u16,
            ul_name_length: u32,
            ul_flags: u32,
        ) -> u32;
    }

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn system_drive_letter() -> char {
        std::env::var("SystemDrive")
            .ok()
            .and_then(|s| s.chars().next())
            .filter(char::is_ascii_alphabetic)
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or('C')
    }

    fn powershell_exe() -> Option<PathBuf> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let p = PathBuf::from(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        p.is_file().then_some(p)
    }

    fn run_powershell(script: &str) -> Option<String> {
        let exe = powershell_exe()?;
        let mut cmd = command(&exe);
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ]);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn read_reg_string(key: &RegKey, name: &str) -> String {
        if let Ok(s) = key.get_value::<String, _>(name) {
            return s.trim().to_string();
        }
        if let Ok(list) = key.get_value::<Vec<String>, _>(name) {
            return list
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
        String::new()
    }

    pub fn read_board_info() -> RawBoardInfo {
        let mut info = RawBoardInfo::default();
        if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"HARDWARE\DESCRIPTION\System\BIOS", KEY_READ)
        {
            info.maker = read_reg_string(&key, "BaseBoardManufacturer");
            info.model = read_reg_string(&key, "BaseBoardProduct");
            info.revision = read_reg_string(&key, "BaseBoardVersion");
            info.bios_version = read_reg_string(&key, "BIOSVersion");
            if info.bios_version.is_empty() {
                info.bios_version = read_reg_string(&key, "SystemBiosVersion");
            }
            info.bios_date = read_reg_string(&key, "BIOSReleaseDate");
            info.system_maker = read_reg_string(&key, "SystemManufacturer");
            info.system_product = read_reg_string(&key, "SystemProductName");
        }

        // Only fall back to Get-CimInstance when both BaseBoardProduct and BaseBoardManufacturer are empty.
        if info.maker.is_empty() && info.model.is_empty() {
            let script = r#"$bb = Get-CimInstance Win32_BaseBoard -ErrorAction SilentlyContinue | Select-Object -First 1; $bi = Get-CimInstance Win32_BIOS -ErrorAction SilentlyContinue | Select-Object -First 1; $cs = Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue | Select-Object -First 1; [PSCustomObject]@{ maker = "$($bb.Manufacturer)"; model = "$($bb.Product)"; revision = "$($bb.Version)"; bios_version = "$($bi.SMBIOSBIOSVersion)"; bios_date = "$($bi.ReleaseDate)"; system_maker = "$($cs.Manufacturer)"; system_product = "$($cs.Model)" } | ConvertTo-Json -Compress"#;
            if let Some(json_str) = run_powershell(script) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
                    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
                    if info.maker.is_empty() {
                        info.maker = s("maker");
                    }
                    if info.model.is_empty() {
                        info.model = s("model");
                    }
                    if info.revision.is_empty() {
                        info.revision = s("revision");
                    }
                    if info.bios_version.is_empty() {
                        info.bios_version = s("bios_version");
                    }
                    if info.bios_date.is_empty() {
                        info.bios_date = s("bios_date");
                    }
                    if info.system_maker.is_empty() {
                        info.system_maker = s("system_maker");
                    }
                    if info.system_product.is_empty() {
                        info.system_product = s("system_product");
                    }
                }
            }
        }
        info
    }

    pub fn has_battery() -> bool {
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetSystemPowerStatus(&mut status) };
        ok != 0 && status.BatteryFlag != 128 && status.BatteryFlag != 255
    }

    pub fn detect_bitlocker() -> (&'static str, Option<i64>) {
        let sys = system_drive_letter();
        let script = format!(
            "(New-Object -ComObject Shell.Application).NameSpace('{sys}:\\').Self.ExtendedProperty('System.Volume.BitLockerProtection')"
        );
        let Some(out) = run_powershell(&script) else {
            return ("unknown", None);
        };
        let Ok(raw) = out.trim().parse::<i64>() else {
            return ("unknown", None);
        };
        let status = match raw {
            1 => "on",
            2 => "off",
            3 => "encrypting",
            5 => "suspended",
            6 => "locked",
            _ => "unknown",
        };
        (status, Some(raw))
    }

    pub fn shell_open_str(target: &str) -> Result<(), String> {
        let verb = wide_str("open");
        let file = wide_str(target);
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if r as usize > 32 {
            Ok(())
        } else {
            Err("Windows couldn't open that item.".into())
        }
    }

    pub fn open_diskmgmt() -> Result<(), String> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let path = format!(r"{root}\System32\diskmgmt.msc");
        shell_open_str(&path)
    }

    fn open_device_metadata(path_wide: &[u16]) -> Option<HandleGuard> {
        let h = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                0, // metadata-only access works without admin
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(HandleGuard(h))
        }
    }

    fn open_volume_metadata(letter: char) -> Option<HandleGuard> {
        let dev = wide_str(&format!(r"\\.\{letter}:"));
        open_device_metadata(&dev)
    }

    fn query_bus_type(handle: HANDLE) -> Option<u32> {
        let query = StoragePropertyQuery {
            property_id: 0, // StorageDeviceProperty
            query_type: 0,  // PropertyStandardQuery
            additional_parameters: [0],
        };
        let mut out = [0u8; 512];
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const c_void,
                std::mem::size_of::<StoragePropertyQuery>() as u32,
                out.as_mut_ptr() as *mut c_void,
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || (returned as usize) < std::mem::size_of::<StorageDeviceDescriptorHeader>() {
            return None;
        }
        // The byte buffer has alignment 1, the header has alignment 4: copy it out unaligned.
        let hdr = unsafe {
            std::ptr::read_unaligned(out.as_ptr() as *const StorageDeviceDescriptorHeader)
        };
        Some(hdr.bus_type)
    }

    fn query_partition_style_ioctl(handle: HANDLE) -> Option<&'static str> {
        // PARTITION_INFORMATION_EX is 144 bytes; first u32 is PartitionStyle (0=MBR, 1=GPT, 2=RAW).
        let mut out = [0u8; 256];
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_DISK_GET_PARTITION_INFO_EX,
                std::ptr::null(),
                0,
                out.as_mut_ptr() as *mut c_void,
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 4 {
            return None;
        }
        let style = u32::from_ne_bytes([out[0], out[1], out[2], out[3]]);
        match style {
            0 => Some("MBR"),
            1 => Some("GPT"),
            _ => None,
        }
    }

    fn query_partition_styles_ps() -> HashMap<char, String> {
        let mut map = HashMap::new();
        let script = r#"Get-Partition -ErrorAction SilentlyContinue | Where-Object { $_.DriveLetter } | ForEach-Object { $d = Get-Disk -Number $_.DiskNumber -ErrorAction SilentlyContinue; if ($d) { "$($_.DriveLetter)=$($d.PartitionStyle)" } }"#;
        let Some(out) = run_powershell(script) else {
            return map;
        };
        for line in out.lines() {
            if let Some((l, style)) = line.trim().split_once('=') {
                if let Some(ch) = l.chars().next().filter(char::is_ascii_alphabetic) {
                    let s = match style.trim().to_ascii_uppercase().as_str() {
                        "MBR" => "MBR",
                        "GPT" => "GPT",
                        _ => continue,
                    };
                    map.insert(ch.to_ascii_uppercase(), s.to_string());
                }
            }
        }
        map
    }

    fn count_visible_root_entries(root: &str) -> u64 {
        let Ok(rd) = std::fs::read_dir(root) else {
            return 0;
        };
        let mut count = 0u64;
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name == "system volume information" || name == "$recycle.bin" {
                continue;
            }
            count += 1;
        }
        count
    }

    fn inspect_single_usb_drive(letter: char) -> Result<Option<UsbDriveInfo>, String> {
        let sys = system_drive_letter();
        if letter.eq_ignore_ascii_case(&sys) {
            return Ok(None);
        }
        let root = format!("{letter}:\\");
        let root_w = wide_str(&root);
        let dtype = unsafe { GetDriveTypeW(root_w.as_ptr()) };
        if dtype != DRIVE_REMOVABLE {
            return Ok(None);
        }

        let mut partition_style = "unknown".to_string();
        // Fail closed: if we can't open the drive or can't prove it is on the USB bus, it isn't USB.
        let Some(h) = open_volume_metadata(letter) else {
            return Ok(None);
        };
        if query_bus_type(h.0) != Some(BUS_TYPE_USB) {
            return Ok(None);
        }
        if let Some(ps) = query_partition_style_ioctl(h.0) {
            partition_style = ps.to_string();
        }

        let mut label_buf = [0u16; 260];
        let mut fs_buf = [0u16; 64];
        let vol_ok = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                label_buf.as_mut_ptr(),
                label_buf.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_buf.as_mut_ptr(),
                fs_buf.len() as u32,
            )
        };

        let (label, fs) = if vol_ok != 0 {
            let l_end = label_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let f_end = fs_buf.iter().position(|&c| c == 0).unwrap_or(0);
            (
                String::from_utf16_lossy(&label_buf[..l_end]),
                String::from_utf16_lossy(&fs_buf[..f_end]),
            )
        } else {
            let err = unsafe { GetLastError() };
            if err == ERROR_NOT_READY || err == ERROR_DEVICE_NOT_CONNECTED {
                // Empty card-reader slot or disconnected drive.
                return Ok(None);
            }
            if err == ERROR_UNRECOGNIZED_VOLUME {
                (String::new(), "RAW".to_string())
            } else {
                (String::new(), String::new())
            }
        };

        let mut free_avail: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free: u64 = 0;
        let space_ok = unsafe {
            GetDiskFreeSpaceExW(
                root_w.as_ptr(),
                &mut free_avail,
                &mut total_bytes,
                &mut total_free,
            )
        };
        if space_ok == 0 && fs != "RAW" {
            return Ok(None);
        }

        let file_count = if vol_ok != 0 {
            count_visible_root_entries(&root)
        } else {
            0
        };

        Ok(Some(UsbDriveInfo {
            letter,
            label,
            fs,
            total_bytes,
            free_bytes: free_avail,
            partition_style,
            file_count,
        }))
    }

    pub fn enumerate_usb_drives() -> Vec<UsbDriveInfo> {
        let mask = unsafe { GetLogicalDrives() };
        let mut drives = Vec::new();
        for bit in 0u32..26 {
            if (mask & (1 << bit)) == 0 {
                continue;
            }
            let letter = (b'A' + bit as u8) as char;
            if let Ok(Some(info)) = inspect_single_usb_drive(letter) {
                drives.push(info);
            }
        }
        // Only invoke the batched PowerShell fallback if any drive couldn't report partition style via IOCTL.
        if drives.iter().any(|d| d.partition_style == "unknown") {
            let ps_styles = query_partition_styles_ps();
            for d in &mut drives {
                if d.partition_style == "unknown" {
                    if let Some(style) = ps_styles.get(&d.letter) {
                        d.partition_style = style.clone();
                    }
                }
            }
        }
        drives
    }

    pub fn verify_removable_usb_drive(letter: char) -> Result<UsbDriveInfo, String> {
        let sys = system_drive_letter();
        if letter.eq_ignore_ascii_case(&sys) {
            return Err("Refusing to touch the Windows system drive.".into());
        }
        match inspect_single_usb_drive(letter)? {
            Some(info) => Ok(info),
            None => Err(format!(
                "Drive {letter}: is not a connected removable USB drive. Refresh the drive list and try again."
            )),
        }
    }

    /// Shows Windows' own format dialog, owned by Swiftlane's main window. This call blocks until
    /// the dialog closes; it runs on a blocking worker thread (api::call uses spawn_blocking),
    /// so the async runtime is not held up.
    pub fn format_drive_dialog(app: &AppHandle, letter: char) -> Result<(), String> {
        let hwnd: *mut c_void = app
            .get_webview_window("main")
            .and_then(|w| w.hwnd().ok())
            .map(|h| h.0 as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
        let drive_index = (letter as u32).saturating_sub('A' as u32);
        let r = unsafe { SHFormatDrive(hwnd, drive_index, SHFMT_ID_DEFAULT, 0) };
        match r {
            SHFMT_NOFORMAT => Err("Windows reported that this drive cannot be formatted.".into()),
            SHFMT_ERROR => Err("Windows couldn't format this drive.".into()),
            SHFMT_CANCEL | _ => Ok(()),
        }
    }

    fn query_device_number(handle: HANDLE) -> Option<StorageDeviceNumber> {
        let mut num = StorageDeviceNumber::default();
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                std::ptr::null(),
                0,
                &mut num as *mut _ as *mut c_void,
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            Some(num)
        } else {
            None
        }
    }

    fn find_disk_devinst(target: StorageDeviceNumber) -> Option<u32> {
        let hdev = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVINTERFACE_DISK,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if hdev.is_null() || hdev == INVALID_HANDLE_VALUE as *mut c_void {
            return None;
        }
        struct DevInfoGuard(*mut c_void);
        impl Drop for DevInfoGuard {
            fn drop(&mut self) {
                unsafe {
                    SetupDiDestroyDeviceInfoList(self.0);
                }
            }
        }
        let _guard = DevInfoGuard(hdev);

        let detail_cb_size: u32 = if cfg!(target_pointer_width = "64") { 8 } else { 6 };
        let mut index = 0u32;

        loop {
            let mut if_data: SpDeviceInterfaceData = unsafe { std::mem::zeroed() };
            if_data.cb_size = std::mem::size_of::<SpDeviceInterfaceData>() as u32;
            let ok = unsafe {
                SetupDiEnumDeviceInterfaces(
                    hdev,
                    std::ptr::null(),
                    &GUID_DEVINTERFACE_DISK,
                    index,
                    &mut if_data,
                )
            };
            if ok == 0 {
                break;
            }
            index += 1;

            let mut req_size: u32 = 0;
            unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    hdev,
                    &if_data,
                    std::ptr::null_mut(),
                    0,
                    &mut req_size,
                    std::ptr::null_mut(),
                );
            }
            if req_size < 8 || req_size > 64 * 1024 {
                continue;
            }

            let mut detail_buf = vec![0u8; req_size as usize];
            detail_buf[..4].copy_from_slice(&detail_cb_size.to_ne_bytes());

            let mut dev_info: SpDevinfoData = unsafe { std::mem::zeroed() };
            dev_info.cb_size = std::mem::size_of::<SpDevinfoData>() as u32;

            let ok = unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    hdev,
                    &if_data,
                    detail_buf.as_mut_ptr() as *mut c_void,
                    req_size,
                    std::ptr::null_mut(),
                    &mut dev_info,
                )
            };
            if ok == 0 {
                continue;
            }

            // DevicePath starts at byte offset 4 as a NUL-terminated UTF-16 string.
            let path_bytes = &detail_buf[4..];
            let mut path_w: Vec<u16> = path_bytes
                .chunks_exact(2)
                .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            path_w.push(0);

            if let Some(h) = open_device_metadata(&path_w) {
                if let Some(dn) = query_device_number(h.0) {
                    if dn.device_type == target.device_type
                        && dn.device_number == target.device_number
                    {
                        return Some(dev_info.dev_inst);
                    }
                }
            }
        }
        None
    }

    /// The parent device node of the disk behind a drive letter.
    fn parent_devinst_of_drive(letter: char) -> Result<u32, String> {
        let dev_num = {
            let h = open_volume_metadata(letter)
                .ok_or_else(|| format!("Couldn't open drive {letter}: to eject it."))?;
            query_device_number(h.0)
                .ok_or_else(|| format!("Couldn't identify the USB device for drive {letter}:."))?
        };
        let disk_inst = find_disk_devinst(dev_num)
            .ok_or_else(|| format!("Couldn't locate the USB disk device for drive {letter}:."))?;

        let mut parent_inst: u32 = 0;
        let cr = unsafe { CM_Get_Parent(&mut parent_inst, disk_inst, 0) };
        if cr != CR_SUCCESS {
            return Err(
                "Couldn't find the parent USB controller for this stick. Use Safely Remove Hardware in the taskbar."
                    .into(),
            );
        }
        Ok(parent_inst)
    }

    fn device_id(inst: u32) -> Option<String> {
        let mut buf = [0u16; 512];
        let cr = unsafe { CM_Get_Device_IDW(inst, buf.as_mut_ptr(), buf.len() as u32, 0) };
        if cr != CR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    fn is_usb_device_id(id: &str) -> bool {
        let up = id.to_ascii_uppercase();
        up.starts_with("USB\\") || up.starts_with("USBSTOR\\")
    }

    /// Ejects the whole USB device behind `letter` and returns the other drive letters that
    /// live on the same device (they go away too).
    pub fn eject_usb_drive(letter: char) -> Result<Vec<char>, String> {
        let parent_inst = parent_devinst_of_drive(letter)?;

        // Only ever eject a USB device node, never something higher up (a hub, a controller).
        match device_id(parent_inst) {
            Some(id) if is_usb_device_id(&id) => {}
            _ => {
                return Err(
                    "This drive isn't directly on a USB device, so Swiftlane won't eject it. Use Safely Remove Hardware in the taskbar."
                        .into(),
                )
            }
        }

        // Other USB drives on the same device (card-reader slots, extra partitions' disks).
        let mut also: Vec<char> = Vec::new();
        for d in enumerate_usb_drives() {
            if d.letter.eq_ignore_ascii_case(&letter) {
                continue;
            }
            if parent_devinst_of_drive(d.letter).is_ok_and(|p| p == parent_inst) {
                also.push(d.letter);
            }
        }

        let mut last_veto = false;
        for attempt in 0..3 {
            let mut veto_type: u32 = 0;
            let mut veto_name = [0u16; 260];
            let cr = unsafe {
                CM_Request_Device_EjectW(
                    parent_inst,
                    &mut veto_type,
                    veto_name.as_mut_ptr(),
                    veto_name.len() as u32,
                    0,
                )
            };
            if cr == CR_SUCCESS && veto_type == 0 {
                return Ok(also);
            }
            last_veto = cr == CR_REMOVE_VETOED || veto_type != 0;
            if attempt < 2 && last_veto {
                std::thread::sleep(Duration::from_millis(300));
            }
        }

        if last_veto {
            Err(
                "Another program (such as File Explorer) is still using this USB stick. Close any windows open on it and try again."
                    .into(),
            )
        } else {
            Err(
                "Windows couldn't eject the USB stick right now. Use Safely Remove Hardware in the taskbar."
                    .into(),
            )
        }
    }

    struct AlignedChunk {
        ptr: *mut u8,
        layout: Layout,
    }

    impl AlignedChunk {
        const SIZE: usize = 64 * 1024;
        const ALIGN: usize = 4096;

        fn new() -> Result<Self, String> {
            let layout = Layout::from_size_align(Self::SIZE, Self::ALIGN)
                .map_err(|_| "Invalid aligned buffer layout.".to_string())?;
            let ptr = unsafe { alloc_zeroed(layout) };
            if ptr.is_null() {
                return Err("Out of memory allocating aligned read-back buffer.".into());
            }
            Ok(Self { ptr, layout })
        }

        fn as_mut_slice(&mut self) -> &mut [u8] {
            unsafe { std::slice::from_raw_parts_mut(self.ptr, Self::SIZE) }
        }
    }

    impl Drop for AlignedChunk {
        fn drop(&mut self) {
            unsafe {
                dealloc(self.ptr, self.layout);
            }
        }
    }

    /// Reads `path` bypassing the Windows page cache (`FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN`)
    /// with a 4096-byte aligned 64 KiB buffer and computes its SHA-256 over the exact `expected_size` bytes.
    pub fn read_sha256_unbuffered(path: &Path, expected_size: u64) -> Result<(String, bool), String> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
            .open(path)
            .map_err(|e| format!("Couldn't re-open the USB file for hardware read-back ({e})."))?;

        let actual_len = file
            .metadata()
            .map_err(|e| format!("Couldn't inspect the written USB file ({e})."))?
            .len();
        if actual_len != expected_size {
            return Err(format!(
                "USB file size mismatch on read-back (expected {expected_size} bytes, got {actual_len})."
            ));
        }

        let mut chunk = AlignedChunk::new()?;
        let mut hasher = Sha256::new();
        let mut total_read: u64 = 0;

        while total_read < expected_size {
            let n = file
                .read(chunk.as_mut_slice())
                .map_err(|e| format!("Hardware read-back error while verifying USB file ({e})."))?;
            if n == 0 {
                break;
            }
            let remaining = (expected_size - total_read) as usize;
            let use_bytes = n.min(remaining);
            hasher.update(&chunk.as_mut_slice()[..use_bytes]);
            total_read += use_bytes as u64;
        }

        if total_read != expected_size {
            return Err(format!(
                "USB read-back ended early ({total_read} of {expected_size} bytes)."
            ));
        }

        Ok((hex::encode(hasher.finalize()), true))
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{RawBoardInfo, UsbDriveInfo};
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use tauri::AppHandle;
    use std::io::Read;
    use std::path::Path;

    pub fn read_board_info() -> RawBoardInfo {
        RawBoardInfo::default()
    }
    pub fn has_battery() -> bool {
        false
    }
    pub fn detect_bitlocker() -> (&'static str, Option<i64>) {
        ("unknown", None)
    }
    pub fn shell_open_str(target: &str) -> Result<(), String> {
        if std::process::Command::new("xdg-open").arg(target).spawn().is_ok() {
            Ok(())
        } else {
            Err("Couldn't open that link.".into())
        }
    }
    pub fn open_diskmgmt() -> Result<(), String> {
        Err("Disk Management is only available on Windows.".into())
    }
    pub fn enumerate_usb_drives() -> Vec<UsbDriveInfo> {
        Vec::new()
    }
    pub fn verify_removable_usb_drive(_letter: char) -> Result<UsbDriveInfo, String> {
        Err("USB preparation is only available on Windows.".into())
    }
    pub fn format_drive_dialog(_app: &AppHandle, _letter: char) -> Result<(), String> {
        Err("Drive formatting is only available on Windows.".into())
    }
    pub fn eject_usb_drive(_letter: char) -> Result<Vec<char>, String> {
        Err("Safe eject is only available on Windows.".into())
    }
    pub fn read_sha256_unbuffered(path: &Path, expected_size: u64) -> Result<(String, bool), String> {
        let mut f = File::open(path).map_err(|e| format!("Read-back failed ({e})."))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut buf).map_err(|e| format!("Read-back failed ({e})."))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        if total != expected_size {
            return Err("Size mismatch on read-back.".into());
        }
        Ok((hex::encode(hasher.finalize()), false))
    }
}

// ------------------------------------------------------------------ unit tests

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let mut b = [0u8; 8];
            getrandom::getrandom(&mut b).unwrap();
            let dir = std::env::temp_dir().join(format!("swl-biosusb-test-{}", hex::encode(b)));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn make_bios_bytes(size: usize, with_fvh: bool) -> Vec<u8> {
        let mut data = vec![0x5Au8; size];
        // Make content non-trivial so SHA-256 and corruption checks are meaningful.
        for (i, chunk) in data.chunks_mut(4096).enumerate() {
            let tag = (i as u32).to_le_bytes();
            if chunk.len() >= 4 {
                chunk[..4].copy_from_slice(&tag);
            }
        }
        if with_fvh && size >= 64 {
            data[40..44].copy_from_slice(b"_FVH");
        }
        data
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8], CompressionMethod)]) {
        let f = File::create(path).unwrap();
        let mut zw = ZipWriter::new(f);
        for &(name, data, method) in entries {
            let opts = SimpleFileOptions::default().compression_method(method);
            zw.start_file(name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }

    #[test]
    fn validate_target_filename_rules() {
        // Valid BIOS names
        for good in [
            "MSI.ROM",
            "GIGABYTE.bin",
            "CREATIVE.ROM",
            "SZ690E.CAP",
            "E7D75AMS.1H0",
            "B650E_3024.CAP",
            "BIOS-F12.bin",
            "a",
        ] {
            assert_eq!(validate_target_filename(good).unwrap(), good);
        }

        // Rejections
        let bad = [
            "",
            "../MSI.ROM",
            "..\\MSI.ROM",
            "dir/MSI.ROM",
            "dir\\MSI.ROM",
            "C:MSI.ROM",
            "MSI ROM.bin",
            "MSI.ROM.",
            ".MSI.ROM",
            "..",
            ".",
            "MSI..ROM",
            "123456789012345678901234567890123", // 33 chars
            "CON",
            "con",
            "CON.CAP",
            "PRN.bin",
            "AUX.ROM",
            "nul.rom",
            "COM1.CAP",
            "COM9.bin",
            "LPT1",
            "LPT9.ROM",
            "bios\u{00E9}.bin",
        ];
        for name in bad {
            assert!(
                validate_target_filename(name).is_err(),
                "expected '{name}' to be rejected"
            );
        }
    }

    #[test]
    fn flashback_filename_resolution() {
        assert_eq!(
            resolve_target_filename("flashback", "msi", "E7D75AMS.1H0", "").unwrap(),
            "MSI.ROM"
        );
        assert_eq!(
            resolve_target_filename("flashback", "gigabyte", "B650AORUS.F12", "").unwrap(),
            "GIGABYTE.bin"
        );
        assert_eq!(
            resolve_target_filename("flashback", "asrock", "B650E3.10", "").unwrap(),
            "CREATIVE.ROM"
        );
        // ASUS requires custom board-specific name
        assert!(resolve_target_filename("flashback", "asus", "ASUS.CAP", "").is_err());
        assert_eq!(
            resolve_target_filename("flashback", "asus", "ASUS.CAP", "SZ690E.CAP").unwrap(),
            "SZ690E.CAP"
        );
        // In-BIOS keeps original entry name (validated)
        assert_eq!(
            resolve_target_filename("in_bios", "msi", "E7D75AMS.1H0", "").unwrap(),
            "E7D75AMS.1H0"
        );
        assert!(resolve_target_filename("in_bios", "msi", "../bad.bin", "").is_err());
    }

    #[test]
    fn zip_inspect_deflate_and_stored_skips_junk_and_flags_renamer() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let payload = make_bios_bytes(4 * 1024 * 1024 + 2048, true);
        let expected_sha = hex::encode(Sha256::digest(&payload));

        // Deflated ZIP with BIOSRenamer.exe, readme.txt, __MACOSX junk, and BIOS inside a subdir
        let zip_deflate = tmp.path().join("ROG-STRIX-B650E-BIOS.zip");
        write_zip(
            &zip_deflate,
            &[
                ("BIOS/README.txt", b"Update instructions", CompressionMethod::Deflated),
                ("BIOS/BIOSRenamer.exe", b"MZfakeexe", CompressionMethod::Deflated),
                ("__MACOSX/._ROG.CAP", b"junk", CompressionMethod::Deflated),
                ("BIOS/ROG-B650E.CAP", &payload, CompressionMethod::Deflated),
            ],
        );

        let (val, staged) = inspect_into(
            &zip_deflate,
            Some("asus"),
            Some("ROG STRIX B650E-F GAMING WIFI"),
            None,
            &stage,
        )
        .unwrap();
        assert_eq!(val["source_kind"], "zip");
        assert_eq!(val["entry_name"], "ROG-B650E.CAP");
        assert_eq!(val["size"], payload.len() as u64);
        assert_eq!(val["sha256"], expected_sha);
        assert_eq!(val["has_bios_renamer"], true);
        assert_eq!(val["looks_valid"], true);
        assert_eq!(val["header_warning"], false);
        assert_eq!(val["size_warning"], false);
        assert!(val["board_mismatch"].is_null());
        assert!(staged.path.is_file());

        // Stored ZIP
        let zip_stored = tmp.path().join("MSI-B650.zip");
        write_zip(
            &zip_stored,
            &[("E7D75AMS.1H0", &payload, CompressionMethod::Stored)],
        );
        let (val2, _) =
            inspect_into(&zip_stored, Some("msi"), Some("MAG B650 TOMAHAWK"), None, &stage)
                .unwrap();
        assert_eq!(val2["entry_name"], "E7D75AMS.1H0");
        assert_eq!(val2["sha256"], expected_sha);
        assert_eq!(val2["has_bios_renamer"], false);
    }

    #[test]
    fn zip_rejects_zip_slip_too_small_too_large_and_warns_over_65mb() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");

        // Zip-Slip path traversal
        let zip_slip = tmp.path().join("evil.zip");
        let small = vec![0u8; 1024];
        write_zip(
            &zip_slip,
            &[("../../evil.bin", &small, CompressionMethod::Stored)],
        );
        assert!(inspect_into(&zip_slip, None, None, None, &stage).is_err());

        // Too small (< 4 MB)
        let zip_small = tmp.path().join("small.zip");
        let under = vec![0u8; 2 * 1024 * 1024];
        write_zip(
            &zip_small,
            &[("TINY.ROM", &under, CompressionMethod::Deflated)],
        );
        assert!(inspect_into(&zip_small, None, None, None, &stage).is_err());

        // Over 65 MB -> allowed with size_warning = true (all zeros compresses in milliseconds!)
        let zip_big_ok = tmp.path().join("big66.zip");
        let big66 = vec![0u8; 66 * 1024 * 1024];
        write_zip(
            &zip_big_ok,
            &[("LARGE.ROM", &big66, CompressionMethod::Deflated)],
        );
        let (val, _) = inspect_into(&zip_big_ok, None, None, None, &stage).unwrap();
        assert_eq!(val["size_warning"], true);
        assert_eq!(val["size"], 66 * 1024 * 1024);

        // Over 130 MB (zip bomb / oversized payload) -> rejected
        let zip_bomb = tmp.path().join("bomb131.zip");
        let bomb = vec![0u8; 131 * 1024 * 1024];
        write_zip(
            &zip_bomb,
            &[("BOMB.ROM", &bomb, CompressionMethod::Deflated)],
        );
        assert!(inspect_into(&zip_bomb, None, None, None, &stage).is_err());
    }

    #[test]
    fn zip_bomb_streaming_cap_enforced_even_if_header_lies() {
        // Test stream_to_staged directly with a repeating reader yielding 131 MiB (exceeds MAX_BIOS_BYTES)
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let endless = std::io::repeat(0xAA).take(131 * 1024 * 1024);
        let err = stream_to_staged(endless, &stage);
        assert!(err.is_err());
        // Partial temp file must be cleaned up on abort!
        let remaining = std::fs::read_dir(&stage)
            .map(|rd| rd.count())
            .unwrap_or(0);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn bare_file_inspection_heuristics_and_board_mismatch() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");

        // 8 MiB valid UEFI _FVH file
        let valid_path = tmp.path().join("ROG-STRIX-B550-F-ASUS-3607.CAP");
        std::fs::write(&valid_path, make_bios_bytes(8 * 1024 * 1024, true)).unwrap();

        // Matching board
        let (ok_val, _) = inspect_into(
            &valid_path,
            Some("asus"),
            Some("ROG STRIX B550-F GAMING"),
            None,
            &stage,
        )
        .unwrap();
        assert_eq!(ok_val["source_kind"], "file");
        assert_eq!(ok_val["looks_valid"], true);
        assert_eq!(ok_val["header_warning"], false);
        assert!(ok_val["board_mismatch"].is_null());

        // Vendor mismatch: ASUS file on MSI board
        let (vendor_mm, _) = inspect_into(
            &valid_path,
            Some("msi"),
            Some("MAG B550 TOMAHAWK"),
            None,
            &stage,
        )
        .unwrap();
        assert!(vendor_mm["board_mismatch"]
            .as_str()
            .is_some_and(|s| s.contains("ASUS") && s.contains("MSI")));

        // Chipset mismatch: B550 file on B650 board (same vendor)
        let (chip_mm, _) = inspect_into(
            &valid_path,
            Some("asus"),
            Some("ROG STRIX B650E-F GAMING"),
            None,
            &stage,
        )
        .unwrap();
        assert!(chip_mm["board_mismatch"]
            .as_str()
            .is_some_and(|s| s.contains("B550") && s.contains("B650")));

        // Non-power-of-2 size without firmware signatures -> header_warning = true (soft, never blocks!)
        let weird_path = tmp.path().join("mystery.bin");
        let weird = make_bios_bytes(5 * 1024 * 1024 + 12345, false);
        std::fs::write(&weird_path, &weird).unwrap();
        let (warn_val, _) = inspect_into(&weird_path, None, None, None, &stage).unwrap();
        assert_eq!(warn_val["looks_valid"], false);
        assert_eq!(warn_val["header_warning"], true);
    }

    #[test]
    fn overwrite_guard_and_unbuffered_hardware_readback_verification() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let fake_usb = tmp.path().join("usb");
        std::fs::create_dir_all(&fake_usb).unwrap();

        // Use a non-4K-multiple size (4 MiB + 2048 bytes, like an ASUS .CAP) to prove
        // FILE_FLAG_NO_BUFFERING handles non-sector-multiple EOF cleanly!
        let src_path = tmp.path().join("TESTBIOS.CAP");
        let data = make_bios_bytes(4 * 1024 * 1024 + 2048, true);
        std::fs::write(&src_path, &data).unwrap();

        let (_, staged) = inspect_into(&src_path, None, None, None, &stage).unwrap();

        // First copy succeeds and verifies via FILE_FLAG_NO_BUFFERING on Windows
        let first = copy_and_verify_to_dir(&staged, &fake_usb, "MSI.ROM", false).unwrap();
        assert_eq!(first["ok"], true);
        assert_eq!(first["target_name"], "MSI.ROM");
        assert_eq!(first["sha256"], staged.sha256);
        assert_eq!(first["verified_unbuffered"], cfg!(windows));

        // Second copy without overwrite=true triggers overwrite guard
        let guard = copy_and_verify_to_dir(&staged, &fake_usb, "MSI.ROM", false).unwrap();
        assert_eq!(guard["needs_overwrite"], true);
        assert_eq!(guard["target_name"], "MSI.ROM");

        // Second copy with overwrite=true succeeds
        let second = copy_and_verify_to_dir(&staged, &fake_usb, "MSI.ROM", true).unwrap();
        assert_eq!(second["ok"], true);
        assert_eq!(second["verified_unbuffered"], cfg!(windows));

        // Direct unbuffered read-back detects a 1-byte bitflip on disk
        let target_file = fake_usb.join("MSI.ROM");
        let mut corrupted = std::fs::read(&target_file).unwrap();
        corrupted[100_000] ^= 0xFF;
        std::fs::write(&target_file, &corrupted).unwrap();
        assert!(
            verify_file_sha256_unbuffered(&target_file, staged.size, &staged.sha256).is_err()
        );
    }

    #[test]
    fn allowlist_rejects_arbitrary_urls() {
        assert!(ALLOWED_SUPPORT_URLS.contains(&"https://www.asus.com/support/download-center/"));
        assert!(open_allowed_url("https://evil.example.com/bios.exe").is_err());
        assert!(open_allowed_url("file:///C:/Windows/System32/cmd.exe").is_err());
        assert!(open_allowed_url("http://www.msi.com/support/download").is_err());
    }

    #[test]
    fn zip_multi_candidate_switching() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let small_bios = make_bios_bytes(4 * 1024 * 1024, true);
        let big_bios = make_bios_bytes(8 * 1024 * 1024, true);
        let small_sha = hex::encode(Sha256::digest(&small_bios));
        let big_sha = hex::encode(Sha256::digest(&big_bios));

        let zip_path = tmp.path().join("MULTI.zip");
        write_zip(
            &zip_path,
            &[
                ("REV1/BOARD_R1.BIN", &small_bios, CompressionMethod::Deflated),
                ("REV2/BOARD_R2.BIN", &big_bios, CompressionMethod::Deflated),
            ],
        );

        let (v_default, _) = inspect_into(&zip_path, None, None, None, &stage).unwrap();
        assert_eq!(v_default["entry_name"], "BOARD_R2.BIN");
        assert_eq!(v_default["sha256"], big_sha);
        assert_eq!(v_default["candidates"].as_array().unwrap().len(), 2);

        let r1_index = v_default["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["entry"] == "BOARD_R1.BIN")
            .unwrap()["index"]
            .as_u64()
            .unwrap() as usize;
        let (v_switched, _) =
            inspect_into(&zip_path, None, None, Some(r1_index), &stage).unwrap();
        assert_eq!(v_switched["entry_name"], "BOARD_R1.BIN");
        assert_eq!(v_switched["sha256"], small_sha);

        // An index that isn't a BIOS candidate is refused.
        assert!(inspect_into(&zip_path, None, None, Some(99), &stage).is_err());
    }

    #[test]
    fn zip_duplicate_basenames_are_selected_by_index() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let a = make_bios_bytes(4 * 1024 * 1024, true);
        let b = make_bios_bytes(8 * 1024 * 1024, true);
        let a_sha = hex::encode(Sha256::digest(&a));
        let b_sha = hex::encode(Sha256::digest(&b));
        let zip_path = tmp.path().join("DUP.zip");
        write_zip(
            &zip_path,
            &[
                ("A/BIOS.CAP", &a, CompressionMethod::Deflated),
                ("B/BIOS.CAP", &b, CompressionMethod::Deflated),
            ],
        );
        let (v_default, _) = inspect_into(&zip_path, None, None, None, &stage).unwrap();
        assert_eq!(v_default["sha256"], b_sha);
        let cands = v_default["candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0]["entry"], cands[1]["entry"]);
        let small = cands.iter().find(|c| c["size"] == a.len() as u64).unwrap();
        let idx = small["index"].as_u64().unwrap() as usize;
        let (v, _) = inspect_into(&zip_path, None, None, Some(idx), &stage).unwrap();
        assert_eq!(v["sha256"], a_sha);
        assert_eq!(v["entry_index"], idx as u64);
    }

    /// Marks every entry in a ZIP as encrypted by setting general-purpose flag bit 0 in the local
    /// and central headers (enough for the scanner; nothing here decrypts).
    fn mark_zip_encrypted(path: &Path, only_name: &str) {
        let mut bytes = std::fs::read(path).unwrap();
        let name = only_name.as_bytes();
        let mut i = 0;
        while i + 46 < bytes.len() {
            let local = bytes[i..i + 4] == [0x50, 0x4B, 0x03, 0x04];
            let central = bytes[i..i + 4] == [0x50, 0x4B, 0x01, 0x02];
            if local || central {
                let (flag_off, name_len_off, name_off) =
                    if local { (6, 26, 30) } else { (8, 28, 46) };
                let nlen = u16::from_le_bytes([bytes[i + name_len_off], bytes[i + name_len_off + 1]]) as usize;
                if bytes.get(i + name_off..i + name_off + nlen) == Some(name) {
                    bytes[i + flag_off] |= 1;
                }
            }
            i += 1;
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn zip_skips_encrypted_entries_and_errors_if_only_encrypted() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let big = make_bios_bytes(8 * 1024 * 1024, true);
        let small = make_bios_bytes(4 * 1024 * 1024, true);
        let small_sha = hex::encode(Sha256::digest(&small));

        let zip_path = tmp.path().join("ENC.zip");
        write_zip(
            &zip_path,
            &[
                ("SECRET.CAP", &big, CompressionMethod::Stored),
                ("OPEN.CAP", &small, CompressionMethod::Stored),
            ],
        );
        mark_zip_encrypted(&zip_path, "SECRET.CAP");
        let (v, _) = inspect_into(&zip_path, None, None, None, &stage).unwrap();
        assert_eq!(v["entry_name"], "OPEN.CAP");
        assert_eq!(v["sha256"], small_sha);
        assert_eq!(v["candidates"].as_array().unwrap().len(), 1);

        let only = tmp.path().join("ONLY.zip");
        write_zip(&only, &[("SECRET.CAP", &big, CompressionMethod::Stored)]);
        mark_zip_encrypted(&only, "SECRET.CAP");
        let err = inspect_into(&only, None, None, None, &stage).unwrap_err();
        assert!(err.contains("password"));
    }

    #[test]
    fn zip_with_too_many_entries_is_rejected() {
        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        let zip_path = tmp.path().join("MANY.zip");
        let f = File::create(&zip_path).unwrap();
        let mut zw = ZipWriter::new(f);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for i in 0..(MAX_ZIP_ENTRIES + 1) {
            zw.start_file(format!("f{i}.txt"), opts).unwrap();
        }
        zw.finish().unwrap();
        let err = inspect_into(&zip_path, None, None, None, &stage).unwrap_err();
        assert!(err.contains("too many files"));
    }

    #[test]
    fn unc_and_device_paths_are_rejected() {
        for bad in [
            r"\\server\share\bios.zip",
            r"\\?\C:\bios.zip",
            r"\\.\PhysicalDrive0",
            "//server/share/bios.zip",
        ] {
            assert!(check_path_syntax(Path::new(bad)).is_err(), "{bad}");
        }
        assert!(check_path_syntax(Path::new(r"C:\Users\me\bios.zip")).is_ok());

        let tmp = TempDir::new();
        let stage = tmp.path().join("stage");
        assert!(inspect_into(Path::new(r"\\server\share\bios.zip"), None, None, None, &stage).is_err());
    }

    #[test]
    fn only_offered_paths_are_remembered() {
        let tmp = TempDir::new();
        let p = tmp.path().join("x.zip");
        assert!(!was_offered(&p));
        offer_path(&p);
        assert!(was_offered(&p));
    }

    #[test]
    fn board_vendor_revision_and_laptop_helpers() {
        assert_eq!(map_vendor("ASUSTeK COMPUTER INC.", ""), "asus");
        assert_eq!(map_vendor("Micro-Star International Co., Ltd.", ""), "msi");
        assert_eq!(map_vendor("Gigabyte Technology Co., Ltd.", ""), "gigabyte");
        assert_eq!(map_vendor("ASRock", ""), "asrock");
        assert_eq!(map_vendor("BIOSTAR Group", ""), "biostar");
        assert_eq!(map_vendor("Default string", "Dell Inc."), "dell");
        assert_eq!(map_vendor("HP", ""), "hp");
        assert_eq!(map_vendor("LENOVO", ""), "lenovo");
        assert_eq!(map_vendor("Unknown Corp", ""), "other");

        assert!(is_revision_uncertain("1.0", "gigabyte"));
        assert!(is_revision_uncertain("Default string", "asus"));
        assert!(is_revision_uncertain("To be filled by O.E.M.", "msi"));
        assert!(is_revision_uncertain("1.xx", "asrock"));
        assert!(is_revision_uncertain("", "asus"));
        assert!(!is_revision_uncertain("Rev 1.02", "asus"));

        assert!(is_laptop_product_hint("ASUS ZenBook UX425"));
        assert!(is_laptop_product_hint("ASUS TUF Gaming A15"));
        assert!(!is_laptop_product_hint("ROG STRIX B650E-F GAMING WIFI"));
        assert!(is_oem_maker_hint("Dell Inc."));
        assert!(!is_oem_maker_hint("ASUSTeK COMPUTER INC."));
    }
}
