//! Poll-scoped attribution of connection-lifecycle metrics to a bounded workload.

use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::pin::pin;

use opentelemetry::KeyValue;

thread_local! {
    /// The attribution of the future this thread is polling right now.
    ///
    /// It is set only for the duration of one synchronous `poll`, so another task the runtime
    /// schedules on this thread between polls can never observe it. The value has no destructor,
    /// so reading it stays valid during thread teardown.
    static CURRENT: Cell<Option<PoolAttribution>> = const { Cell::new(None) };
}

/// A single bounded attribute identifying the workload that uses a connection pool.
///
/// While a future runs inside [`scope`](Self::scope), every connection it acquires from any
/// [`Pool`](crate::Pool) carries this attribute on `db.client.connection.wait_time`,
/// `db.client.connection.use_time`, `db.client.connection.timeouts`,
/// `sqlx_otel.connection.abandoned_wait.duration`, `sqlx_otel.transaction.begin.duration` and
/// `sqlx_otel.transaction.duration`. It splits existing series rather than adding samples, so
/// summing across it reproduces the pool-wide totals.
///
/// Pool-wide state stays pool-wide: `db.client.connection.pending_requests` and the
/// `db.client.connection.count` / `max` / `idle.*` gauges never carry it. Neither do
/// `db.client.operation.duration` and the row-count histograms, whose `db.query.summary` dimension
/// would multiply with it. A workload's timeout series is initialized at zero at its first
/// acquisition. On backends that compute rates from cumulative samples, the first timeout is
/// visible only if that zero was exported first.
///
/// # Cardinality
///
/// Both strings are `&'static str`, so a value derived at runtime from an identifier, a query or
/// user input cannot reach a metric label without deliberately leaking memory. One attribution is
/// exactly one key and one value. Choose a key in your own namespace. Attribution using any of
/// these reserved keys is ignored, even when the corresponding optional pool attribute is absent:
///
/// - `db.system.name`, `db.namespace`, `server.address`, `server.port`
/// - `network.peer.address`, `network.peer.port`, `network.protocol.name`, `network.transport`
/// - `db.client.connection.pool.name`
/// - `sqlx_otel.transaction.begin.outcome`, `sqlx_otel.transaction.outcome`
///
/// The added series are bounded by `pools × (values + 1)`, multiplied by the outcome count
/// on the two transaction instruments; the extra value is work without emitted attribution.
///
/// # What is and is not covered
///
/// Attribution is captured once, when an acquisition first runs, and stored with the attempt and
/// the resulting lease. Everything recorded later uses that captured value, wherever and whenever
/// the connection or transaction is finally dropped, so it cannot drift to another workload.
///
/// The scope itself follows polling, not "the task" or the call stack:
///
/// - Futures polled inside the scope inherit it, including `join!`, `select!` and
///   `FuturesUnordered` children. Concurrent scopes on one task stay separate.
/// - **`tokio::spawn`, `spawn_blocking` and other threads do not inherit it.** Their acquisitions
///   record without this attribute unless you pass it on explicitly with
///   [`inherit`](Self::inherit).
/// - A stream or query future acquires when first polled, so the scope active at that first poll
///   wins, not the one where it was constructed.
/// - Anything polled re-entrantly inside the scope on the same thread, such as tasks driven by a
///   nested `LocalSet` or `block_on`, is inside the scope too.
///
/// Absence of the attribute therefore means "acquired without valid attribution", not
/// "pool-internal".
///
/// # Example
///
/// ```no_run
/// # #[cfg(feature = "sqlite")]
/// # async fn _doc() -> Result<(), sqlx::Error> {
/// use sqlx::Executor as _;
/// use sqlx_otel::{PoolAttribution, PoolBuilder};
/// # let pool = PoolBuilder::from(sqlx::SqlitePool::connect(":memory:").await?).build();
///
/// PoolAttribution::new("myapp.job.kind", "nightly_rollup")
///     .scope(async {
///         pool.execute("SELECT 1").await?;
///
///         // A spawned task starts outside the scope unless it is passed on explicitly.
///         let spawned_pool = pool.clone();
///         tokio::spawn(PoolAttribution::inherit(async move {
///             spawned_pool.execute("SELECT 2").await
///         }));
///         Ok::<_, sqlx::Error>(())
///     })
///     .await
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolAttribution {
    key: &'static str,
    value: &'static str,
}

impl PoolAttribution {
    /// Create an attribution from one compile-time key and one compile-time value.
    ///
    /// Reserved connection and transaction-outcome keys are ignored when recording metrics; see
    /// [cardinality](Self#cardinality) for the exact list.
    #[must_use]
    pub const fn new(key: &'static str, value: &'static str) -> Self {
        Self { key, value }
    }

    /// The attribution of the future being polled on this thread, if any.
    #[must_use]
    pub fn current() -> Option<Self> {
        CURRENT.with(Cell::get)
    }

    /// Run `future` with this attribution active during each of its polls.
    ///
    /// A nested scope replaces this one only while the inner future is being polled. Dropping the
    /// returned future, even mid-way, leaves no state behind.
    pub fn scope<F: Future>(self, future: F) -> impl Future<Output = F::Output> {
        poll_scoped(Some(self), future)
    }

    /// Capture the attribution active *now* and run `future` under it, for handing work to a
    /// spawned task.
    ///
    /// The capture happens when this function is called, not when the returned future is first
    /// polled, so call it in the scope you want to propagate. When no scope is active, the future
    /// also runs without one, even if it is later polled inside another scope.
    pub fn inherit<F: Future>(future: F) -> impl Future<Output = F::Output> {
        poll_scoped(Self::current(), future)
    }

    fn key_value(self) -> KeyValue {
        KeyValue::new(self.key, self.value)
    }
}

async fn poll_scoped<F: Future>(attribution: Option<PoolAttribution>, future: F) -> F::Output {
    let mut future = pin!(future);
    poll_fn(|cx| {
        let _active = ActiveAttribution::enter(attribution);
        future.as_mut().poll(cx)
    })
    .await
}

/// Restores the enclosing attribution on every exit from a poll, including a panic.
struct ActiveAttribution {
    enclosing: Option<PoolAttribution>,
}

impl ActiveAttribution {
    fn enter(attribution: Option<PoolAttribution>) -> Self {
        Self {
            enclosing: CURRENT.with(|current| current.replace(attribution)),
        }
    }
}

impl Drop for ActiveAttribution {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.enclosing));
    }
}

// Reserve optional connection keys even when their pool values are absent, as well as the
// outcome keys appended later by transaction recording. Telemetry must not overwrite identity or
// outcomes, and invalid attribution must not panic in the acquisition path.
const RESERVED_KEYS: &[&str] = &[
    "db.system.name",
    "db.namespace",
    "server.address",
    "server.port",
    "network.peer.address",
    "network.peer.port",
    "network.protocol.name",
    "network.transport",
    "db.client.connection.pool.name",
    "sqlx_otel.transaction.begin.outcome",
    "sqlx_otel.transaction.outcome",
];

/// The pool-wide attribute set plus any valid attribution active right now.
///
/// Called once per acquisition. The result is owned by the attempt and lease, so nothing recorded
/// later reads the thread-local again.
pub(crate) fn lease_attributes(pool_attrs: &[KeyValue]) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(pool_attrs.len() + 2);
    attrs.extend_from_slice(pool_attrs);
    if let Some(attribution) = PoolAttribution::current()
        && !RESERVED_KEYS.contains(&attribution.key)
    {
        attrs.push(attribution.key_value());
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::{PoolAttribution, lease_attributes};
    use futures::executor::block_on;
    use futures::future::{FutureExt, lazy, poll_fn};
    use opentelemetry::KeyValue;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::pin::pin;
    use std::task::Poll;

    const A: PoolAttribution = PoolAttribution::new("test.kind", "a");
    const B: PoolAttribution = PoolAttribution::new("test.kind", "b");

    /// Returns `Pending` once and wakes itself, so a sibling future gets polled in between.
    async fn yield_once() {
        let mut yielded = false;
        poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    async fn unreachable_after_yield() {
        yield_once().await;
        unreachable!("dropped while suspended");
    }

    fn assert_send<T: Send>(value: T) -> T {
        value
    }

    #[test]
    fn no_attribution_outside_a_scope() {
        assert_eq!(PoolAttribution::current(), None);
        assert_eq!(lease_attributes(&[]), Vec::<KeyValue>::new());
    }

    /// The scope is visible on every poll and gone between polls: this is what stops another task
    /// sharing the worker thread from seeing it.
    #[test]
    fn scope_is_active_only_while_polling() {
        block_on(async {
            let mut scoped = pin!(A.scope(async {
                assert_eq!(PoolAttribution::current(), Some(A));
                yield_once().await;
                assert_eq!(PoolAttribution::current(), Some(A));
            }));
            assert_eq!(PoolAttribution::current(), None);
            assert!(futures::poll!(&mut scoped).is_pending());
            assert_eq!(
                PoolAttribution::current(),
                None,
                "must not outlive the poll"
            );
            assert!(futures::poll!(&mut scoped).is_ready());
            assert_eq!(PoolAttribution::current(), None);
        });
    }

    #[test]
    fn nested_scope_replaces_and_restores() {
        block_on(A.scope(async {
            B.scope(async {
                assert_eq!(PoolAttribution::current(), Some(B));
                yield_once().await;
                assert_eq!(PoolAttribution::current(), Some(B));
            })
            .await;
            assert_eq!(PoolAttribution::current(), Some(A));
        }));
    }

    /// Two scopes interleaved on one task, each resuming after the other ran.
    #[test]
    fn concurrent_scopes_on_one_task_never_see_each_other_across_awaits() {
        async fn observe(expected: Option<PoolAttribution>, rounds: usize) -> usize {
            for _ in 0..rounds {
                assert_eq!(PoolAttribution::current(), expected);
                yield_once().await;
            }
            assert_eq!(PoolAttribution::current(), expected);
            rounds
        }
        block_on(async {
            let (a, b, unscoped) = futures::join!(
                A.scope(observe(Some(A), 25)),
                B.scope(observe(Some(B), 25)),
                // The enclosing unscoped body must also stay unscoped between its siblings.
                observe(None, 25),
            );
            assert_eq!((a, b, unscoped), (25, 25, 25));
        });
    }

    #[test]
    fn dropping_a_suspended_scope_leaves_nothing_behind() {
        block_on(async {
            let mut scoped = Box::pin(A.scope(unreachable_after_yield()));
            assert!(futures::poll!(&mut scoped).is_pending());
            drop(scoped);
            assert_eq!(PoolAttribution::current(), None);
        });
    }

    #[test]
    fn a_never_polled_scope_has_no_effect() {
        drop(A.scope(lazy(|_| -> () { unreachable!("never polled") })));
        assert_eq!(PoolAttribution::current(), None);
    }

    #[test]
    fn a_panicking_poll_restores_the_enclosing_attribution() {
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            A.scope(lazy(|_| -> () { panic!("inside scope") }))
                .now_or_never();
        }));
        assert!(panicked.is_err());
        assert_eq!(PoolAttribution::current(), None);
    }

    #[test]
    fn inherit_captures_at_the_call_not_at_the_first_poll() {
        let inherited = block_on(A.scope(lazy(|_| {
            PoolAttribution::inherit(async { PoolAttribution::current() })
        })));
        assert_eq!(PoolAttribution::current(), None);
        assert_eq!(block_on(inherited), Some(A), "polled outside any scope");

        let inherited = block_on(A.scope(lazy(|_| {
            PoolAttribution::inherit(async { PoolAttribution::current() })
        })));
        assert_eq!(block_on(B.scope(inherited)), Some(A), "polled inside B");
    }

    #[test]
    fn inheriting_no_attribution_masks_a_later_scope() {
        let unscoped = PoolAttribution::inherit(async { PoolAttribution::current() });
        assert_eq!(block_on(A.scope(unscoped)), None);
    }

    #[test]
    fn scoped_futures_stay_send_for_spawning() {
        drop(assert_send(A.scope(yield_once())));
        drop(assert_send(PoolAttribution::inherit(yield_once())));
    }

    #[test]
    fn lease_attributes_append_exactly_the_active_attribution() {
        let base = [KeyValue::new("db.system.name", "sqlite")];
        block_on(A.scope(async {
            assert_eq!(
                lease_attributes(&base),
                vec![base[0].clone(), KeyValue::new("test.kind", "a")]
            );
        }));
        assert_eq!(lease_attributes(&base), base);
    }

    #[test]
    fn reserved_attribution_never_changes_pool_attributes_even_when_optional_keys_are_absent() {
        for key in [
            "db.system.name",
            "db.namespace",
            "server.address",
            "server.port",
            "network.peer.address",
            "network.peer.port",
            "network.protocol.name",
            "network.transport",
            "db.client.connection.pool.name",
            "sqlx_otel.transaction.begin.outcome",
            "sqlx_otel.transaction.outcome",
        ] {
            let attribution = PoolAttribution::new(key, "spoofed");
            let present = [KeyValue::new(key, "canonical")];
            block_on(attribution.scope(async {
                assert_eq!(lease_attributes(&present), present, "{key} already present");
                assert!(lease_attributes(&[]).is_empty(), "{key} absent");
            }));
        }
    }
}
