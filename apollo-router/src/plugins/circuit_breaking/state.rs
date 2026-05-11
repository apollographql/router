use std::fmt;
use std::time::Duration;
use std::time::Instant;

use dashmap::DashMap;

/// Composite key identifying a specific field on a specific subgraph.
/// Uses type-qualified coordinates (e.g. "Product.inventory") so that a failing
/// resolver accumulates errors across all queries that touch it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CircuitKey {
    pub(crate) subgraph_name: String,
    pub(crate) field_coordinate: String,
}

impl fmt::Display for CircuitKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.subgraph_name, self.field_coordinate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl fmt::Display for BreakerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BreakerState::Closed => write!(f, "closed"),
            BreakerState::Open => write!(f, "open"),
            BreakerState::HalfOpen => write!(f, "half_open"),
        }
    }
}

pub(crate) struct CircuitState {
    state: BreakerState,
    error_count: u32,
    window_start: Instant,
    opened_at: Option<Instant>,
    half_open_inflight: u32,
}

impl CircuitState {
    fn new() -> Self {
        Self {
            state: BreakerState::Closed,
            error_count: 0,
            window_start: Instant::now(),
            opened_at: None,
            half_open_inflight: 0,
        }
    }
}

/// Transition returned from state checks so callers can emit metrics outside the lock.
pub(crate) struct Transition {
    pub(crate) from: BreakerState,
    pub(crate) to: BreakerState,
}

/// Result of checking whether a request should proceed.
pub(crate) enum CheckResult {
    /// Request is allowed; includes any state transition that occurred.
    Allowed(Option<Transition>),
    /// Request is rejected because the circuit is open.
    Rejected,
}

/// Shared, process-global circuit breaker state keyed by (subgraph, field coordinate).
#[derive(Clone)]
pub(crate) struct CircuitBreakerRegistry {
    states: std::sync::Arc<DashMap<CircuitKey, CircuitState>>,
    error_threshold: u32,
    window: Duration,
    recovery_timeout: Duration,
    half_open_max_requests: u32,
}

impl CircuitBreakerRegistry {
    pub(crate) fn new(
        error_threshold: u32,
        window: Duration,
        recovery_timeout: Duration,
        half_open_max_requests: u32,
    ) -> Self {
        Self {
            states: std::sync::Arc::new(DashMap::new()),
            error_threshold,
            window,
            recovery_timeout,
            half_open_max_requests,
        }
    }

    /// Check whether a request for the given key should be allowed.
    pub(crate) fn check(&self, key: &CircuitKey) -> CheckResult {
        let now = Instant::now();
        let mut entry = self
            .states
            .entry(key.clone())
            .or_insert_with(CircuitState::new);
        let state = entry.value_mut();

        match state.state {
            BreakerState::Closed => CheckResult::Allowed(None),
            BreakerState::Open => {
                let opened_at = state.opened_at.expect("opened_at set when entering Open");
                if now.duration_since(opened_at) >= self.recovery_timeout {
                    let from = state.state;
                    state.state = BreakerState::HalfOpen;
                    state.half_open_inflight = 1;
                    CheckResult::Allowed(Some(Transition {
                        from,
                        to: BreakerState::HalfOpen,
                    }))
                } else {
                    CheckResult::Rejected
                }
            }
            BreakerState::HalfOpen => {
                if state.half_open_inflight < self.half_open_max_requests {
                    state.half_open_inflight += 1;
                    CheckResult::Allowed(None)
                } else {
                    CheckResult::Rejected
                }
            }
        }
    }

    /// Record a successful response for the given key.
    pub(crate) fn record_success(&self, key: &CircuitKey) -> Option<Transition> {
        let mut entry = self
            .states
            .entry(key.clone())
            .or_insert_with(CircuitState::new);
        let state = entry.value_mut();

        match state.state {
            BreakerState::HalfOpen => {
                let from = state.state;
                state.state = BreakerState::Closed;
                state.error_count = 0;
                state.window_start = Instant::now();
                state.opened_at = None;
                state.half_open_inflight = 0;
                Some(Transition {
                    from,
                    to: BreakerState::Closed,
                })
            }
            BreakerState::Closed => {
                if Instant::now().duration_since(state.window_start) >= self.window {
                    state.error_count = 0;
                    state.window_start = Instant::now();
                }
                None
            }
            BreakerState::Open => None,
        }
    }

    /// Record an error response for the given key.
    /// Returns a transition if the circuit state changed.
    pub(crate) fn record_error(&self, key: &CircuitKey) -> Option<Transition> {
        let now = Instant::now();
        let mut entry = self
            .states
            .entry(key.clone())
            .or_insert_with(CircuitState::new);
        let state = entry.value_mut();

        match state.state {
            BreakerState::Closed => {
                if now.duration_since(state.window_start) >= self.window {
                    state.error_count = 1;
                    state.window_start = now;
                } else {
                    state.error_count += 1;
                }

                if state.error_count >= self.error_threshold {
                    let from = state.state;
                    state.state = BreakerState::Open;
                    state.opened_at = Some(now);
                    state.half_open_inflight = 0;
                    Some(Transition {
                        from,
                        to: BreakerState::Open,
                    })
                } else {
                    None
                }
            }
            BreakerState::HalfOpen => {
                let from = state.state;
                state.state = BreakerState::Open;
                state.opened_at = Some(now);
                state.half_open_inflight = 0;
                Some(Transition {
                    from,
                    to: BreakerState::Open,
                })
            }
            BreakerState::Open => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn get_state(&self, key: &CircuitKey) -> BreakerState {
        self.states
            .get(key)
            .map(|s| s.state)
            .unwrap_or(BreakerState::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(subgraph: &str, coordinate: &str) -> CircuitKey {
        CircuitKey {
            subgraph_name: subgraph.to_string(),
            field_coordinate: coordinate.to_string(),
        }
    }

    #[test]
    fn starts_closed() {
        let registry =
            CircuitBreakerRegistry::new(3, Duration::from_secs(60), Duration::from_secs(30), 1);
        assert_eq!(
            registry.get_state(&test_key("products", "Product.inventory")),
            BreakerState::Closed
        );
    }

    #[test]
    fn opens_after_threshold() {
        let registry =
            CircuitBreakerRegistry::new(3, Duration::from_secs(60), Duration::from_secs(30), 1);
        let key = test_key("products", "Product.inventory");

        assert!(registry.record_error(&key).is_none());
        assert!(registry.record_error(&key).is_none());
        let transition = registry.record_error(&key).expect("should transition");
        assert_eq!(transition.from, BreakerState::Closed);
        assert_eq!(transition.to, BreakerState::Open);
        assert_eq!(registry.get_state(&key), BreakerState::Open);
    }

    #[test]
    fn rejects_when_open() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_secs(300), 1);
        let key = test_key("products", "Product.inventory");
        registry.record_error(&key);

        assert!(matches!(registry.check(&key), CheckResult::Rejected));
    }

    #[test]
    fn transitions_to_half_open_after_recovery() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_millis(0), 1);
        let key = test_key("products", "Product.inventory");
        registry.record_error(&key);
        assert_eq!(registry.get_state(&key), BreakerState::Open);

        let result = registry.check(&key);
        assert!(
            matches!(result, CheckResult::Allowed(Some(ref t)) if t.to == BreakerState::HalfOpen)
        );
        assert_eq!(registry.get_state(&key), BreakerState::HalfOpen);
    }

    #[test]
    fn half_open_success_closes() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_millis(0), 1);
        let key = test_key("products", "Product.inventory");
        registry.record_error(&key);

        registry.check(&key);
        assert_eq!(registry.get_state(&key), BreakerState::HalfOpen);

        let transition = registry.record_success(&key).expect("should transition");
        assert_eq!(transition.from, BreakerState::HalfOpen);
        assert_eq!(transition.to, BreakerState::Closed);
        assert_eq!(registry.get_state(&key), BreakerState::Closed);
    }

    #[test]
    fn half_open_error_reopens() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_millis(0), 1);
        let key = test_key("products", "Product.inventory");
        registry.record_error(&key);

        registry.check(&key);
        assert_eq!(registry.get_state(&key), BreakerState::HalfOpen);

        let transition = registry.record_error(&key).expect("should transition");
        assert_eq!(transition.from, BreakerState::HalfOpen);
        assert_eq!(transition.to, BreakerState::Open);
    }

    #[test]
    fn half_open_limits_inflight() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_millis(0), 1);
        let key = test_key("products", "Product.inventory");
        registry.record_error(&key);

        let result = registry.check(&key);
        assert!(matches!(result, CheckResult::Allowed(_)));

        assert!(matches!(registry.check(&key), CheckResult::Rejected));
    }

    #[test]
    fn window_reset_on_expiry() {
        let registry =
            CircuitBreakerRegistry::new(3, Duration::from_millis(0), Duration::from_secs(30), 1);
        let key = test_key("products", "Product.inventory");

        registry.record_error(&key);
        registry.record_error(&key);

        let transition = registry.record_error(&key);
        assert!(transition.is_none());
        assert_eq!(registry.get_state(&key), BreakerState::Closed);
    }

    #[test]
    fn independent_subgraphs() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_secs(30), 1);
        let key_a = test_key("products", "Product.inventory");
        let key_b = test_key("reviews", "Review.text");

        registry.record_error(&key_a);
        assert_eq!(registry.get_state(&key_a), BreakerState::Open);
        assert_eq!(registry.get_state(&key_b), BreakerState::Closed);

        assert!(matches!(registry.check(&key_b), CheckResult::Allowed(None)));
    }

    #[test]
    fn different_fields_same_subgraph_are_independent() {
        let registry =
            CircuitBreakerRegistry::new(1, Duration::from_secs(60), Duration::from_secs(30), 1);
        let key_inventory = test_key("products", "Product.inventory");
        let key_name = test_key("products", "Product.name");

        registry.record_error(&key_inventory);
        assert_eq!(registry.get_state(&key_inventory), BreakerState::Open);
        assert_eq!(registry.get_state(&key_name), BreakerState::Closed);

        assert!(matches!(
            registry.check(&key_name),
            CheckResult::Allowed(None)
        ));
    }

    #[test]
    fn same_field_coordinate_across_operations_shares_circuit() {
        let registry =
            CircuitBreakerRegistry::new(2, Duration::from_secs(60), Duration::from_secs(30), 1);
        let key = test_key("products", "Product.inventory");

        registry.record_error(&key);
        assert_eq!(registry.get_state(&key), BreakerState::Closed);

        registry.record_error(&key);
        assert_eq!(registry.get_state(&key), BreakerState::Open);
    }
}
