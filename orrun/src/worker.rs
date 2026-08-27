//! Central conversion of worker-thread panics into typed application errors.

use engine::{EngineError, EngineResult};
use std::any::Any;
use std::thread::JoinHandle;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{name} worker panicked: {detail}")]
struct WorkerPanic {
    name: &'static str,
    detail: String,
}

pub fn join_worker<T>(name: &'static str, worker: JoinHandle<T>) -> EngineResult<T> {
    worker.join().map_err(|payload| {
        let detail = panic_payload(&payload);
        EngineError::application(WorkerPanic { name, detail })
    })
}

fn panic_payload(payload: &Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else {
        "non-string panic payload".to_owned()
    }
}
