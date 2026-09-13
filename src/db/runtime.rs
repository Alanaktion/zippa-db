//! Tokio runtime used for database work.
//!
//! sqlx needs a Tokio reactor, which GPUI's executor is not. Every database
//! future therefore runs on this dedicated runtime and its result travels back
//! to the UI over a oneshot channel that GPUI can await.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use tokio::runtime::{Builder, Runtime};
use tokio::sync::oneshot;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("zippa-db")
            .build()
            .expect("failed to start the database runtime")
    })
}

/// A database future that is on its way back to the UI.
///
/// Awaiting it yields what the future returned. Giving up on it — the user
/// cancelling a long query — drops the work on the runtime, which is what
/// closes the connection sqlx was using and lets the pool open a fresh one.
pub struct Task<T> {
    receiver: oneshot::Receiver<T>,
    /// `None` under test, where the work has already run to completion.
    running: Option<tokio::task::JoinHandle<()>>,
}

impl<T> Task<T> {
    /// A handle that can stop the work after the task itself has been moved
    /// into whatever is awaiting it.
    ///
    /// `None` under test, where the work has already run.
    pub fn abort_handle(&self) -> Option<tokio::task::AbortHandle> {
        self.running.as_ref().map(|running| running.abort_handle())
    }
}

impl<T> Future for Task<T> {
    type Output = Result<T, oneshot::error::RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(cx)
    }
}

/// Run `future` on the database runtime.
///
/// Await the returned receiver from a GPUI task:
///
/// ```ignore
/// let task = db::runtime::spawn(async move { db::connect(&config, None).await });
/// cx.spawn(async move |this, cx| {
///     let result = task.await;
/// })
/// .detach();
/// ```
#[cfg(not(test))]
pub fn spawn<F>(future: F) -> Task<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let running = runtime().spawn(async move {
        let output = future.await;
        // The receiver is dropped when the UI stops caring about the result.
        let _ = tx.send(output);
    });

    Task {
        receiver: rx,
        running: Some(running),
    }
}

/// Under test the future runs to completion on the calling thread.
///
/// GPUI's test scheduler is single-threaded and aborts the process if one of
/// its tasks is woken from another thread, which is exactly what a background
/// runtime does when its result arrives. Running the work inline keeps every
/// wake-up on the test thread, and makes database results deterministic.
#[cfg(test)]
pub fn spawn<F>(future: F) -> Task<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let _ = tx.send(runtime().block_on(future));

    Task {
        receiver: rx,
        running: None,
    }
}

/// Block the current thread on `future`. Tests only: the UI never blocks.
#[cfg(test)]
pub fn block_on<F: Future>(future: F) -> F::Output {
    runtime().block_on(future)
}
