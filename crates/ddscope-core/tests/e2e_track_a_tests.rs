//! DDScopeHost Phase 1 Track A (TA2 - TA5) End-to-End Integration Test Suite.
//!
//! Validates all 24 features of `PROJECT.md § Feature Inventory` across Tiers 1-4
//! through public interfaces:
//! - storage::m4 (M4 envelope pyramid, decimation, extrema retention)
//! - storage::quota (Memory quota manager, chunk pinning, FIFO eviction)
//! - scope::controller & scope::sync (ScopeController, trigger engine, sync group)
//! - scope::ddsf (DDSF v1 zero-copy binary wire serializer)
//!
//! Architecture:
//! - Tier 1: Feature Coverage (120 tests, 5 per feature across F01-F24)
//! - Tier 2: Boundary & Corner Cases (120 tests, 5 per feature across F01-F24)
//! - Tier 3: Cross-Feature Combinations (24 pairwise tests)
//! - Tier 4: Real-World Multi-Scope Application Scenarios (12 scenarios)
//!
//! Total test cases: 276 (plus 1 suite configuration sanity test = 277 tests).

#![allow(
    clippy::approx_constant,
    clippy::needless_range_loop,
    clippy::unnecessary_cast,
    clippy::useless_vec,
    clippy::doc_lazy_continuation,
    clippy::float_cmp
)]

#[macro_use]
#[path = "e2e_track_a_tests/common.rs"]
pub mod common;
#[path = "e2e_track_a_tests/tier1_feature_tests.rs"]
pub mod tier1_feature_tests;
#[path = "e2e_track_a_tests/tier2_boundary_tests.rs"]
pub mod tier2_boundary_tests;
#[path = "e2e_track_a_tests/tier3_pairwise_tests.rs"]
pub mod tier3_pairwise_tests;
#[path = "e2e_track_a_tests/tier4_scenario_tests.rs"]
pub mod tier4_scenario_tests;

#[test]
fn verify_e2e_suite_configuration() {
    let build = ddscope_core::build_info();
    assert_eq!(build.name, "ddscope-core");
    assert!(!build.version.is_empty());
}
