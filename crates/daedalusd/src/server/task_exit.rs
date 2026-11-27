use std::future::pending;
use tokio::task::{JoinError, JoinHandle};
use tracing::{error, warn};

/// Await a background worker once. An absent handle cannot win a server select.
pub(super) async fn join_worker(worker: &mut Option<JoinHandle<()>>) -> Result<(), JoinError> {
    if let Some(handle) = worker {
        let result = handle.await;
        *worker = None;
        result
    } else {
        pending().await
    }
}

pub(super) fn report_task_exit(
    task: &'static str,
    long_running: bool,
    shutting_down: bool,
    result: Result<(), JoinError>,
) {
    match result {
        Ok(()) if long_running && !shutting_down => {
            warn!(
                task,
                reason = "completed",
                "daemon task exited unexpectedly"
            );
        }
        Err(error) if error.is_panic() => {
            // The panic payload can include paths or data from the task.
            error!(task, reason = "panic", "daemon task exited unexpectedly");
        }
        Err(_) if !shutting_down => {
            warn!(
                task,
                reason = "cancelled",
                "daemon task exited unexpectedly"
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn task_exit_reports_panic_but_not_shutdown_cancellation() {
        let mut panicked = Some(tokio::spawn(async { panic!("private fixture payload") }));
        let cancelled = tokio::spawn(pending::<()>());
        cancelled.abort();
        let mut cancelled = Some(cancelled);
        let panic_result = join_worker(&mut panicked).await;
        let cancel_result = join_worker(&mut cancelled).await;
        assert!(panicked.is_none());
        assert!(cancelled.is_none());

        let captured = Arc::new(Mutex::new(Vec::new()));
        let buffer = Arc::clone(&captured);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || Capture(Arc::clone(&buffer)))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            report_task_exit("widget_scheduler", true, false, panic_result);
            report_task_exit("expected_cleanup", false, true, cancel_result);
            report_task_exit("runtime_monitor", true, false, Ok(()));
        });
        let output = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("widget_scheduler") && output.contains("panic"),
            "{output}"
        );
        assert!(
            output.contains("runtime_monitor") && output.contains("completed"),
            "{output}"
        );
        assert!(!output.contains("expected_cleanup"), "{output}");
        assert!(!output.contains("private fixture payload"), "{output}");
    }
}
