//! Multi-scope synchronization group coordinator providing zero-parallax timebase alignment,
//! synchronized horizontal zoom/pan, and shared measurement cursors.
//!
//! Conforms to Phase 1 Track A Milestone TA4 specification in `docs/04-phase1-design-and-plan.md` §3.5.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::scope::controller::ScopeControllerConfig;
use crate::types::{ScopeId, SyncGroupId};

/// Helper function to sanitize and clamp a cursor position ratio to $[0.0, 1.0]$.
#[inline]
fn clamp_cursor_ratio(ratio: Option<f64>) -> Option<f64> {
    ratio.and_then(|r| {
        if r.is_finite() {
            Some(r.clamp(0.0, 1.0))
        } else {
            None
        }
    })
}

/// Synchronized timebase and measurement cursor state for a group of oscilloscopes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncGroupState {
    /// Unique synchronization group identifier.
    pub group_id: SyncGroupId,
    /// Shared horizontal time span across all member scopes in nanoseconds.
    pub time_span_ns: u64,
    /// Shared center pan offset relative to trigger zero point in nanoseconds.
    pub center_offset_ns: i64,
    /// Synchronized measurement cursor A position ratio in $[0.0, 1.0]$ across viewports.
    pub cursor_a_ratio: Option<f64>,
    /// Synchronized measurement cursor B position ratio in $[0.0, 1.0]$ across viewports.
    pub cursor_b_ratio: Option<f64>,
    /// Monotonically increasing state version counter for change detection.
    pub version: u64,
}

impl SyncGroupState {
    /// Constructs a new [`SyncGroupState`] with initial parameters.
    pub fn new(group_id: SyncGroupId, time_span_ns: u64, center_offset_ns: i64) -> Self {
        Self {
            group_id,
            time_span_ns: time_span_ns.max(1000),
            center_offset_ns,
            cursor_a_ratio: None,
            cursor_b_ratio: None,
            version: 1,
        }
    }
}

/// Thread-safe multi-scope coordination manager backed by lock-free concurrent hash maps.
#[derive(Debug, Default)]
pub struct SyncGroupCoordinator {
    groups: DashMap<SyncGroupId, SyncGroupState>,
    scope_memberships: DashMap<ScopeId, SyncGroupId>,
}

impl SyncGroupCoordinator {
    /// Constructs a new empty [`SyncGroupCoordinator`].
    pub fn new() -> Self {
        Self {
            groups: DashMap::new(),
            scope_memberships: DashMap::new(),
        }
    }

    /// Registers a scope into a synchronization group.
    ///
    /// If the group does not yet exist, initializes it with a default 10 ms time span.
    pub fn register_scope(&self, scope_id: ScopeId, group_id: SyncGroupId) {
        self.groups
            .entry(group_id)
            .or_insert_with(|| SyncGroupState::new(group_id, 10_000_000, 0));
        self.scope_memberships.insert(scope_id, group_id);
    }

    /// Unregisters a scope from its current synchronization group.
    pub fn unregister_scope(&self, scope_id: ScopeId) {
        self.scope_memberships.remove(&scope_id);
    }

    /// Returns the synchronization group ID to which `scope_id` belongs, if registered.
    pub fn get_scope_group(&self, scope_id: ScopeId) -> Option<SyncGroupId> {
        self.scope_memberships.get(&scope_id).map(|r| *r.value())
    }

    /// Returns all scope IDs currently belonging to `group_id`.
    pub fn get_group_scopes(&self, group_id: SyncGroupId) -> Vec<ScopeId> {
        self.scope_memberships
            .iter()
            .filter(|entry| *entry.value() == group_id)
            .map(|entry| *entry.key())
            .collect()
    }

    /// Retrieves a cloned snapshot of the current state of `group_id`.
    pub fn get_group_state(&self, group_id: SyncGroupId) -> Option<SyncGroupState> {
        self.groups.get(&group_id).map(|r| r.value().clone())
    }

    /// Atomically updates the timebase parameters (`time_span_ns` and `center_offset_ns`)
    /// for all scopes in `group_id`.
    ///
    /// Increments and returns the updated `version`.
    pub fn update_timebase(
        &self,
        group_id: SyncGroupId,
        time_span_ns: u64,
        center_offset_ns: i64,
    ) -> u64 {
        let valid_span = time_span_ns.max(1000);
        let mut entry = self
            .groups
            .entry(group_id)
            .or_insert_with(|| SyncGroupState::new(group_id, valid_span, center_offset_ns));

        entry.time_span_ns = valid_span;
        entry.center_offset_ns = center_offset_ns;
        entry.version = entry.version.wrapping_add(1);
        entry.version
    }

    /// Atomically updates shared measurement cursors `cursor_a` and `cursor_b` for `group_id`.
    ///
    /// Cursor ratios are automatically clamped to $[0.0, 1.0]$.
    /// Increments and returns the updated `version`.
    pub fn update_cursors(
        &self,
        group_id: SyncGroupId,
        cursor_a_ratio: Option<f64>,
        cursor_b_ratio: Option<f64>,
    ) -> u64 {
        let a = clamp_cursor_ratio(cursor_a_ratio);
        let b = clamp_cursor_ratio(cursor_b_ratio);

        let mut entry = self
            .groups
            .entry(group_id)
            .or_insert_with(|| SyncGroupState::new(group_id, 10_000_000, 0));

        entry.cursor_a_ratio = a;
        entry.cursor_b_ratio = b;
        entry.version = entry.version.wrapping_add(1);
        entry.version
    }

    /// Synchronizes a [`ScopeControllerConfig`] with its group's current timebase.
    ///
    /// Returns `true` if the scope was registered to a group and updated, or `false` otherwise.
    pub fn apply_to_controller_config(
        &self,
        scope_id: ScopeId,
        config: &mut ScopeControllerConfig,
    ) -> bool {
        if let Some(group_id) = self.get_scope_group(scope_id) {
            if let Some(state) = self.get_group_state(group_id) {
                config.time_span_ns = state.time_span_ns;
                config.pan_offset_ns = state.center_offset_ns;
                config.sync_group_id = Some(group_id);
                return true;
            }
        }
        false
    }

    /// Returns the number of active synchronization groups.
    #[inline]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Returns the total number of scopes registered across all groups.
    #[inline]
    pub fn scope_count(&self) -> usize {
        self.scope_memberships.len()
    }

    /// Clears all registered groups and scope memberships.
    pub fn clear(&self) {
        self.groups.clear();
        self.scope_memberships.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_sync_group_registration_and_unregistration() {
        let coord = SyncGroupCoordinator::new();
        let s1 = ScopeId(1);
        let s2 = ScopeId(2);
        let g1 = SyncGroupId(10);

        coord.register_scope(s1, g1);
        coord.register_scope(s2, g1);

        assert_eq!(coord.scope_count(), 2);
        assert_eq!(coord.group_count(), 1);
        assert_eq!(coord.get_scope_group(s1), Some(g1));
        assert_eq!(coord.get_scope_group(s2), Some(g1));

        let mut scopes = coord.get_group_scopes(g1);
        scopes.sort_by_key(|s| s.0);
        assert_eq!(scopes, vec![s1, s2]);

        coord.unregister_scope(s1);
        assert_eq!(coord.get_scope_group(s1), None);
        assert_eq!(coord.get_group_scopes(g1), vec![s2]);
        assert_eq!(coord.scope_count(), 1);
    }

    #[test]
    fn test_sync_group_atomic_timebase_update() {
        let coord = SyncGroupCoordinator::new();
        let g1 = SyncGroupId(5);

        let v1 = coord.update_timebase(g1, 50_000_000, -5_000_000);
        assert!(v1 >= 1);

        let state = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state.time_span_ns, 50_000_000);
        assert_eq!(state.center_offset_ns, -5_000_000);
        assert_eq!(state.version, v1);

        // Update again: version increments
        let v2 = coord.update_timebase(g1, 20_000_000, 1_000_000);
        assert_eq!(v2, v1 + 1);

        let state2 = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state2.time_span_ns, 20_000_000);
        assert_eq!(state2.center_offset_ns, 1_000_000);
        assert_eq!(state2.version, v2);

        // Minimum time span clamp (1000 ns)
        coord.update_timebase(g1, 10, 0);
        let state3 = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state3.time_span_ns, 1000);
    }

    #[test]
    fn test_sync_group_atomic_cursor_update() {
        let coord = SyncGroupCoordinator::new();
        let g1 = SyncGroupId(8);

        let v1 = coord.update_cursors(g1, Some(0.25), Some(0.75));
        let state = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state.cursor_a_ratio, Some(0.25));
        assert_eq!(state.cursor_b_ratio, Some(0.75));
        assert_eq!(state.version, v1);

        // Out-of-bounds cursor values are clamped to [0.0, 1.0]
        coord.update_cursors(g1, Some(-0.5), Some(1.8));
        let state2 = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state2.cursor_a_ratio, Some(0.0));
        assert_eq!(state2.cursor_b_ratio, Some(1.0));

        // NaN / Inf are converted to None
        coord.update_cursors(g1, Some(f64::NAN), Some(f64::INFINITY));
        let state3 = coord.get_group_state(g1).expect("group state exists");
        assert_eq!(state3.cursor_a_ratio, None);
        assert_eq!(state3.cursor_b_ratio, None);
    }

    #[test]
    fn test_sync_group_apply_to_controller_config() {
        let coord = SyncGroupCoordinator::new();
        let s1 = ScopeId(42);
        let g1 = SyncGroupId(7);

        coord.register_scope(s1, g1);
        coord.update_timebase(g1, 33_000_000, 1_234_567);

        let mut config = ScopeControllerConfig {
            scope_id: s1,
            time_span_ns: 10_000,
            pan_offset_ns: 0,
            sync_group_id: None,
            ..Default::default()
        };

        let applied = coord.apply_to_controller_config(s1, &mut config);
        assert!(applied);
        assert_eq!(config.time_span_ns, 33_000_000);
        assert_eq!(config.pan_offset_ns, 1_234_567);
        assert_eq!(config.sync_group_id, Some(g1));

        // Unregistered scope returns false and modifies nothing
        let mut unreg_config = ScopeControllerConfig {
            scope_id: ScopeId(999),
            time_span_ns: 5_000,
            ..Default::default()
        };
        assert!(!coord.apply_to_controller_config(ScopeId(999), &mut unreg_config));
        assert_eq!(unreg_config.time_span_ns, 5_000);
    }

    #[test]
    fn test_multi_thread_coordination() {
        let coord = Arc::new(SyncGroupCoordinator::new());
        let g1 = SyncGroupId(1);
        let num_threads = 8;
        let iters = 200;

        let mut handles = Vec::new();
        for t in 0..num_threads {
            let coord_clone = Arc::clone(&coord);
            let handle = thread::spawn(move || {
                let scope_id = ScopeId(t as u32);
                coord_clone.register_scope(scope_id, g1);

                for i in 0..iters {
                    let span = 10_000 + (t * 100 + i) as u64;
                    coord_clone.update_timebase(g1, span, (t * 10) as i64);
                    let _ = coord_clone.get_group_state(g1);
                }
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().expect("thread finished cleanly");
        }

        assert_eq!(coord.scope_count(), num_threads);
        let final_state = coord.get_group_state(g1).expect("final state exists");
        assert!(final_state.version >= (num_threads * iters) as u64);
    }
}
