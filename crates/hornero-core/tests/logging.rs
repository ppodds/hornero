//! What hornero logs for failed requests.
//!
//! This binary holds a single test on purpose: `tracing` caches per-callsite interest globally, so
//! other tests hitting the same callsites from parallel threads would race with the thread-local
//! subscriber that captures the output here.

mod common;

use std::sync::{Arc, Mutex};

use common::domain::{DomainApp, TodoErrorFilter};
use common::get;
use hornero_core::Hornero;

/// Collects everything `tracing` writes while the returned guard is alive (current thread only).
fn capture_logs() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Writer(sink.clone()))
        .finish();
    (logs, tracing::subscriber::set_default(subscriber))
}

fn take(logs: &Mutex<Vec<u8>>) -> String {
    String::from_utf8(std::mem::take(&mut *logs.lock().unwrap())).unwrap()
}

#[tokio::test]
async fn only_responses_still_failing_after_filters_are_logged() {
    let router = Hornero::new(DomainApp)
        .filter::<TodoErrorFilter>()
        .build()
        .await
        .unwrap()
        .router();
    let (logs, _guard) = capture_logs();

    // Converted to a 404 by a filter: not an error worth logging.
    get(&router, "/domain/todos/7").await;
    assert_eq!(take(&logs), "");

    // Uncaught: logged once, with the source the client never sees.
    get(&router, "/domain/billing").await;
    let logged = take(&logs);
    assert_eq!(logged.matches("card 4242 declined").count(), 1, "{logged}");
    assert!(logged.contains("ERROR"), "{logged}");

    // Caught but still a 500: the original error is logged.
    get(&router, "/domain/todos/0").await;
    assert!(take(&logs).contains("todo table is locked"));

    get(&router, "/domain/panic").await;
    assert!(take(&logs).contains("handler panicked: boom"));
}
