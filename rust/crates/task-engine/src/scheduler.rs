use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{AdmissionRequest, Result, SchedulerLimits, SchedulerSnapshot, TaskEngineError};

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    limits: SchedulerLimits,
    state: Mutex<SchedulerState>,
    notify: Notify,
}

#[derive(Default)]
struct SchedulerState {
    closed: bool,
    active_workers: usize,
    active_global: usize,
    rooms: HashMap<String, usize>,
    members: HashMap<String, usize>,
    providers: HashMap<String, usize>,
    profiles: HashMap<String, usize>,
    tasks: HashMap<String, usize>,
    waiters: VecDeque<Waiter>,
}

#[derive(Clone)]
struct Waiter {
    ticket: String,
    request: AdmissionRequest,
}

impl Scheduler {
    pub fn new(limits: SchedulerLimits) -> Result<Self> {
        if [
            limits.max_workers,
            limits.max_global,
            limits.max_per_room,
            limits.max_per_member,
            limits.max_per_provider,
            limits.max_per_profile,
            limits.max_per_task,
        ]
        .contains(&0)
        {
            return Err(TaskEngineError::Invalid(
                "all scheduler limits must be greater than zero".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(SchedulerInner {
                limits,
                state: Mutex::new(SchedulerState::default()),
                notify: Notify::new(),
            }),
        })
    }

    pub async fn admit(
        &self,
        request: AdmissionRequest,
        cancellation: CancellationToken,
    ) -> Result<AdmissionLease> {
        validate_request(&request)?;
        if cancellation.is_cancelled() {
            return Err(TaskEngineError::Cancelled);
        }
        let ticket = Uuid::new_v4().to_string();
        {
            let mut state = self.inner.state.lock().expect("scheduler state poisoned");
            if state.closed {
                return Err(TaskEngineError::SchedulerClosed);
            }
            state.waiters.push_back(Waiter {
                ticket: ticket.clone(),
                request: request.clone(),
            });
        }
        self.inner.notify.notify_waiters();

        loop {
            let notified = self.inner.notify.notified();
            let decision = {
                let mut state = self.inner.state.lock().expect("scheduler state poisoned");
                if state.closed {
                    remove_waiter(&mut state, &ticket);
                    Some(Err(TaskEngineError::SchedulerClosed))
                } else if cancellation.is_cancelled() {
                    remove_waiter(&mut state, &ticket);
                    Some(Err(TaskEngineError::Cancelled))
                } else {
                    let selected = state
                        .waiters
                        .iter()
                        .find(|waiter| can_admit(&state, &self.inner.limits, &waiter.request))
                        .map(|waiter| waiter.ticket.clone());
                    if selected.as_deref() == Some(ticket.as_str()) {
                        remove_waiter(&mut state, &ticket);
                        occupy(&mut state, &request);
                        Some(Ok(AdmissionLease {
                            inner: Arc::clone(&self.inner),
                            request: Some(request.clone()),
                        }))
                    } else {
                        None
                    }
                }
            };
            if let Some(decision) = decision {
                self.inner.notify.notify_waiters();
                return decision;
            }

            tokio::select! {
                () = cancellation.cancelled() => {
                    let mut state = self.inner.state.lock().expect("scheduler state poisoned");
                    remove_waiter(&mut state, &ticket);
                    drop(state);
                    self.inner.notify.notify_waiters();
                    return Err(TaskEngineError::Cancelled);
                }
                () = notified => {}
            }
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> SchedulerSnapshot {
        let state = self.inner.state.lock().expect("scheduler state poisoned");
        SchedulerSnapshot {
            active_workers: state.active_workers,
            active_global: state.active_global,
            waiting: state.waiters.len(),
        }
    }

    pub fn close(&self) {
        self.inner
            .state
            .lock()
            .expect("scheduler state poisoned")
            .closed = true;
        self.inner.notify.notify_waiters();
    }
}

pub struct AdmissionLease {
    inner: Arc<SchedulerInner>,
    request: Option<AdmissionRequest>,
}

impl AdmissionLease {
    #[must_use]
    pub fn request(&self) -> &AdmissionRequest {
        self.request
            .as_ref()
            .expect("admission lease request is present until drop")
    }
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        let Some(request) = self.request.take() else {
            return;
        };
        let mut state = self.inner.state.lock().expect("scheduler state poisoned");
        release(&mut state, &request);
        drop(state);
        self.inner.notify.notify_waiters();
    }
}

fn validate_request(request: &AdmissionRequest) -> Result<()> {
    for (field, value) in [
        ("request_id", request.request_id.as_str()),
        ("task_run_id", request.task_run_id.as_str()),
        ("provider", request.provider.as_str()),
        ("profile", request.profile.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(TaskEngineError::Invalid(format!(
                "admission {field} must not be empty"
            )));
        }
    }
    Ok(())
}

fn can_admit(state: &SchedulerState, limits: &SchedulerLimits, request: &AdmissionRequest) -> bool {
    state.active_workers < limits.max_workers
        && state.active_global < limits.max_global
        && below_optional(
            &state.rooms,
            request.room_id.as_deref(),
            limits.max_per_room,
        )
        && below_optional(
            &state.members,
            request.member_id.as_deref(),
            limits.max_per_member,
        )
        && below(&state.providers, &request.provider, limits.max_per_provider)
        && below(&state.profiles, &request.profile, limits.max_per_profile)
        && below(&state.tasks, &request.task_run_id, limits.max_per_task)
}

fn below(counts: &HashMap<String, usize>, key: &str, limit: usize) -> bool {
    counts.get(key).copied().unwrap_or_default() < limit
}

fn below_optional(counts: &HashMap<String, usize>, key: Option<&str>, limit: usize) -> bool {
    key.is_none_or(|key| below(counts, key, limit))
}

fn occupy(state: &mut SchedulerState, request: &AdmissionRequest) {
    state.active_workers += 1;
    state.active_global += 1;
    increment_optional(&mut state.rooms, request.room_id.as_deref());
    increment_optional(&mut state.members, request.member_id.as_deref());
    increment(&mut state.providers, &request.provider);
    increment(&mut state.profiles, &request.profile);
    increment(&mut state.tasks, &request.task_run_id);
}

fn release(state: &mut SchedulerState, request: &AdmissionRequest) {
    state.active_workers = state.active_workers.saturating_sub(1);
    state.active_global = state.active_global.saturating_sub(1);
    decrement_optional(&mut state.rooms, request.room_id.as_deref());
    decrement_optional(&mut state.members, request.member_id.as_deref());
    decrement(&mut state.providers, &request.provider);
    decrement(&mut state.profiles, &request.profile);
    decrement(&mut state.tasks, &request.task_run_id);
}

fn increment(counts: &mut HashMap<String, usize>, key: &str) {
    *counts.entry(key.into()).or_default() += 1;
}

fn increment_optional(counts: &mut HashMap<String, usize>, key: Option<&str>) {
    if let Some(key) = key {
        increment(counts, key);
    }
}

fn decrement(counts: &mut HashMap<String, usize>, key: &str) {
    let Some(count) = counts.get_mut(key) else {
        return;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        counts.remove(key);
    }
}

fn decrement_optional(counts: &mut HashMap<String, usize>, key: Option<&str>) {
    if let Some(key) = key {
        decrement(counts, key);
    }
}

fn remove_waiter(state: &mut SchedulerState, ticket: &str) {
    if let Some(index) = state
        .waiters
        .iter()
        .position(|waiter| waiter.ticket == ticket)
    {
        state.waiters.remove(index);
    }
}
