//! Stages, progress and the job runner shared by the GUI and the CLI.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use iris_core::{decisions::Decisions, model::Settings};
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::{config::Config, db::Database};

/// Where a stage reports progress: a terminal bar (CLI) or the GUI.
pub trait ProgressSink: Send + Sync {
    fn start(&self, total: usize);
    fn inc(&self, n: usize);
    fn message(&self, message: &str);
    fn item_failed(&self, photo_id: i64, reason: &str);
}

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// For stages: stop between Photos when cancelled.
    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(!self.is_cancelled(), Cancelled);
        Ok(())
    }
}

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// What a stage gets to work with.
pub struct StageCtx {
    pub config: Config,
    pub db: Database,
    pub decisions: Decisions,
}

impl StageCtx {
    pub fn open(config: &Config) -> Result<Self> {
        let db = Database::open(&config.database_path)?;
        let decisions = Decisions::replay(&db.active_overrides()?);
        Ok(Self { config: config.clone(), db, decisions })
    }

    pub fn settings(&self) -> &Settings {
        &self.decisions.settings
    }
}

pub trait Stage: Send + Sync {
    fn name(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// Bump to make every Photo (or the whole stage) run again.
    fn version(&self) -> i64;
    fn deps(&self) -> &'static [&'static str];
    /// Per-Photo stages are incremental: they only process pending Photos.
    fn per_photo(&self) -> bool;
    /// The settings and decisions this stage depends on; a change makes it stale.
    fn decisions_key(&self, _decisions: &Decisions) -> String {
        String::new()
    }
    /// Optional stages never run as part of "Run all stale".
    fn optional(&self) -> bool {
        false
    }
    /// A question to confirm before running (e.g. what would be sent where, and the cost).
    fn confirm(&self, _db: &Database, _decisions: &Decisions) -> Option<String> {
        None
    }
    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()>;
}

#[derive(Debug, Clone, Serialize)]
pub struct StageStatus {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub per_photo: bool,
    pub done: i64,
    pub failed: i64,
    pub pending: i64,
    pub last_run_ms: Option<i64>,
    pub stale: bool,
    pub stale_reason: Option<String>,
    pub optional: bool,
}

pub fn stage_status(db: &Database, stages: &[Arc<dyn Stage>], stage: &dyn Stage, decisions: &Decisions) -> Result<StageStatus> {
    let run = db.stage_run(stage.name())?;
    let (done, failed, pending) = if stage.per_photo() {
        db.photo_stage_counts(stage.name(), stage.version())?
    } else {
        (0, 0, 0)
    };
    let mut reason = None;
    match &run {
        None => reason = Some("never run".to_string()),
        Some(run) if run.version != stage.version() => reason = Some("new version".into()),
        Some(run) if run.settings_hash != hash(&stage.decisions_key(decisions)) => {
            reason = Some("your changes since".into());
        }
        Some(run) => {
            for dep in stage.deps() {
                if let Some(dep_run) = db.stage_run(dep)?
                    && dep_run.finished_at_ms > run.finished_at_ms
                {
                    let title = stages.iter().find(|s| s.name() == *dep).map_or(*dep, |s| s.title());
                    reason = Some(format!("{title} ran since"));
                    break;
                }
            }
        }
    }
    if reason.is_none() && pending > 0 {
        reason = Some(format!("{pending} new photos"));
    }
    Ok(StageStatus {
        name: stage.name(),
        title: stage.title(),
        description: stage.description(),
        per_photo: stage.per_photo(),
        done,
        failed,
        pending,
        last_run_ms: run.map(|run| run.finished_at_ms),
        stale: reason.is_some() && !stage.optional(),
        stale_reason: reason.filter(|_| !stage.optional()),
        optional: stage.optional(),
    })
}

pub fn hash(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..16].to_string()
}

/// Runs one stage to completion and records it.
pub fn run_stage(ctx: &StageCtx, stage: &dyn Stage, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
    info!(stage = stage.name(), "stage started");
    stage.run(ctx, sink, cancel)?;
    ctx.db.record_stage_run(stage.name(), stage.version(), &hash(&stage.decisions_key(&ctx.decisions)))?;
    info!(stage = stage.name(), "stage finished");
    Ok(())
}

// ---- Terminal progress --------------------------------------------------------------

pub struct TerminalSink(indicatif::ProgressBar);

impl TerminalSink {
    pub fn new(name: &str) -> Self {
        let bar = indicatif::ProgressBar::new(0);
        bar.set_style(
            indicatif::ProgressStyle::with_template(
                "{spinner:.green} {prefix:<10} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} eta {eta} {msg}",
            )
            .expect("valid template")
            .progress_chars("=> "),
        );
        bar.set_prefix(name.to_string());
        Self(bar)
    }
}

impl ProgressSink for TerminalSink {
    fn start(&self, total: usize) {
        self.0.set_length(total as u64);
        self.0.set_position(0);
    }

    fn inc(&self, n: usize) {
        self.0.inc(n as u64);
    }

    fn message(&self, message: &str) {
        self.0.set_message(message.to_string());
    }

    fn item_failed(&self, photo_id: i64, reason: &str) {
        self.0.println(format!("photo {photo_id}: {reason}"));
    }
}

impl Drop for TerminalSink {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

// ---- GUI job runner -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct JobSnapshot {
    pub job_id: i64,
    pub stage: String,
    pub title: String,
    pub done: usize,
    pub total: usize,
    pub failed: usize,
    pub message: String,
    pub started_ms: i64,
    pub eta_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    /// Progress of the running job (throttled).
    Progress(JobSnapshot),
    /// A job was queued, started, finished, failed or cancelled.
    Changed { stage: String },
}

struct Running {
    snapshot: Mutex<JobSnapshot>,
    started: Instant,
    cancel: CancelToken,
    last_emit: Mutex<Instant>,
}

struct Inner {
    config: Config,
    stages: Vec<Arc<dyn Stage>>,
    queue: Mutex<VecDeque<(i64, String)>>,
    wake: Condvar,
    running: Mutex<Option<Arc<Running>>>,
    events: broadcast::Sender<JobEvent>,
}

#[derive(Clone)]
pub struct JobManager(Arc<Inner>);

impl JobManager {
    pub fn start(config: Config, stages: Vec<Arc<dyn Stage>>) -> Result<Self> {
        Database::open(&config.database_path)?.fail_stale_jobs()?;
        let (events, _) = broadcast::channel(256);
        let manager = Self(Arc::new(Inner {
            config,
            stages,
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
            running: Mutex::new(None),
            events,
        }));
        let worker = manager.clone();
        std::thread::Builder::new()
            .name("iris-jobs".into())
            .spawn(move || worker.work())?;
        Ok(manager)
    }

    pub fn stages(&self) -> &[Arc<dyn Stage>] {
        &self.0.stages
    }

    pub fn stage(&self, name: &str) -> Option<Arc<dyn Stage>> {
        self.0
            .stages
            .iter()
            .find(|stage| stage.name() == name)
            .cloned()
            .or_else(|| crate::stages::dynamic_export(name))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.0.events.subscribe()
    }

    pub fn notify(&self, stage: &str) {
        let _ = self.0.events.send(JobEvent::Changed { stage: stage.to_string() });
    }

    pub fn running(&self) -> Option<JobSnapshot> {
        let running = self.0.running.lock().unwrap().clone()?;
        let snapshot = running.snapshot.lock().unwrap().clone();
        Some(snapshot)
    }

    pub fn queued(&self) -> Vec<String> {
        self.0.queue.lock().unwrap().iter().map(|(_, stage)| stage.clone()).collect()
    }

    pub fn is_busy(&self, stage: &str) -> bool {
        self.queued().iter().any(|queued| queued == stage)
            || self.running().is_some_and(|running| running.stage == stage)
    }

    /// Queues a stage unless it is already queued or running.
    pub fn enqueue(&self, stage: &str) -> Result<()> {
        anyhow::ensure!(self.stage(stage).is_some(), "unknown stage {stage}");
        if self.queued().iter().any(|queued| queued == stage) {
            return Ok(());
        }
        let job_id = Database::open(&self.0.config.database_path)?.create_job(stage)?;
        self.0.queue.lock().unwrap().push_back((job_id, stage.to_string()));
        self.0.wake.notify_one();
        self.notify(stage);
        Ok(())
    }

    /// Queues every stale stage, and everything downstream of one, in dependency order.
    pub fn enqueue_stale(&self) -> Result<usize> {
        let db = Database::open(&self.0.config.database_path)?;
        let decisions = Decisions::replay(&db.active_overrides()?);
        let mut dirty: Vec<&str> = Vec::new();
        for stage in self.0.stages.iter().filter(|s| !s.name().starts_with("export:") && !s.optional()) {
            let status = stage_status(&db, &self.0.stages, stage.as_ref(), &decisions)?;
            let upstream_dirty = stage.deps().iter().any(|dep| dirty.contains(dep));
            if status.stale || upstream_dirty {
                dirty.push(stage.name());
            }
        }
        for stage in &dirty {
            self.enqueue(stage)?;
        }
        Ok(dirty.len())
    }

    pub fn cancel(&self, stage: Option<&str>) {
        let mut queue = self.0.queue.lock().unwrap();
        if let Some(stage) = stage {
            queue.retain(|(_, queued)| queued != stage);
        }
        drop(queue);
        if let Some(running) = self.0.running.lock().unwrap().as_ref()
            && stage.is_none_or(|stage| running.snapshot.lock().unwrap().stage == stage)
        {
            running.cancel.cancel();
        }
        self.notify(stage.unwrap_or(""));
    }

    pub fn cancel_all(&self) {
        let removed: Vec<(i64, String)> = self.0.queue.lock().unwrap().drain(..).collect();
        if let Ok(db) = Database::open(&self.0.config.database_path) {
            for (job_id, _) in removed {
                let _ = db.finish_job(job_id, "cancelled", None);
            }
        }
        self.cancel(None);
    }

    fn work(&self) {
        loop {
            let (job_id, stage_name) = {
                let mut queue = self.0.queue.lock().unwrap();
                loop {
                    if let Some(next) = queue.pop_front() {
                        break next;
                    }
                    queue = self.0.wake.wait(queue).unwrap();
                }
            };
            let Some(stage) = self.stage(&stage_name) else { continue };
            let running = Arc::new(Running {
                snapshot: Mutex::new(JobSnapshot {
                    job_id,
                    stage: stage_name.clone(),
                    title: stage.title().to_string(),
                    started_ms: crate::db::now_ms(),
                    ..JobSnapshot::default()
                }),
                started: Instant::now(),
                cancel: CancelToken::default(),
                last_emit: Mutex::new(Instant::now() - Duration::from_secs(1)),
            });
            *self.0.running.lock().unwrap() = Some(running.clone());
            self.notify(&stage_name);

            let outcome = (|| -> Result<()> {
                let ctx = StageCtx::open(&self.0.config)?;
                ctx.db.start_job(job_id)?;
                let sink = ChannelSink { manager: self, running: &running, db: &ctx.db };
                run_stage(&ctx, stage.as_ref(), &sink, &running.cancel)
            })();

            let snapshot = running.snapshot.lock().unwrap().clone();
            if let Ok(db) = Database::open(&self.0.config.database_path) {
                let _ = db.update_job(job_id, snapshot.done as i64, snapshot.total as i64, snapshot.failed as i64, None);
                let _ = match &outcome {
                    Ok(()) => db.finish_job(job_id, "succeeded", None),
                    Err(error) if error.is::<Cancelled>() => db.finish_job(job_id, "cancelled", None),
                    Err(error) => {
                        error!(stage = %stage_name, error = format!("{error:#}"), "stage failed");
                        db.finish_job(job_id, "failed", Some(&format!("{error:#}")))
                    }
                };
            }
            *self.0.running.lock().unwrap() = None;
            self.notify(&stage_name);
        }
    }
}

struct ChannelSink<'a> {
    manager: &'a JobManager,
    running: &'a Running,
    db: &'a Database,
}

impl ChannelSink<'_> {
    fn emit(&self, force: bool) {
        let mut last = self.running.last_emit.lock().unwrap();
        if !force && last.elapsed() < Duration::from_millis(250) {
            return;
        }
        *last = Instant::now();
        let mut snapshot = self.running.snapshot.lock().unwrap();
        let elapsed = self.running.started.elapsed().as_secs_f64();
        snapshot.eta_seconds = (snapshot.done > 0 && snapshot.total > snapshot.done)
            .then(|| (elapsed / snapshot.done as f64 * (snapshot.total - snapshot.done) as f64) as u64);
        let snapshot = snapshot.clone();
        let _ = self.db.update_job(
            snapshot.job_id,
            snapshot.done as i64,
            snapshot.total as i64,
            snapshot.failed as i64,
            Some(&snapshot.message),
        );
        let _ = self.manager.0.events.send(JobEvent::Progress(snapshot));
    }
}

impl ProgressSink for ChannelSink<'_> {
    fn start(&self, total: usize) {
        {
            let mut snapshot = self.running.snapshot.lock().unwrap();
            snapshot.total = total;
            snapshot.done = 0;
        }
        self.emit(true);
    }

    fn inc(&self, n: usize) {
        self.running.snapshot.lock().unwrap().done += n;
        self.emit(false);
    }

    fn message(&self, message: &str) {
        self.running.snapshot.lock().unwrap().message = message.to_string();
        self.emit(true);
    }

    fn item_failed(&self, photo_id: i64, reason: &str) {
        let job_id = {
            let mut snapshot = self.running.snapshot.lock().unwrap();
            snapshot.failed += 1;
            snapshot.job_id
        };
        let _ = self.db.add_job_item(job_id, photo_id, reason);
        self.emit(false);
    }
}
