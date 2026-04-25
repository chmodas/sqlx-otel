//! Runtime abstraction for spawning background tasks.
//!
//! The pool metrics background polling task needs a way to spawn an async task and sleep
//! for a duration. This module provides a trait that can be implemented for different
//! async runtimes.

#[cfg(any(feature = "runtime-tokio", feature = "runtime-async-std"))]
mod inner {
    use std::future::Future;
    use std::time::Duration;

    /// Minimal runtime abstraction for spawning background tasks and sleeping.
    pub(crate) trait Runtime: Send + Sync + 'static {
        /// Spawn a `Send + 'static` future as a background task.
        fn spawn(fut: impl Future<Output = ()> + Send + 'static);

        /// Sleep for the given duration.
        fn sleep(duration: Duration) -> impl Future<Output = ()> + Send;
    }

    #[cfg(feature = "runtime-tokio")]
    pub(crate) struct TokioRuntime;

    #[cfg(feature = "runtime-tokio")]
    impl Runtime for TokioRuntime {
        fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
            tokio::spawn(fut);
        }

        fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
            tokio::time::sleep(duration)
        }
    }

    #[cfg(all(feature = "runtime-async-std", not(feature = "runtime-tokio")))]
    pub(crate) struct AsyncStdRuntime;

    #[cfg(all(feature = "runtime-async-std", not(feature = "runtime-tokio")))]
    impl Runtime for AsyncStdRuntime {
        fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
            async_std::task::spawn(fut);
        }

        fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
            async_std::task::sleep(duration)
        }
    }
}

#[cfg(any(feature = "runtime-tokio", feature = "runtime-async-std"))]
pub(crate) use inner::Runtime;

#[cfg(feature = "runtime-tokio")]
pub(crate) use inner::TokioRuntime;

#[cfg(all(feature = "runtime-async-std", not(feature = "runtime-tokio")))]
pub(crate) use inner::AsyncStdRuntime;
