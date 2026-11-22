//! Operation-local diagnostics. Only static labels and numeric counters cross this boundary.

use crate::http::Submission;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct OperationTrace(Arc<Trace>);

struct Trace {
    operation_id: String,
    operation: &'static str,
    start_unix_ms: u128,
    start: Instant,
    state: Mutex<State>,
}

struct State {
    previous: Instant,
    last_stage: &'static str,
    submission: Submission,
    finished: bool,
    #[cfg(test)]
    events: Arc<Mutex<Vec<Value>>>,
}

impl OperationTrace {
    pub(crate) fn new(operation: &'static str) -> Self {
        let start = Instant::now();
        let trace = Self(Arc::new(Trace {
            operation_id: format!(
                "{}-{}",
                std::process::id(),
                NEXT_OPERATION.fetch_add(1, Ordering::Relaxed)
            ),
            operation,
            start_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_millis(),
            start,
            state: Mutex::new(State {
                previous: start,
                last_stage: "operation_start",
                submission: Submission::NotSubmitted,
                finished: false,
                #[cfg(test)]
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        }));
        trace.mark("operation_start");
        trace
    }

    /// elapsed_ms is cumulative; delta_ms is the gap since the previous marker, not a nested span.
    pub(crate) fn mark(&self, stage: &'static str) {
        self.counts(stage, &[]);
    }

    pub(crate) fn counts(&self, stage: &'static str, counts: &[(&'static str, usize)]) {
        let mut state = self.0.state.lock().expect("operation trace");
        let mut event = self.event(&mut state, stage);
        for (key, value) in counts {
            event[*key] = json!(*value);
        }
        state.last_stage = stage;
        Self::emit(&mut state, event);
    }

    pub(crate) fn submission(&self, submission: Submission) {
        self.0.state.lock().expect("operation trace").submission = submission;
    }

    pub(crate) fn finish(&self, status: &'static str, submission: Submission, code: &'static str) {
        let mut state = self.0.state.lock().expect("operation trace");
        state.finished = true;
        state.submission = submission;
        let mut event = self.event(&mut state, "operation_end");
        event["last_stage"] = json!(state.last_stage);
        event["status"] = json!(status);
        event["submission"] = json!(submission);
        event["code"] = json!(code);
        Self::emit(&mut state, event);
    }

    fn event(&self, state: &mut State, stage: &'static str) -> Value {
        let now = Instant::now();
        let event = json!({
            "diagnostic": "tokemula_image_performance",
            "operation_id": self.0.operation_id,
            "operation": self.0.operation,
            "start_unix_ms": self.0.start_unix_ms,
            "stage": stage,
            "elapsed_ms": now.duration_since(self.0.start).as_millis(),
            "delta_ms": now.duration_since(state.previous).as_millis(),
        });
        state.previous = now;
        event
    }

    fn emit(state: &mut State, event: Value) {
        eprintln!("{event}");
        #[cfg(test)]
        state.events.lock().expect("trace events").push(event);
        #[cfg(not(test))]
        let _ = state;
    }

    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<Value> {
        self.0
            .state
            .lock()
            .expect("operation trace")
            .events
            .lock()
            .expect("trace events")
            .clone()
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        let state = self.state.get_mut().expect("operation trace");
        if !state.finished {
            let now = Instant::now();
            let event = json!({
                "diagnostic": "tokemula_image_performance",
                "operation_id": self.operation_id,
                "operation": self.operation,
                "start_unix_ms": self.start_unix_ms,
                "stage": "operation_end",
                "elapsed_ms": now.duration_since(self.start).as_millis(),
                "delta_ms": now.duration_since(state.previous).as_millis(),
                "last_stage": state.last_stage,
                "status": "cancelled",
                "submission": state.submission,
                "code": "cancelled",
            });
            OperationTrace::emit(state, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_last_handle_records_cancellation_with_the_last_submission() {
        let trace = OperationTrace::new("synthetic");
        let captured = trace.0.state.lock().unwrap().events.clone();
        let child = trace.clone();
        drop(child);
        assert_eq!(trace.events().len(), 1);
        trace.submission(Submission::Unknown);
        trace.mark("post_start");
        drop(trace);
        let events = captured.lock().unwrap();
        assert_eq!(events.last().unwrap()["status"], "cancelled");
        assert_eq!(events.last().unwrap()["last_stage"], "post_start");
        assert_eq!(events.last().unwrap()["submission"], "unknown");
    }

    #[test]
    fn markers_share_identity_and_keep_cumulative_and_gap_clocks_distinct() {
        let trace = OperationTrace::new("synthetic");
        trace.clone().counts("body_read", &[("bytes", 32)]);
        trace.finish("error", Submission::Unknown, "invalid_image");
        let events = trace.events();
        assert_eq!(events.len(), 3);
        for event in &events {
            assert_eq!(event["operation_id"], events[0]["operation_id"]);
            assert_eq!(event["start_unix_ms"], events[0]["start_unix_ms"]);
            assert!(event["elapsed_ms"].as_u64().unwrap() >= event["delta_ms"].as_u64().unwrap());
            assert!(event.get("prompt").is_none());
            assert!(event.get("path").is_none());
        }
        assert!(
            events
                .windows(2)
                .all(|pair| pair[1]["elapsed_ms"].as_u64() >= pair[0]["elapsed_ms"].as_u64())
        );
        assert_eq!(events[2]["last_stage"], "body_read");
        assert_eq!(events[2]["submission"], "unknown");
        assert_eq!(events[2]["code"], "invalid_image");
    }
}
