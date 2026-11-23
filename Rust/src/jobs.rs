//! Long-running work (conversions, downloads, scans) on background threads, with progress and
//! cancel, exactly like the Python version's Jobs: the page gets "job" events
//! {id, kind, title, status: running|done|cancelled|error, progress, text, outputs}.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

use crate::api::push;
use crate::config;

/// What a job's work gets: report progress, and check whether the user pressed Cancel.
#[derive(Clone)]
pub struct Ctx {
    app: AppHandle,
    id: String,
    kind: String,
    title: String,
    cancel: Arc<AtomicBool>,
}

impl Ctx {
    /// fraction: 0..1 or None (unknown, shows a moving bar); text: optional short status line.
    pub fn report(&self, fraction: Option<f64>, text: Option<&str>) {
        push(&self.app, "job", json!({ "id": self.id, "kind": self.kind, "title": self.title, "status": "running", "progress": fraction, "text": text }));
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// Convenience: Err(Cancelled) to stop the work when the user pressed Cancel.
    pub fn check(&self) -> Result<(), JobError> {
        if self.cancelled() { Err(JobError::Cancelled) } else { Ok(()) }
    }
    #[allow(dead_code)] // for tools still to be ported
    pub fn app(&self) -> &AppHandle {
        &self.app
    }
}

pub enum JobError {
    Cancelled,
    Failed(String), // shown to the user as it is: write plain sentences
}

impl<E: std::fmt::Display> From<E> for JobError {
    fn from(e: E) -> Self {
        JobError::Failed(e.to_string())
    }
}

#[derive(Default)]
pub struct Jobs {
    ids: AtomicU64,
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Jobs {
    /// Starts `work` on its own thread and returns the job id at once. Its Ok(paths) are the
    /// files it made (shown with Open / Show in folder, and remembered for the uninstaller).
    pub fn start<F>(&self, app: &AppHandle, kind: &str, title: &str, notify: bool, work: F) -> String
    where
        F: FnOnce(&Ctx) -> Result<Vec<PathBuf>, JobError> + Send + 'static,
    {
        let id = (self.ids.fetch_add(1, Ordering::Relaxed) + 1).to_string();
        let flag = Arc::new(AtomicBool::new(false));
        self.flags.lock().unwrap().insert(id.clone(), flag.clone());
        let ctx = Ctx { app: app.clone(), id: id.clone(), kind: kind.into(), title: title.into(), cancel: flag };
        std::thread::spawn(move || {
            ctx.report(Some(0.0), Some("Starting…"));
            let base = json!({ "id": ctx.id, "kind": ctx.kind, "title": ctx.title });
            let done = |extra: Value| {
                let mut v = base.clone();
                if let (Value::Object(m), Value::Object(e)) = (&mut v, extra) {
                    m.extend(e);
                }
                push(&ctx.app, "job", v);
            };
            match work(&ctx) {
                Ok(outputs) => {
                    for p in &outputs {
                        crate::transfer::record_created(p);
                    }
                    done(json!({ "status": "done", "progress": 1, "outputs": outputs.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>() }));
                    if notify {
                        crate::notify::show(&ctx.app, "Done", &ctx.title);
                    }
                }
                Err(JobError::Cancelled) => done(json!({ "status": "cancelled" })),
                Err(JobError::Failed(text)) => {
                    config::log(&format!("{} job failed: {text}", ctx.kind));
                    done(json!({ "status": "error", "text": text.chars().take(300).collect::<String>() }));
                }
            }
            if let Some(st) = tauri::Manager::try_state::<crate::api::AppState>(&ctx.app) {
                st.jobs.flags.lock().unwrap().remove(&ctx.id);
            }
        });
        id
    }

    pub fn cancel(&self, id: &str) {
        if let Some(f) = self.flags.lock().unwrap().get(id) {
            f.store(true, Ordering::Relaxed);
        }
    }
}
