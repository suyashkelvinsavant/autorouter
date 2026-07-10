#![deny(unused_crate_dependencies)]
//! autorouter-router
//!
//! Smart routing engine. Phase 4 ships the rule engine, capability
//! registry, and health tracker; Phase 5 wires telemetry into them.

// Referenced only from integration tests.
#[cfg(test)]
use serde_json as _;

pub mod decision;
pub mod engine;
pub mod error;
pub mod health;
pub mod model;

pub use decision::{IdentityRouter, RouteDecision, Router};
pub use engine::{RuleEngine, SmartRouter};
pub use error::{RoutingError, RoutingResult};
pub use health::{HealthConfig, HealthSample, HealthSnapshot, HealthTracker, ModelHealthSnapshot};
pub use model::{
    CapabilityMatcher, CapabilityNeeds, CapabilityRegistry, ModelCapability, MultimodalNeeds,
    RouteTarget, RoutingContext, RoutingRule,
};
