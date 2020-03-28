//! Logging and error-handling context.

use slog::{OwnedKV, SendSyncRefUnwindSafeKV};
use tokio::process::Child;

use crate::common::*;

/// Context shared by our various asynchronous operations.
#[derive(Debug, Clone)]
pub struct Context {
    /// The logger to use for code in this context.
    log: Logger,
    /// To report asynchronous errors anywhere in the application, send them to
    /// this channel.
    error_sender: mpsc::Sender<Error>,
}

impl Context {
    /// Create a new context, and a future represents our background workers,
    /// returning `()` if they all succeed, or an `Error` as soon as one of them
    /// fails.
    pub fn create(log: Logger) -> (Self, BoxFuture<()>) {
        let (error_sender, mut receiver) = mpsc::channel(1);
        let context = Context { log, error_sender };
        let worker_future = async move {
            match receiver.next().await {
                // All senders have shut down correctly.
                None => Ok(()),
                // We received an error from a background worker, so report that
                // as the result for all our background workers.
                Some(err) => Err(err),
            }
        };
        (context, worker_future.boxed())
    }

    /// Create a new context which can be used from a test case.
    #[cfg(test)]
    pub fn create_for_test(test_name: &str) -> (Self, BoxFuture<()>) {
        use slog::Drain;
        use slog_async::OverflowStrategy;

        let decorator = slog_term::PlainDecorator::new(std::io::stderr());
        let formatted = slog_term::FullFormat::new(decorator).build().fuse();
        let filtered = slog_envlogger::new(formatted);
        let drain = slog_async::Async::new(filtered)
            .chan_size(2)
            // Keep all log entries, at possible performance cost.
            .overflow_strategy(OverflowStrategy::Block)
            .build()
            .fuse();
        let log = Logger::root(drain, o!("test" => test_name.to_owned()));
        Self::create(log)
    }

    /// Get the logger associated with this context.
    pub fn log(&self) -> &Logger {
        &self.log
    }

    /// Create a child context, adding extra `slog` logging context. You can
    /// create the `log_kv` value using `slog`'s `o!` macro.
    pub fn child<T>(&self, log_kv: OwnedKV<T>) -> Self
    where
        T: SendSyncRefUnwindSafeKV + 'static,
    {
        Context {
            log: self.log.new(log_kv),
            error_sender: self.error_sender.clone(),
        }
    }

    /// Spawn an async worker in this context, and report any errors to the
    /// future returned by `create`.
    pub fn spawn_worker<W>(&self, worker: W)
    where
        W: Future<Output = Result<()>> + Send + 'static,
    {
        let log = self.log.clone();
        let mut error_sender = self.error_sender.clone();
        tokio::spawn(
            async move {
                if let Err(err) = worker.await {
                    debug!(log, "reporting background worker error: {}", err);
                    if let Err(_err) = error_sender.send(err).await {
                        debug!(log, "broken pipe reporting background worker error");
                    }
                }
            }
            .boxed(),
        );
    }

    /// Monitor an asynchrnous child process, and report any errors or non-zero
    /// exit codes that occur.
    pub fn spawn_process(&self, name: String, child: Child) {
        let worker = async move {
            match child.await {
                Ok(ref status) if status.success() => Ok(()),
                Ok(status) => Err(format_err!("{} failed with {}", name, status)),
                Err(err) => Err(format_err!("{} failed with error: {}", name, err)),
            }
        };
        self.spawn_worker(worker.boxed());
    }
}