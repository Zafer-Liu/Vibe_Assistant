//! In-memory automatic extraction queue. Only claimed work gets a task; hooks
//! and retries never spawn waiters. A running session keeps its queue entry so
//! later hooks cannot start another extraction or replace a failed batch.
use super::SessionBuffer;
use crate::telemetry_store::L1MemoryCandidate;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub(super) const AUTOMATIC_CONCURRENCY: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryClass {
    Configuration,
    Transient,
    Contract,
    LocalWrite,
    // The model may have completed, but the facade could not save its trace.
    // It deliberately withholds the response: only an explicit UI retry may
    // send it again, never a timer or provider configuration change.
    UncertainOutcome,
}

impl RetryClass {
    fn delay(self, attempts: u32) -> Duration {
        let (base, cap) = match self {
            Self::Configuration => (1_800u64, 1_800),
            Self::Transient => (15, 900),
            Self::Contract => (300, 3_600),
            Self::LocalWrite => (30, 300),
            Self::UncertainOutcome => (0, 0), // no automatic due time
        };
        Duration::from_secs(base.saturating_mul(1u64 << attempts.saturating_sub(1).min(16)).min(cap))
    }
}

pub(super) struct PreparedMemories {
    pub candidates: Vec<L1MemoryCandidate>,
    pub provider_name: String,
}

pub(super) struct AutomaticWork {
    pub session_id: String,
    pub buffer: SessionBuffer,
    pub prepared: Option<PreparedMemories>,
    generation: u64,
}

struct Retry {
    class: RetryClass,
    attempts: u32,
    at: Instant,
}

#[derive(Default)]
struct Session {
    pending: Option<SessionBuffer>,
    completed: VecDeque<SessionBuffer>,
    retained: Option<AutomaticWork>,
    running: Option<String>,
    retry: Option<Retry>,
    order: u64,
}

#[derive(Default)]
pub(super) struct AutomaticScheduler {
    sessions: HashMap<String, Session>,
    active: usize,
    order: u64,
    generation: u64,
    configuration_blocked_until: Option<Instant>,
}

impl AutomaticScheduler {
    pub fn buffer(&mut self, id: &str, agent: &str, source: &str, now: Instant) -> &mut SessionBuffer {
        let session = self.sessions.entry(id.to_string()).or_default();
        session.pending.get_or_insert_with(|| SessionBuffer {
            agent_id: agent.to_string(),
            source: source.to_string(),
            messages: Vec::new(),
            event_key: None,
            last_active: now,
        })
    }

    pub fn take_pending(&mut self, id: &str) -> Option<SessionBuffer> {
        let session = self.sessions.get_mut(id)?;
        let pending = session.pending.take();
        if session.running.is_none() && session.retained.is_none() && session.completed.is_empty() {
            self.sessions.remove(id);
        }
        pending
    }

    pub fn restore_pending(&mut self, id: &str, mut previous: SessionBuffer) {
        let session = self.sessions.entry(id.to_string()).or_default();
        if let Some(current) = session.pending.take() {
            previous.messages.extend(current.messages);
            previous.last_active = current.last_active;
        }
        session.pending = Some(previous);
    }

    fn enqueue_receipt(session: &mut Session, buffer: SessionBuffer) -> bool {
        let Some(key) = buffer.event_key.as_deref() else { return false };
        let duplicate = session.running.as_deref() == Some(key)
            || session.retained.as_ref().is_some_and(|w| w.buffer.event_key.as_deref() == Some(key))
            || session.completed.iter().any(|b| b.event_key.as_deref() == Some(key));
        if !duplicate && !buffer.messages.is_empty() {
            session.completed.push_back(buffer);
        }
        !duplicate
    }

    pub fn enqueue(&mut self, id: &str, buffer: SessionBuffer) -> bool {
        if buffer.messages.is_empty() {
            return true;
        }
        if buffer.event_key.is_none() {
            self.restore_pending(id, buffer);
            return true;
        }
        // Distinct Stop receipts keep distinct evidence and source identities.
        // Only the bounded dispatcher turns these queued buffers into tasks.
        Self::enqueue_receipt(self.sessions.entry(id.to_string()).or_default(), buffer)
    }

    fn complete_pending(session: &mut Session) {
        if session.pending.as_ref().is_some_and(|b| b.event_key.is_some()) {
            let buffer = session.pending.take().unwrap();
            Self::enqueue_receipt(session, buffer);
        }
    }

    pub fn request_all(&mut self) -> usize {
        // This is an explicit UI flush, so it may bypass automatic backoff.
        self.configuration_blocked_until = None;
        let mut count = 0;
        for session in self.sessions.values_mut() {
            Self::complete_pending(session);
            if session.retained.is_some() || !session.completed.is_empty() {
                session.retry = None;
                count += 1;
            }
        }
        count
    }

    pub fn buffered_sessions(&self) -> usize {
        self.sessions.values().filter(|s| s.retained.is_some() || !s.completed.is_empty() || s.pending.is_some()).count()
    }

    fn due(&self, session: &Session) -> Option<Instant> {
        if session.running.is_some() || session.retry.as_ref().is_some_and(|r| r.class == RetryClass::UncertainOutcome) {
            return None;
        }
        let (mut at, prepared) = if let Some(work) = &session.retained {
            (session.retry.as_ref().map(|r| r.at).unwrap_or(work.buffer.last_active), work.prepared.is_some())
        } else {
            // Hook-only idle buffers have no complete receipt. Leave them for
            // a later Stop or native/manual recovery, without a due-time spin.
            (session.completed.front()?.last_active, false)
        };
        // A missing/invalid provider is shared by every session. Probe at most
        // once per cooldown (up to the already claimed pair), not per session.
        if !prepared {
            if let Some(blocked) = self.configuration_blocked_until {
                at = at.max(blocked);
            }
        }
        Some(at)
    }

    pub fn take_ready(&mut self, now: Instant, _idle: Duration) -> Option<AutomaticWork> {
        if self.active >= AUTOMATIC_CONCURRENCY {
            return None;
        }
        let id = self.sessions.iter()
            .filter_map(|(id, session)| self.due(session).filter(|at| *at <= now).map(|at| (id, at, session.order)))
            .min_by(|a, b| (a.1, a.2, a.0).cmp(&(b.1, b.2, b.0)))
            .map(|(id, _, _)| id.clone())?;
        let session = self.sessions.get_mut(&id).unwrap();
        let mut work = if let Some(work) = session.retained.take() {
            work
        } else {
            AutomaticWork {
                session_id: id,
                buffer: session.completed.pop_front().unwrap(),
                prepared: None,
                generation: self.generation,
            }
        };
        work.generation = self.generation;
        session.running = work.buffer.event_key.clone();
        self.order = self.order.wrapping_add(1);
        session.order = self.order;
        self.active += 1;
        Some(work)
    }

    pub fn finish(&mut self, work: AutomaticWork, failure: Option<RetryClass>, now: Instant) {
        let session = self.sessions.get_mut(&work.session_id).expect("claimed session remains registered");
        session.running = None;
        self.active -= 1;
        if let Some(class) = failure {
            let changed = work.generation != self.generation
                && !matches!(class, RetryClass::LocalWrite | RetryClass::UncertainOutcome);
            let attempts = if changed { 1 } else {
                session.retry.as_ref().filter(|r| r.class == class).map(|r| r.attempts.saturating_add(1)).unwrap_or(1)
            };
            let at = if changed { now } else { now + class.delay(attempts) };
            session.retry = Some(Retry { class, attempts, at });
            if class == RetryClass::Configuration && !changed {
                self.configuration_blocked_until = Some(at);
            }
            session.retained = Some(work);
        } else {
            session.retry = None;
            if session.pending.is_none() && session.completed.is_empty() {
                self.sessions.remove(&work.session_id);
            }
        }
    }

    pub fn configuration_changed(&mut self, now: Instant) {
        self.generation = self.generation.wrapping_add(1);
        self.configuration_blocked_until = None;
        for session in self.sessions.values_mut() {
            if let Some(retry) = session.retry.as_mut() {
                if !matches!(retry.class, RetryClass::LocalWrite | RetryClass::UncertainOutcome) {
                    retry.at = now;
                    retry.attempts = 0;
                }
            }
        }
    }

    pub fn next_delay(&self, now: Instant, _idle: Duration, poll: Duration) -> Duration {
        if self.active >= AUTOMATIC_CONCURRENCY {
            return poll;
        }
        self.sessions.values().filter_map(|s| self.due(s))
            .map(|at| at.saturating_duration_since(now)).min().unwrap_or(poll).min(poll)
    }
}

#[cfg(test)]
mod tests;
