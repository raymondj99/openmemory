use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use openmemory_admin::{
    AdminErrorCode, AdminEvent, AdminEventType, AdminJob, AdminJobState, AdminLogEntry,
    AdminLogLevel, ComponentHealth,
};
use tokio::sync::{broadcast, watch};

use crate::product_store::{ProductStore, ProductStoreError};
use crate::{redact_log_text, redact_log_value, unix_now_secs, AdminToken, DaemonConfig};

#[derive(Clone)]
pub(crate) struct AdminState {
    pub(crate) token: Arc<RwLock<AdminToken>>,
    pub(crate) token_generation: watch::Sender<u64>,
    pub(crate) config: DaemonConfig,
    pub(crate) logs: Arc<RedactedLogRing>,
    pub(crate) jobs: Arc<JobRegistry>,
    pub(crate) shutdown: Option<watch::Sender<bool>>,
}

#[derive(Debug)]
pub(crate) struct RedactedLogRing {
    inner: Mutex<LogRingInner>,
    capacity: usize,
}

#[derive(Debug)]
struct LogRingInner {
    next_sequence: u64,
    entries: VecDeque<AdminLogEntry>,
}

impl RedactedLogRing {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(LogRingInner {
                next_sequence: 1,
                entries: VecDeque::with_capacity(capacity),
            }),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn push(
        &self,
        level: AdminLogLevel,
        event: impl Into<String>,
        message: impl Into<String>,
        details: serde_json::Value,
    ) {
        let unix_secs = unix_now_secs().unwrap_or(0);
        let mut details = details;
        redact_log_value(&mut details);

        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry = AdminLogEntry {
            sequence: inner.next_sequence,
            unix_secs,
            level,
            event: event.into(),
            message: redact_log_text(&message.into()),
            details,
        };
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        if inner.entries.len() == self.capacity {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }

    pub(crate) fn snapshot(&self) -> Vec<AdminLogEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .cloned()
            .collect()
    }
}

#[derive(Debug)]
pub(crate) struct JobRegistry {
    inner: Mutex<JobRegistryInner>,
    event_order: Mutex<()>,
    events: broadcast::Sender<AdminEvent>,
    store: Option<ProductStore>,
    durability_error: Mutex<Option<String>>,
}

#[derive(Debug)]
struct JobRegistryInner {
    next_event_sequence: u64,
    jobs: HashMap<String, AdminJob>,
}

impl JobRegistry {
    pub(crate) fn open(home: &Path) -> Self {
        let (events, _) = broadcast::channel(256);
        let mut durability_error = None;
        let (store, jobs, next_event_sequence) = match ProductStore::open(home) {
            Ok(store) => match load_durable_state(&store) {
                Ok((jobs, next_event_sequence)) => (Some(store), jobs, next_event_sequence),
                Err(error) => {
                    durability_error = Some(error.to_string());
                    (None, Vec::new(), 1)
                }
            },
            Err(error) => {
                durability_error = Some(error.to_string());
                (None, Vec::new(), 1)
            }
        };
        Self {
            inner: Mutex::new(JobRegistryInner {
                next_event_sequence,
                jobs: jobs.into_iter().map(|job| (job.id.clone(), job)).collect(),
            }),
            event_order: Mutex::new(()),
            events,
            store,
            durability_error: Mutex::new(durability_error),
        }
    }

    pub(crate) fn insert(&self, job: AdminJob) -> AdminJob {
        let _event_order = self.event_order.lock().unwrap_or_else(|e| e.into_inner());
        let event = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.jobs.insert(job.id.clone(), job.clone());
            Self::event_for_job_locked(&mut inner, job.clone())
        };
        self.persist_job(&job);
        self.persist_event(&event);
        let _ = self.events.send(event);
        job
    }

    pub(crate) fn update(&self, id: &str, update: impl FnOnce(&mut AdminJob)) -> Option<AdminJob> {
        let _event_order = self.event_order.lock().unwrap_or_else(|e| e.into_inner());
        let (job, event) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let job = inner.jobs.get_mut(id)?;
            update(job);
            let job = job.clone();
            let event = Self::event_for_job_locked(&mut inner, job.clone());
            (job, event)
        };
        self.persist_job(&job);
        self.persist_event(&event);
        let _ = self.events.send(event);
        Some(job)
    }

    pub(crate) fn get(&self, id: &str) -> Option<AdminJob> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jobs
            .get(id)
            .cloned()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.events.subscribe()
    }

    pub(crate) fn events_after(&self, sequence: u64, limit: usize) -> Vec<AdminEvent> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        match store.events_after(sequence, limit) {
            Ok(events) => events,
            Err(error) => {
                self.record_store_error(error);
                Vec::new()
            }
        }
    }

    pub(crate) fn health(&self) -> ComponentHealth {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let queued = inner
            .jobs
            .values()
            .filter(|job| job.state == AdminJobState::Queued)
            .count();
        let running = inner
            .jobs
            .values()
            .filter(|job| job.state == AdminJobState::Running)
            .count();
        let failed = inner
            .jobs
            .values()
            .filter(|job| job.state == AdminJobState::Failed)
            .count();
        let total = inner.jobs.len();
        let next_event_sequence = inner.next_event_sequence;
        drop(inner);

        let durability_error = self
            .durability_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let durable = self.store.is_some() && durability_error.is_none();
        let details = serde_json::json!({
            "durable": durable,
            "jobs": total,
            "queued": queued,
            "running": running,
            "failed": failed,
            "next_event_sequence": next_event_sequence,
            "store_path": self.store.as_ref().map(|store| store.path().display().to_string()),
        });

        if let Some(error) = durability_error {
            ComponentHealth::error(
                AdminErrorCode::StoreUnreadable,
                "job registry persistence is unavailable",
            )
            .with_details(merge_json(details, serde_json::json!({ "error": error })))
        } else {
            ComponentHealth::ok("job registry is durable").with_details(details)
        }
    }

    fn event_for_job_locked(inner: &mut JobRegistryInner, job: AdminJob) -> AdminEvent {
        let event = AdminEvent {
            sequence: inner.next_event_sequence,
            unix_secs: unix_now_secs().unwrap_or(0),
            event_type: AdminEventType::JobUpdated,
            job: Some(job),
            message: None,
        };
        inner.next_event_sequence = inner.next_event_sequence.saturating_add(1);
        event
    }

    fn persist_job(&self, job: &AdminJob) {
        if let Some(store) = &self.store {
            if let Err(error) = store.upsert_job(job) {
                self.record_store_error(error);
            }
        }
    }

    fn persist_event(&self, event: &AdminEvent) {
        if let Some(store) = &self.store {
            if let Err(error) = store.insert_event(event) {
                self.record_store_error(error);
            }
        }
    }

    fn record_store_error(&self, error: ProductStoreError) {
        *self
            .durability_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
    }
}

fn load_durable_state(store: &ProductStore) -> Result<(Vec<AdminJob>, u64), ProductStoreError> {
    let jobs = store.load_jobs()?;
    let next_event_sequence = store.next_event_sequence()?;
    Ok((jobs, next_event_sequence))
}

fn merge_json(mut left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        for (key, value) in right {
            left.insert(key.clone(), value.clone());
        }
    }
    left
}
