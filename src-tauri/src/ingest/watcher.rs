//! Watches each service's folders and re-indexes (incrementally) when files change.

use super::{is_supported, IndexManager};
use crate::services::Service;
use notify::{EventKind, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

pub struct Watchers {
    inner: Mutex<HashMap<String, notify::RecommendedWatcher>>,
    tx: UnboundedSender<String>,
}

impl Watchers {
    pub fn new(app: AppHandle, indexer: Arc<IndexManager>) -> Arc<Self> {
        let (tx, mut rx) = unbounded_channel::<String>();
        tauri::async_runtime::spawn(async move {
            while let Some(first) = rx.recv().await {
                // debounce: editors save in several steps
                let mut ids = HashSet::from([first]);
                tokio::time::sleep(Duration::from_secs(3)).await;
                while let Ok(more) = rx.try_recv() {
                    ids.insert(more);
                }
                for id in ids {
                    indexer.start(app.clone(), id, false);
                }
            }
        });
        Arc::new(Self { inner: Mutex::new(HashMap::new()), tx })
    }

    pub fn watch(&self, svc: &Service) {
        self.unwatch(&svc.id);
        if svc.folders.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let id = svc.id.clone();
        let handler = move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            if ev.paths.iter().any(|p| is_supported(p)) {
                let _ = tx.send(id.clone());
            }
        };
        let Ok(mut w) = notify::recommended_watcher(handler) else {
            log::warn!("cannot create file watcher for service {}", svc.id);
            return;
        };
        for f in &svc.folders {
            let p = Path::new(f);
            if p.is_dir() {
                if let Err(e) = w.watch(p, RecursiveMode::Recursive) {
                    log::warn!("watch {f} failed: {e}");
                }
            }
        }
        self.inner.lock().insert(svc.id.clone(), w);
    }

    pub fn unwatch(&self, service_id: &str) {
        self.inner.lock().remove(service_id);
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}
