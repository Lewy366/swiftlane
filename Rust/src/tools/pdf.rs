//! PDF tools (Source/toolkit/pdftools.py): open PDFs as page thumbnails, save the pages in a new
//! order (several PDFs merged, pages rotated or left out, signatures placed), split, make smaller.
//!
//! Two engines:
//! - lopdf (pure Rust) re-assembles pages into new files: save, split and sign. It copies only
//!   what the chosen pages use, so nothing else from the original files comes along.
//! - PDFium (Google's PDF engine, the one the Python version used) draws the pages: thumbnails,
//!   the big page for placing a signature, and "make smaller". It is pdfium.dll next to
//!   Swiftlane.exe, loaded at run time from there only; without it those parts say so plainly.
//!
//! PDFs are untrusted: they are only read and copied, nothing in them (scripts, links, forms) is
//! ever run, sizes and page counts are limited, and the original files are never changed.

use base64::Engine;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use pdfium_render::prelude::{PdfDocument, PdfPage, PdfRenderConfig, Pdfium, PdfiumError, PdfiumInternalError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use super::{arg, ask_folder, ask_save};
use crate::jobs::JobError;

const MAX_BYTES: u64 = 500 * 1024 * 1024; // per PDF
const MAX_PAGES: usize = 10_000; // per PDF, and per new PDF
const MAX_THUMBS: usize = 400;
const THUMB_WIDTH: f32 = 150.0;
const THUMB_MAX_HEIGHT: f32 = 600.0;
const MAX_RENDER_PIXELS: f32 = 25_000_000.0; // one drawn page, whatever size the PDF claims
const SMALLER_DPI: f32 = 110.0;
const SMALLER_QUALITY: u8 = 65;
const MAX_SIGNATURE_BYTES: usize = 5 * 1024 * 1024;
const MAX_SIGNATURE_SIDE: u32 = 2400;
const MAX_PICTURE_FILE: u64 = 40 * 1024 * 1024;

const UNREADABLE: &str = "This file isn't a PDF Swiftlane can read.";
const LOCKED: &str = "This PDF is password-protected. Open it once with its password and save a copy without one.";
const TOO_BIG: &str = "This PDF is over 500 MB, which is too big for the PDF tools.";
const TOO_MANY: &str = "This PDF has more than 10,000 pages, which is too many for the PDF tools.";
const DAMAGED: &str = "This PDF is damaged. Open it in a PDF viewer, save a fresh copy, and try that one.";
const NO_ENGINE: &str = "Swiftlane's PDF viewer (pdfium.dll) is missing from its folder or doesn't work, so pages can't be shown. Reinstalling Swiftlane puts it back.";
const MISSING_PAGE: &str = "A page is missing. Open the PDFs again and retry.";

pub fn dispatch(app: &AppHandle, method: &str, args: &[Value]) -> Option<Value> {
    Some(match method {
        "pdf_pick" => pick_pdfs(app),
        "pdf_open" => json!(strings(arg(args, 0)).iter().map(|p| open(Path::new(p))).collect::<Vec<_>>()),
        "pdf_page" => match (arg(args, 0).as_str(), index(arg(args, 1))) {
            (Some(p), Some(i)) => page_image(Path::new(p), i, 900.0).map_or(Value::Null, Value::from),
            _ => Value::Null,
        },
        "pdf_save" => save(app, args),
        "pdf_split" => split_job(app, args),
        "pdf_smaller" => smaller_job(app, args),
        "pick_signature" => pick_signature(app),
        _ => return None,
    })
}

// ------------------------------------------------------------------ page calls

fn pick_pdfs(app: &AppHandle) -> Value {
    let picked = app.dialog().file().add_filter("PDF files", &["pdf"]).add_filter("All files", &["*"]).blocking_pick_files().unwrap_or_default();
    json!(picked.into_iter().filter_map(|p| p.into_path().ok()).map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>())
}

fn open(path: &Path) -> Value {
    let name = file_name(path);
    match thumbnails(path) {
        Ok(pages) => json!({ "path": path.to_string_lossy(), "name": name, "pages": pages }),
        Err(e) => json!({ "path": path.to_string_lossy(), "name": name, "error": e }),
    }
}

fn save(app: &AppHandle, args: &[Value]) -> Value {
    let sources: Vec<PathBuf> = strings(arg(args, 0)).into_iter().map(PathBuf::from).collect();
    let plan = match parse_plan(arg(args, 1)) {
        Ok(p) if p.is_empty() => return json!({ "error": "There are no pages to save." }),
        Ok(p) => p,
        Err(e) => return json!({ "error": e }),
    };
    let sigs = match parse_signatures(arg(args, 2)) {
        Ok(s) => s,
        Err(e) => return json!({ "error": e }),
    };
    let Some(first) = sources.get(plan[0].src) else { return json!({ "error": MISSING_PAGE }) };
    let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(out) = ask_save(app, &dir, &format!("{} (edited).pdf", stem(first)), &["pdf"]) else { return Value::Null };
    if sources.iter().any(|s| same_file(s, &out)) {
        return json!({ "error": "That's one of the original PDFs, and those are never changed. Pick a new name." });
    }
    let title = format!("{} pages → {}", plan.len(), file_name(&out));
    let id = app.state::<crate::api::AppState>().jobs.start(app, "pdf", &title, true, move |ctx| {
        ctx.report(None, Some("Putting the pages together…"));
        build(&sources, &plan, &sigs, &out, &|| ctx.check())
    });
    json!(id)
}

fn split_job(app: &AppHandle, args: &[Value]) -> Value {
    let Some(path) = arg(args, 0).as_str().map(PathBuf::from) else { return Value::Null };
    let groups: Vec<Vec<usize>> = arg(args, 1).as_array().map(|g| g.iter().map(|x| x.as_array().map(|a| a.iter().filter_map(index).collect()).unwrap_or_default()).collect()).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(folder) = ask_folder(app, &dir) else { return Value::Null };
    let title = format!("Split {} into {}", file_name(&path), groups.len());
    let id = app.state::<crate::api::AppState>().jobs.start(app, "pdf", &title, true, move |ctx| {
        split(&path, &folder, &groups, &|f| {
            ctx.report(Some(f), None);
            ctx.check()
        })
    });
    json!(id)
}

fn smaller_job(app: &AppHandle, args: &[Value]) -> Value {
    let Some(path) = arg(args, 0).as_str().map(PathBuf::from) else { return Value::Null };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(out) = ask_save(app, &dir, &format!("{} (smaller).pdf", stem(&path)), &["pdf"]) else { return Value::Null };
    if same_file(&path, &out) {
        return json!({ "error": "That's the original PDF, and it's never changed. Pick a new name." });
    }
    let title = format!("Make {} smaller", file_name(&path));
    let id = app.state::<crate::api::AppState>().jobs.start(app, "pdf", &title, true, move |ctx| {
        make_smaller(&path, &out, &|f| {
            ctx.report(Some(f), None);
            ctx.check()
        })
    });
    json!(id)
}

/// A picture of a signature, cut down to a sensible size, as a PNG data: URL.
fn pick_signature(app: &AppHandle) -> Value {
    let picked = app.dialog().file().add_filter("Images", &["png", "jpg", "jpeg"]).add_filter("All files", &["*"]).blocking_pick_file().and_then(|p| p.into_path().ok());
    let Some(path) = picked else { return Value::Null };
    match signature_from_file(&path) {
        Some(url) => json!(url),
        None => json!({ "error": "Can't use that picture." }),
    }
}

fn signature_from_file(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_PICTURE_FILE {
        return None;
    }
    let img = decode_picture(&std::fs::read(path).ok()?)?;
    let img = if img.width() > 1200 || img.height() > 600 { img.thumbnail(1200, 600) } else { img };
    let mut png = Vec::new();
    img.to_rgba8().write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

/// Decodes a picture with limits, so a crafted file can't use up all memory.
fn decode_picture(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(300 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

// ------------------------------------------------------------------ arguments

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// A whole, non-negative number from the page (it may arrive as 3 or 3.0).
fn index(v: &Value) -> Option<usize> {
    v.as_u64().map(|n| n as usize).or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0 && *f < 1e9).map(|f| f as usize))
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "PDF".into())
}

/// True if `out` is (an existing name for) the file `src`.
fn same_file(src: &Path, out: &Path) -> bool {
    match (std::fs::canonicalize(src), std::fs::canonicalize(out)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// One page of the new PDF: page `page` of source `src`, turned `rotate` degrees more.
#[derive(Clone, Copy, Debug)]
struct Item {
    src: usize,
    page: usize,
    rotate: i64,
}

/// A signature on page `at` of the new PDF; x, y = its top left corner and w = its width, as
/// fractions of the page as shown (before any extra rotation).
#[derive(Clone, Debug)]
struct Sig {
    at: usize,
    png: String,
    x: f32,
    y: f32,
    w: f32,
}

fn parse_plan(v: &Value) -> Result<Vec<Item>, String> {
    let items = v.as_array().cloned().unwrap_or_default();
    if items.len() > MAX_PAGES {
        return Err("That's more than 10,000 pages, which is too many for one PDF.".into());
    }
    items
        .iter()
        .map(|i| {
            let rotate = i["rotate"].as_f64().unwrap_or(0.0);
            match (index(&i["src"]), index(&i["page"])) {
                (Some(src), Some(page)) if rotate.is_finite() => Ok(Item { src, page, rotate: rotate as i64 }),
                _ => Err(MISSING_PAGE.to_string()),
            }
        })
        .collect()
}

fn parse_signatures(v: &Value) -> Result<Vec<Sig>, String> {
    let items = v.as_array().cloned().unwrap_or_default();
    if items.len() > 200 {
        return Err("That's too many signatures for one PDF.".into());
    }
    items
        .iter()
        .map(|s| {
            let f = |k: &str| s[k].as_f64().filter(|x| x.is_finite()).map(|x| x as f32);
            match (index(&s["at"]), s["png"].as_str(), f("x"), f("y"), f("w")) {
                (Some(at), Some(png), Some(x), Some(y), Some(w)) => Ok(Sig { at, png: png.to_string(), x, y, w }),
                _ => Err("A signature couldn't be placed. Add it again.".to_string()),
            }
        })
        .collect()
}

// ------------------------------------------------------------------ reading PDFs (lopdf)

struct Source {
    doc: Document,
    pages: Vec<ObjectId>,
    page_set: HashSet<ObjectId>,
}

fn check_size(path: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|_| "Can't find this PDF anymore. Was it moved or deleted?".to_string())?;
    if !meta.is_file() {
        return Err(UNREADABLE.into());
    }
    if meta.len() > MAX_BYTES {
        return Err(TOO_BIG.into());
    }
    Ok(())
}

fn load(path: &Path) -> Result<Source, String> {
    check_size(path)?;
    // Lenient loading (repairs damaged files), with lopdf's limits on nesting and on how much a
    // compressed part may grow when unpacked.
    let doc = Document::load_with_options(path, lopdf::LoadOptions::default()).map_err(|e| match e {
        lopdf::Error::InvalidPassword => LOCKED.to_string(),
        _ => UNREADABLE.to_string(),
    })?;
    let pages: Vec<ObjectId> = doc.page_iter().take(MAX_PAGES + 1).collect();
    if pages.len() > MAX_PAGES {
        return Err(TOO_MANY.into());
    }
    if pages.is_empty() {
        return Err("This PDF has no pages.".into());
    }
    let page_set = pages.iter().copied().collect();
    Ok(Source { doc, pages, page_set })
}

/// A page attribute that may be set on the page itself or on a page-tree node above it.
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut dict = doc.get_dictionary(page).ok()?;
    for _ in 0..64 {
        if let Ok(v) = dict.get(key) {
            return Some(v);
        }
        dict = doc.get_dictionary(dict.get(b"Parent").ok()?.as_reference().ok()?).ok()?;
    }
    None
}

/// 0, 90, 180 or 270.
fn normalize_rotation(r: i64) -> i64 {
    (r.rem_euclid(360) / 90) * 90
}

fn page_rotation(doc: &Document, page: ObjectId) -> i64 {
    let r = inherited(doc, page, b"Rotate").and_then(|o| doc.dereference(o).ok()).and_then(|(_, o)| o.as_float().ok()).unwrap_or(0.0);
    normalize_rotation(r as i64)
}

/// [llx, lly, urx, ury] of a box (MediaBox, CropBox) given directly or by reference.
fn rect(doc: &Document, obj: &Object) -> Option<[f32; 4]> {
    let (_, arr) = doc.dereference(obj).ok()?;
    let a = arr.as_array().ok()?;
    if a.len() != 4 {
        return None;
    }
    let mut v = [0f32; 4];
    for (slot, o) in v.iter_mut().zip(a) {
        *slot = doc.dereference(o).ok()?.1.as_float().ok()?;
        if !slot.is_finite() {
            return None;
        }
    }
    let r = [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])];
    (r[2] - r[0] > 0.0 && r[3] - r[1] > 0.0).then_some(r)
}

// ------------------------------------------------------------------ writing PDFs (lopdf)

const INHERITED_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// A new PDF made of pages copied from sources. Only the objects the copied pages use are copied
/// (fonts, pictures, annotations); links to pages that aren't copied become empty.
struct Builder<'a> {
    srcs: &'a [Source],
    out: Document,
    pages_id: ObjectId,
    kids: Vec<Object>,
    copied: Vec<HashMap<ObjectId, ObjectId>>,    // per source: old id -> new id
    page_target: Vec<HashMap<ObjectId, ObjectId>>, // per source: page -> its first copy
    queue: Vec<(usize, ObjectId, ObjectId)>,
}

impl<'a> Builder<'a> {
    fn new(srcs: &'a [Source]) -> Self {
        let mut out = Document::with_version("1.7");
        let pages_id = out.new_object_id();
        Builder { srcs, out, pages_id, kids: Vec::new(), copied: vec![HashMap::new(); srcs.len()], page_target: vec![HashMap::new(); srcs.len()], queue: Vec::new() }
    }

    /// Adds the pages in order and returns their new ids.
    fn add_pages(&mut self, items: &[Item]) -> Vec<ObjectId> {
        let srcs = self.srcs;
        // Ids first, so links between copied pages can point at pages copied later.
        let ids: Vec<ObjectId> = items.iter().map(|_| self.out.new_object_id()).collect();
        for (it, id) in items.iter().zip(&ids) {
            self.page_target[it.src].entry(srcs[it.src].pages[it.page]).or_insert(*id);
        }
        for (it, id) in items.iter().zip(&ids) {
            let dict = self.page_dict(it.src, srcs[it.src].pages[it.page], it.rotate);
            self.out.objects.insert(*id, Object::Dictionary(dict));
            self.kids.push(Object::Reference(*id));
        }
        while let Some((s, old, new)) = self.queue.pop() {
            let mut o = srcs[s].doc.objects.get(&old).cloned().unwrap_or(Object::Null);
            self.convert(s, &mut o);
            self.out.objects.insert(new, o);
        }
        ids
    }

    fn page_dict(&mut self, s: usize, page: ObjectId, extra_rotate: i64) -> Dictionary {
        let srcs = self.srcs;
        let doc = &srcs[s].doc;
        let mut d = doc.get_dictionary(page).cloned().unwrap_or_default();
        for key in INHERITED_KEYS {
            if !d.has(key) {
                if let Some(v) = inherited(doc, page, key) {
                    d.set(key.to_vec(), v.clone());
                }
            }
        }
        if !d.has(b"MediaBox") {
            d.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]);
        }
        let rotation = normalize_rotation(page_rotation(doc, page) + extra_rotate);
        d.set("Rotate", rotation);
        d.set("Type", Object::Name(b"Page".to_vec()));
        d.remove(b"Parent");
        d.remove(b"AA"); // page open/close actions
        let mut obj = Object::Dictionary(d);
        self.convert(s, &mut obj);
        let Object::Dictionary(mut d) = obj else { unreachable!() };
        d.set("Parent", self.pages_id);
        d
    }

    /// Rewrites the references inside a copied object to the new file's ids, queueing objects
    /// not copied yet. lopdf limits how deeply a PDF's objects nest, so this recursion is bounded.
    fn convert(&mut self, s: usize, obj: &mut Object) {
        match obj {
            Object::Reference(id) => *obj = self.map_ref(s, *id),
            Object::Array(a) => a.iter_mut().for_each(|o| self.convert(s, o)),
            Object::Dictionary(d) => d.iter_mut().for_each(|(_, o)| self.convert(s, o)),
            Object::Stream(st) => st.dict.iter_mut().for_each(|(_, o)| self.convert(s, o)),
            _ => {}
        }
    }

    fn map_ref(&mut self, s: usize, id: ObjectId) -> Object {
        let srcs = self.srcs;
        let src = &srcs[s];
        if src.page_set.contains(&id) {
            return self.page_target[s].get(&id).map_or(Object::Null, |n| Object::Reference(*n));
        }
        if let Some(n) = self.copied[s].get(&id) {
            return Object::Reference(*n);
        }
        match src.doc.objects.get(&id) {
            None => Object::Null,
            // The old page tree and catalogue stay behind.
            Some(Object::Dictionary(d)) if d.has_type(b"Pages") || d.has_type(b"Catalog") => Object::Null,
            Some(_) => {
                let n = self.out.new_object_id();
                self.copied[s].insert(id, n);
                self.queue.push((s, id, n));
                Object::Reference(n)
            }
        }
    }

    fn finish(self) -> Document {
        finish(self.out, self.pages_id, self.kids)
    }
}

fn finish(mut out: Document, pages_id: ObjectId, kids: Vec<Object>) -> Document {
    let count = kids.len() as i64;
    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", kids);
    pages.set("Count", count);
    out.objects.insert(pages_id, Object::Dictionary(pages));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", pages_id);
    let catalog_id = out.add_object(catalog);
    out.trailer.set("Root", catalog_id);
    out.compress();
    out
}

/// Writes next to `out` first and then moves it into place, so a failed save never leaves half
/// a file under the chosen name.
fn write_pdf(doc: &mut Document, out: &Path) -> Result<(), JobError> {
    let mut rnd = [0u8; 6];
    let _ = getrandom::getrandom(&mut rnd);
    let tmp = out.with_file_name(format!(".{}.{}.part", file_name(out), hex::encode(rnd)));
    let fail = || JobError::Failed("Couldn't write the new PDF there. Pick another folder.".into());
    let written = (|| -> std::io::Result<()> {
        let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut w = std::io::BufWriter::new(f);
        doc.save_to(&mut w).map_err(std::io::Error::other)?;
        w.flush()?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, out)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail());
    }
    Ok(())
}

/// The new PDF from `plan` (pages of `sources` in the new order), with signatures placed.
fn build(sources: &[PathBuf], plan: &[Item], sigs: &[Sig], out: &Path, check: &dyn Fn() -> Result<(), JobError>) -> Result<Vec<PathBuf>, JobError> {
    if plan.is_empty() {
        return Err(JobError::Failed("There are no pages to save.".into()));
    }
    // Only the sources the plan uses are opened.
    let mut used: Vec<usize> = plan.iter().map(|i| i.src).collect();
    used.sort_unstable();
    used.dedup();
    let mut srcs = Vec::new();
    let mut slot = HashMap::new();
    for s in used {
        let path = sources.get(s).ok_or(MISSING_PAGE)?;
        check()?;
        let src = load(path).map_err(|e| format!("{}: {e}", file_name(path)))?;
        slot.insert(s, srcs.len());
        srcs.push(src);
    }
    let items: Vec<Item> = plan
        .iter()
        .map(|i| {
            let s = slot[&i.src];
            if i.page < srcs[s].pages.len() { Ok(Item { src: s, ..*i }) } else { Err(MISSING_PAGE) }
        })
        .collect::<Result<_, _>>()?;
    check()?;
    let mut b = Builder::new(&srcs);
    let ids = b.add_pages(&items);
    for sig in sigs {
        let (Some(&page_id), Some(item)) = (ids.get(sig.at), items.get(sig.at)) else { continue };
        let shown_rotation = page_rotation(&srcs[item.src].doc, srcs[item.src].pages[item.page]);
        let img = signature_image(&sig.png)?;
        place_signature(&mut b.out, page_id, &img, sig, shown_rotation)?;
    }
    check()?;
    let mut doc = b.finish();
    write_pdf(&mut doc, out)?;
    Ok(vec![out.to_path_buf()])
}

/// One new PDF per group of pages, named "<name> (pages a-b).pdf" in `folder`; existing files are
/// never replaced.
fn split(path: &Path, folder: &Path, groups: &[Vec<usize>], progress: &dyn Fn(f64) -> Result<(), JobError>) -> Result<Vec<PathBuf>, JobError> {
    let srcs = [load(path)?];
    let n = srcs[0].pages.len();
    let groups: Vec<Vec<usize>> = groups.iter().map(|g| g.iter().copied().filter(|p| *p < n).collect::<Vec<_>>()).filter(|g| !g.is_empty()).take(MAX_PAGES).collect();
    let stem = stem(path);
    let mut outs = Vec::new();
    for (k, g) in groups.iter().enumerate() {
        progress(k as f64 / groups.len() as f64)?;
        let label = if g.len() == 1 { format!("page {}", g[0] + 1) } else { format!("pages {}-{}", g[0] + 1, g[g.len() - 1] + 1) };
        let out = reserve(folder, &format!("{stem} ({label})"))?;
        let mut b = Builder::new(&srcs);
        b.add_pages(&g.iter().map(|&page| Item { src: 0, page, rotate: 0 }).collect::<Vec<_>>());
        let mut doc = b.finish();
        if let Err(e) = write_pdf(&mut doc, &out) {
            let _ = std::fs::remove_file(&out);
            return Err(e);
        }
        outs.push(out);
    }
    Ok(outs)
}

/// Claims a new file name "<base>.pdf", "<base> (2).pdf", … in `folder` (made empty, filled later).
fn reserve(folder: &Path, base: &str) -> Result<PathBuf, JobError> {
    for n in 1..1000 {
        let name = if n == 1 { format!("{base}.pdf") } else { format!("{base} ({n}).pdf") };
        let p = folder.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => break,
        }
    }
    Err(JobError::Failed("Couldn't write the new PDFs there. Pick another folder.".into()))
}

// ------------------------------------------------------------------ signatures

fn signature_image(data_url: &str) -> Result<image::RgbaImage, JobError> {
    let b64 = data_url.split_once(',').map_or(data_url, |(_, b)| b);
    if b64.len() > MAX_SIGNATURE_BYTES / 3 * 4 + 8 {
        return Err(JobError::Failed("The signature picture is too big.".into()));
    }
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| "The signature picture can't be read.")?;
    if raw.len() > MAX_SIGNATURE_BYTES {
        return Err(JobError::Failed("The signature picture is too big.".into()));
    }
    let img = decode_picture(&raw).ok_or("The signature picture can't be read.")?;
    let img = if img.width().max(img.height()) > MAX_SIGNATURE_SIDE { img.thumbnail(MAX_SIGNATURE_SIDE, MAX_SIGNATURE_SIDE) } else { img };
    Ok(img.to_rgba8())
}

/// Where a signature goes in the page's own coordinates: the matrix [a b c d e f] that maps the
/// picture's unit square onto the page. `shown_rotation` is how the page was shown when the
/// signature was placed; the picture comes out upright as it was seen.
fn signature_matrix(page_box: [f32; 4], shown_rotation: i64, iw: u32, ih: u32, sig: &Sig) -> [f32; 6] {
    let [llx, lly, urx, ury] = page_box;
    let (bw, bh) = (urx - llx, ury - lly);
    let (vw, vh) = if shown_rotation % 180 == 90 { (bh, bw) } else { (bw, bh) };
    let sw = sig.w.clamp(0.02, 1.0) * vw;
    let sh = sw * ih as f32 / iw.max(1) as f32;
    let (left, top) = (sig.x.clamp(0.0, 1.0) * vw, sig.y.clamp(0.0, 1.0) * vh);
    // From "as shown" (from the top left corner, downwards) to the page's own coordinates.
    let page_point = |u: f32, v: f32| match shown_rotation {
        90 => (llx + v, lly + u),
        180 => (urx - u, lly + v),
        270 => (urx - v, ury - u),
        _ => (llx + u, ury - v),
    };
    let p00 = page_point(left, top + sh); // the picture's bottom left
    let p10 = page_point(left + sw, top + sh); // bottom right
    let p01 = page_point(left, top); // top left
    [p10.0 - p00.0, p10.1 - p00.1, p01.0 - p00.0, p01.1 - p00.1, p00.0, p00.1]
}

fn resolved_dict(doc: &Document, v: Option<&Object>) -> Dictionary {
    match v {
        Some(Object::Reference(id)) => doc.get_dictionary(*id).cloned().unwrap_or_default(),
        Some(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    }
}

fn place_signature(out: &mut Document, page_id: ObjectId, img: &image::RgbaImage, sig: &Sig, shown_rotation: i64) -> Result<(), JobError> {
    let mut page = out.get_dictionary(page_id).cloned().map_err(|_| MISSING_PAGE)?;
    let page_box = page.get(b"CropBox").ok().and_then(|o| rect(out, o)).or_else(|| page.get(b"MediaBox").ok().and_then(|o| rect(out, o))).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let (w, h) = img.dimensions();

    // The picture, with its transparency kept as a soft mask.
    let rgb: Vec<u8> = img.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", w as i64);
    dict.set("Height", h as i64);
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", 8);
    if img.pixels().any(|p| p[3] < 255) {
        let mut mask = Dictionary::new();
        mask.set("Type", Object::Name(b"XObject".to_vec()));
        mask.set("Subtype", Object::Name(b"Image".to_vec()));
        mask.set("Width", w as i64);
        mask.set("Height", h as i64);
        mask.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
        mask.set("BitsPerComponent", 8);
        let alpha: Vec<u8> = img.pixels().map(|p| p[3]).collect();
        let mask_id = out.add_object(Stream::new(mask, alpha));
        dict.set("SMask", mask_id);
    }
    let img_id = out.add_object(Stream::new(dict, rgb));

    // Its name in the page's own resources (copied, as other pages may share them).
    let mut res = resolved_dict(out, page.get(b"Resources").ok());
    let mut xobjects = resolved_dict(out, res.get(b"XObject").ok());
    let name = (1..).map(|n| format!("SwiftlaneSignature{n}")).find(|n| !xobjects.has(n.as_bytes())).unwrap_or_default();
    xobjects.set(name.clone(), img_id);
    res.set("XObject", xobjects);
    page.set("Resources", res);

    // Draw it after the page's own content, which is wrapped in q … Q so whatever it leaves
    // behind (scaling, clipping) doesn't move the signature.
    let m = signature_matrix(page_box, shown_rotation, w, h, sig);
    let mut parts: Vec<Object> = match page.get(b"Contents") {
        Ok(Object::Reference(id)) => match out.get_object(*id) {
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![Object::Reference(*id)],
        },
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let before = out.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let ops = format!("\nQ\nq {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /{name} Do Q\n", m[0], m[1], m[2], m[3], m[4], m[5]);
    let after = out.add_object(Stream::new(Dictionary::new(), ops.into_bytes()));
    parts.insert(0, Object::Reference(before));
    parts.push(Object::Reference(after));
    page.set("Contents", parts);
    out.objects.insert(page_id, Object::Dictionary(page));
    Ok(())
}

// ------------------------------------------------------------------ drawing pages (PDFium)

/// PDFium, loaded once from pdfium.dll in Swiftlane's own folder (never from PATH or the current
/// folder). None if it's missing or doesn't work.
fn engine() -> Result<&'static Pdfium, String> {
    static ENGINE: OnceLock<Option<Pdfium>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            let lib = [dir.to_path_buf(), dir.join("tools")].iter().map(Pdfium::pdfium_platform_library_name_at_path).find(|p| p.is_file())?;
            match Pdfium::bind_to_library(&lib) {
                Ok(bindings) => Some(Pdfium::new(bindings)),
                Err(e) => {
                    crate::config::log(&format!("pdfium couldn't be loaded: {e:?}"));
                    None
                }
            }
        })
        .as_ref()
        .ok_or_else(|| NO_ENGINE.to_string())
}

fn open_pdfium<'a>(pdfium: &'a Pdfium, path: &Path) -> Result<PdfDocument<'a>, String> {
    check_size(path)?;
    pdfium.load_pdf_from_file(path, None).map_err(|e| match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => LOCKED.to_string(),
        _ => UNREADABLE.to_string(),
    })
}

/// Draws a page `scale` pixels per point (less if that would be a huge picture) and returns it as
/// a JPEG with its size in pixels.
fn render_jpeg(page: &PdfPage, scale: f32, quality: u8) -> Result<(Vec<u8>, u32, u32), String> {
    let (w, h) = (page.width().value.max(1.0), page.height().value.max(1.0));
    let mut scale = scale;
    if w * h * scale * scale > MAX_RENDER_PIXELS {
        scale = (MAX_RENDER_PIXELS / (w * h)).sqrt();
    }
    let bitmap = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale)).map_err(|_| "A page couldn't be drawn.".to_string())?;
    let (bw, bh) = (bitmap.width().max(0) as u32, bitmap.height().max(0) as u32);
    let rgba = bitmap.as_rgba_bytes();
    if bw == 0 || bh == 0 || rgba.len() != bw as usize * bh as usize * 4 {
        return Err("A page couldn't be drawn.".into());
    }
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality).encode(&rgb, bw, bh, image::ExtendedColorType::Rgb8).map_err(|_| "A page couldn't be drawn.".to_string())?;
    Ok((jpeg, bw, bh))
}

fn data_url(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg))
}

/// Size, rotation and a small picture of each page (the first 400).
fn thumbnails(path: &Path) -> Result<Vec<Value>, String> {
    // Read with lopdf too, so a PDF that couldn't be saved later is reported now.
    let src = load(path)?;
    let pdfium = engine()?;
    let doc = open_pdfium(pdfium, path)?;
    let pages = doc.pages();
    if pages.len().max(0) as usize != src.pages.len() {
        return Err(DAMAGED.into());
    }
    let mut out = Vec::with_capacity(src.pages.len());
    for (i, id) in src.pages.iter().enumerate() {
        let page = pages.get(i as i32).map_err(|_| DAMAGED.to_string())?;
        let (w, h) = (page.width().value, page.height().value);
        let mut item = json!({ "w": w, "h": h, "rotation": page_rotation(&src.doc, *id) });
        if i < MAX_THUMBS {
            let scale = (THUMB_WIDTH / w.max(1.0)).min(THUMB_MAX_HEIGHT / h.max(1.0));
            if let Ok((jpeg, _, _)) = render_jpeg(&page, scale, 70) {
                item["thumb"] = json!(data_url(&jpeg));
            }
        }
        out.push(item);
    }
    Ok(out)
}

/// A bigger picture of one page, for placing a signature.
fn page_image(path: &Path, index: usize, width: f32) -> Option<String> {
    let pdfium = engine().ok()?;
    let doc = open_pdfium(pdfium, path).ok()?;
    let page = doc.pages().get(i32::try_from(index).ok()?).ok()?;
    let (jpeg, _, _) = render_jpeg(&page, width / page.width().value.max(1.0), 85).ok()?;
    Some(data_url(&jpeg))
}

/// A smaller file by turning every page into a picture. Text can't be selected or searched
/// afterwards (the page warns about that before starting).
fn make_smaller(path: &Path, out: &Path, progress: &dyn Fn(f64) -> Result<(), JobError>) -> Result<Vec<PathBuf>, JobError> {
    let pdfium = engine()?;
    let src = open_pdfium(pdfium, path)?;
    let n = src.pages().len().max(0) as usize;
    if n == 0 {
        return Err(JobError::Failed("This PDF has no pages.".into()));
    }
    if n > MAX_PAGES {
        return Err(JobError::Failed(TOO_MANY.into()));
    }
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(n);
    for i in 0..n {
        progress(i as f64 / n as f64)?;
        let page = src.pages().get(i as i32).map_err(|_| DAMAGED)?;
        let (jpeg, bw, bh) = render_jpeg(&page, SMALLER_DPI / 72.0, SMALLER_QUALITY)?;
        // The page as shown (already turned), with the picture's proportions.
        let w = page.width().value.max(1.0);
        let h = w * bh as f32 / bw as f32;
        let mut im = Dictionary::new();
        im.set("Type", Object::Name(b"XObject".to_vec()));
        im.set("Subtype", Object::Name(b"Image".to_vec()));
        im.set("Width", bw as i64);
        im.set("Height", bh as i64);
        im.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        im.set("BitsPerComponent", 8);
        im.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        let im_id = doc.add_object(Stream::new(im, jpeg).with_compression(false));
        let content = doc.add_object(Stream::new(Dictionary::new(), format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q").into_bytes()));
        let mut xo = Dictionary::new();
        xo.set("Im0", im_id);
        let mut res = Dictionary::new();
        res.set("XObject", xo);
        let mut p = Dictionary::new();
        p.set("Type", Object::Name(b"Page".to_vec()));
        p.set("Parent", pages_id);
        p.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Real(w), Object::Real(h)]);
        p.set("Resources", res);
        p.set("Contents", content);
        kids.push(Object::Reference(doc.add_object(p)));
    }
    progress(1.0)?;
    let mut doc = finish(doc, pages_id, kids);
    write_pdf(&mut doc, out)?;
    Ok(vec![out.to_path_buf()])
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder in the system temp folder, removed again when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let mut r = [0u8; 8];
            getrandom::getrandom(&mut r).unwrap();
            let p = std::env::temp_dir().join(format!("swl-pdf-test-{}", hex::encode(r)));
            std::fs::create_dir(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ok<T>(r: Result<T, JobError>) -> T {
        match r {
            Ok(v) => v,
            Err(JobError::Failed(e)) => panic!("failed: {e}"),
            Err(JobError::Cancelled) => panic!("cancelled"),
        }
    }

    /// A small PDF: `n` pages saying "Page k of <tag>", MediaBox and Resources inherited from the
    /// page tree, page 2 turned 90 degrees, and a link on page 1 pointing at page 2.
    fn make_pdf(path: &Path, tag: &str, n: usize) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        let font_id = doc.add_object(font);
        let mut fonts = Dictionary::new();
        fonts.set("F1", font_id);
        let mut res = Dictionary::new();
        res.set("Font", fonts);
        let res_id = doc.add_object(res);
        let ids: Vec<ObjectId> = (0..n).map(|_| doc.new_object_id()).collect();
        for (k, id) in ids.iter().enumerate() {
            let text = format!("BT /F1 24 Tf 72 700 Td (Page {} of {tag}) Tj ET", k + 1);
            let c = doc.add_object(Stream::new(Dictionary::new(), text.into_bytes()));
            let mut p = Dictionary::new();
            p.set("Type", Object::Name(b"Page".to_vec()));
            p.set("Parent", pages_id);
            p.set("Contents", c);
            if k == 1 {
                p.set("Rotate", 90);
            }
            if k == 0 && n > 1 {
                let mut link = Dictionary::new();
                link.set("Type", Object::Name(b"Annot".to_vec()));
                link.set("Subtype", Object::Name(b"Link".to_vec()));
                link.set("Rect", vec![Object::Integer(0), Object::Integer(0), Object::Integer(10), Object::Integer(10)]);
                link.set("Dest", vec![Object::Reference(ids[1]), Object::Name(b"Fit".to_vec())]);
                link.set("P", *id);
                let l = doc.add_object(link);
                p.set("Annots", vec![Object::Reference(l)]);
            }
            doc.objects.insert(*id, Object::Dictionary(p));
        }
        let mut pages = Dictionary::new();
        pages.set("Type", Object::Name(b"Pages".to_vec()));
        pages.set("Kids", ids.iter().map(|i| Object::Reference(*i)).collect::<Vec<_>>());
        pages.set("Count", n as i64);
        pages.set("MediaBox", vec![Object::Integer(0), Object::Integer(0), Object::Integer(595), Object::Integer(842)]);
        pages.set("Resources", res_id);
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut cat = Dictionary::new();
        cat.set("Type", Object::Name(b"Catalog".to_vec()));
        cat.set("Pages", pages_id);
        let cat_id = doc.add_object(cat);
        doc.trailer.set("Root", cat_id);
        doc.compress();
        doc.save(path).unwrap();
    }

    fn page_text(doc: &Document, page: ObjectId) -> String {
        String::from_utf8_lossy(&doc.get_page_content(page)).to_string()
    }

    fn reopen(path: &Path) -> (Document, Vec<ObjectId>) {
        let doc = Document::load(path).unwrap();
        let pages = doc.get_pages().into_values().collect();
        (doc, pages)
    }

    #[test]
    fn merges_reorders_and_rotates() {
        let dir = TempDir::new();
        let (a, b) = (dir.0.join("a.pdf"), dir.0.join("b.pdf"));
        make_pdf(&a, "A", 3);
        make_pdf(&b, "B", 2);
        let before = std::fs::read(&a).unwrap();
        let out = dir.0.join("out.pdf");
        let plan = parse_plan(&json!([
            { "src": 1, "page": 1, "rotate": 0 },
            { "src": 0, "page": 2, "rotate": 90 },
            { "src": 0, "page": 0, "rotate": 270 },
            { "src": 0, "page": 1, "rotate": 180 },
            { "src": 0, "page": 1, "rotate": 0 },
        ]))
        .unwrap();
        let made = ok(build(&[a.clone(), b], &plan, &[], &out, &|| Ok(())));
        assert_eq!(made, vec![out.clone()]);
        assert_eq!(std::fs::read(&a).unwrap(), before, "the original must not change");

        let (doc, pages) = reopen(&out);
        assert_eq!(pages.len(), 5);
        let texts: Vec<String> = pages.iter().map(|p| page_text(&doc, *p)).collect();
        assert!(texts[0].contains("Page 2 of B"));
        assert!(texts[1].contains("Page 3 of A"));
        assert!(texts[2].contains("Page 1 of A"));
        assert!(texts[3].contains("Page 2 of A") && texts[4].contains("Page 2 of A"));
        let rot: Vec<i64> = pages.iter().map(|p| page_rotation(&doc, *p)).collect();
        assert_eq!(rot, vec![90, 90, 270, 270, 90]); // page 2 of each file starts at 90
        for p in &pages {
            let d = doc.get_dictionary(*p).unwrap();
            assert!(rect(&doc, d.get(b"MediaBox").unwrap()).is_some(), "inherited MediaBox copied");
            assert!(d.has(b"Resources"), "inherited Resources copied");
        }
        // The link on page 1 of A now points at the first copy of page 2 of A.
        let annots = doc.get_dictionary(pages[2]).unwrap().get(b"Annots").unwrap().as_array().unwrap();
        let link = doc.get_dictionary(annots[0].as_reference().unwrap()).unwrap();
        assert_eq!(link.get(b"Dest").unwrap().as_array().unwrap()[0].as_reference().unwrap(), pages[3]);
        assert_eq!(link.get(b"P").unwrap().as_reference().unwrap(), pages[2]);
    }

    #[test]
    fn pages_left_out_are_not_linked() {
        let dir = TempDir::new();
        let a = dir.0.join("a.pdf");
        make_pdf(&a, "A", 3);
        let out = dir.0.join("first only.pdf");
        let plan = parse_plan(&json!([{ "src": 0, "page": 0, "rotate": 0 }])).unwrap();
        ok(build(&[a], &plan, &[], &out, &|| Ok(())));
        let (doc, pages) = reopen(&out);
        assert_eq!(pages.len(), 1);
        // Page 2 isn't in the new file, so the link's target is empty, and no other page came along.
        let annots = doc.get_dictionary(pages[0]).unwrap().get(b"Annots").unwrap().as_array().unwrap();
        let link = doc.get_dictionary(annots[0].as_reference().unwrap()).unwrap();
        assert!(link.get(b"Dest").unwrap().as_array().unwrap()[0].is_null());
        let page_objects = doc.objects.values().filter(|o| o.as_dict().is_ok_and(|d| d.has_type(b"Page"))).count();
        assert_eq!(page_objects, 1);
    }

    #[test]
    fn missing_pages_and_bad_files_fail_plainly() {
        let dir = TempDir::new();
        let a = dir.0.join("a.pdf");
        make_pdf(&a, "A", 2);
        let out = dir.0.join("out.pdf");
        let plan = parse_plan(&json!([{ "src": 0, "page": 5 }])).unwrap();
        match build(&[a.clone()], &plan, &[], &out, &|| Ok(())) {
            Err(JobError::Failed(e)) => assert_eq!(e, MISSING_PAGE),
            _ => panic!("expected an error"),
        }
        assert!(!out.exists());
        let junk = dir.0.join("junk.pdf");
        std::fs::write(&junk, b"this is not a pdf").unwrap();
        assert_eq!(load(&junk).err().unwrap(), UNREADABLE);
        assert!(parse_plan(&json!([{ "src": -1, "page": 0 }])).is_err());
        assert!(parse_plan(&json!([{ "src": 0, "page": 0.5 }])).is_err());
        // Cancelling stops before anything is written.
        let plan = parse_plan(&json!([{ "src": 0, "page": 0 }])).unwrap();
        assert!(matches!(build(&[a], &plan, &[], &out, &|| Err(JobError::Cancelled)), Err(JobError::Cancelled)));
        assert!(!out.exists());
        // Nothing but the test files is left in the folder (no .part files).
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 2);
    }

    #[test]
    fn splits_into_groups_without_replacing_files() {
        let dir = TempDir::new();
        let a = dir.0.join("Report.pdf");
        make_pdf(&a, "A", 5);
        let outdir = dir.0.join("out");
        std::fs::create_dir(&outdir).unwrap();
        std::fs::write(outdir.join("Report (page 5).pdf"), b"keep me").unwrap();
        let groups = vec![vec![0, 1], vec![2, 3], vec![4], vec![99]];
        let outs = ok(split(&a, &outdir, &groups, &|_| Ok(())));
        let names: Vec<String> = outs.iter().map(|p| file_name(p)).collect();
        assert_eq!(names, vec!["Report (pages 1-2).pdf", "Report (pages 3-4).pdf", "Report (page 5) (2).pdf"]);
        assert_eq!(std::fs::read(outdir.join("Report (page 5).pdf")).unwrap(), b"keep me");
        let counts: Vec<usize> = outs.iter().map(|p| reopen(p).1.len()).collect();
        assert_eq!(counts, vec![2, 2, 1]);
        let (doc, pages) = reopen(&outs[1]);
        assert!(page_text(&doc, pages[0]).contains("Page 3 of A"));
        assert!(page_text(&doc, pages[1]).contains("Page 4 of A"));
        assert_eq!(page_rotation(&doc, pages[0]), 0);
    }

    fn signature_png(w: u32, h: u32) -> String {
        let mut img = image::RgbaImage::new(w, h); // transparent
        for x in 0..w {
            img.put_pixel(x, h / 2, image::Rgba([0, 0, 0, 255]));
        }
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
    }

    #[test]
    fn signs_a_page() {
        let dir = TempDir::new();
        let a = dir.0.join("a.pdf");
        make_pdf(&a, "A", 2);
        let out = dir.0.join("signed.pdf");
        let plan = parse_plan(&json!([{ "src": 0, "page": 0 }, { "src": 0, "page": 1 }])).unwrap();
        let sigs = parse_signatures(&json!([{ "at": 1, "png": signature_png(40, 10), "x": 0.5, "y": 0.8, "w": 0.3 }])).unwrap();
        ok(build(&[a], &plan, &sigs, &out, &|| Ok(())));
        let (doc, pages) = reopen(&out);
        let page = doc.get_dictionary(pages[1]).unwrap();
        let contents = page.get(b"Contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 3);
        let text = page_text(&doc, pages[1]);
        assert!(text.contains("Page 2 of A") && text.contains("/SwiftlaneSignature1 Do"));
        let res = page.get(b"Resources").unwrap().as_dict().unwrap();
        assert!(res.has(b"Font"), "the page's fonts are kept");
        let xo = res.get(b"XObject").unwrap().as_dict().unwrap();
        let img = doc.get_object(xo.get(b"SwiftlaneSignature1").unwrap().as_reference().unwrap()).unwrap().as_stream().unwrap();
        assert_eq!(img.dict.get(b"Width").unwrap().as_i64().unwrap(), 40);
        assert!(img.dict.has(b"SMask"), "transparency is kept");
        // The unsigned page is untouched.
        assert!(!page_text(&doc, pages[0]).contains("Signature"));
        // A broken picture is a plain error.
        let bad = parse_signatures(&json!([{ "at": 0, "png": "data:image/png;base64,AAAA", "x": 0, "y": 0, "w": 0.3 }])).unwrap();
        assert!(matches!(build(&[dir.0.join("a.pdf")], &plan, &bad, &dir.0.join("x.pdf"), &|| Ok(())), Err(JobError::Failed(_))));
    }

    #[test]
    fn signature_lands_where_it_was_shown() {
        let sig = Sig { at: 0, png: String::new(), x: 0.25, y: 0.5, w: 0.5 };
        let page = [0.0, 0.0, 200.0, 100.0]; // landscape box
        // Not turned: top left at (50, 50 from top) = (50, 50); 100 wide, 10 high (image 10:1).
        let m = signature_matrix(page, 0, 100, 10, &sig);
        assert_eq!(m, [100.0, 0.0, 0.0, 10.0, 50.0, 40.0]);
        // Shown turned 90 degrees clockwise: shown 100 wide x 200 high. The picture's width runs
        // along the page's y axis, its height along x (from the shown top, i.e. the page's left).
        let m = signature_matrix(page, 90, 100, 10, &sig);
        // shown left/top = (25, 100), size 50 x 5; bottom left shown = (25, 105) -> page (105, 25)
        assert_eq!(m, [0.0, 50.0, -5.0, 0.0, 105.0, 25.0]);
        let m = signature_matrix(page, 180, 100, 10, &sig);
        assert_eq!(m, [-100.0, 0.0, 0.0, -10.0, 150.0, 60.0]);
        let m = signature_matrix(page, 270, 100, 10, &sig);
        assert_eq!(m, [0.0, -50.0, 5.0, 0.0, 95.0, 75.0]);
    }

    #[test]
    fn picked_signature_is_shrunk() {
        let dir = TempDir::new();
        let p = dir.0.join("sig.png");
        image::RgbaImage::new(3000, 1000).save(&p).unwrap();
        let url = signature_from_file(&p).unwrap();
        let img = signature_image(&url).ok().unwrap();
        assert_eq!(img.dimensions(), (1200, 400));
        std::fs::write(&p, b"nope").unwrap();
        assert!(signature_from_file(&p).is_none());
    }

    /// Only runs where pdfium is next to the test program (it isn't, normally).
    #[test]
    fn draws_pages_when_pdfium_is_there() {
        if engine().is_err() {
            eprintln!("pdfium not found next to the test program: skipping");
            return;
        }
        let dir = TempDir::new();
        let a = dir.0.join("a.pdf");
        make_pdf(&a, "A", 2);
        let thumbs = thumbnails(&a).unwrap();
        assert_eq!(thumbs.len(), 2);
        assert!(thumbs[0]["thumb"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
        assert!(page_image(&a, 1, 900.0).is_some());
        let out = dir.0.join("small.pdf");
        ok(make_smaller(&a, &out, &|_| Ok(())));
        assert_eq!(reopen(&out).1.len(), 2);
    }
}
