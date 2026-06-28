//! Routing decision types.

use std::borrow::Cow;
use std::sync::Arc;

use autorouter_core::{ProviderKind, UniversalRequest};
use autorouter_translate::ProviderAdapter;

use crate::error::{RoutingError, RoutingResult};
use crate::model::RoutingContext;

/// A single routing decision. The gateway uses this to drive the
/// upstream call.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecision {
    pub target_provider: ProviderKind,
    /// Name of the custom provider when target_provider is Custom.
    pub custom_target: Option<String>,
    pub target_model: String,
    pub reason: Cow<'static, str>,
}

/// Trait for the routing engine. Phase 3 ships a single implementation
/// that picks the source provider; Phase 4 layers rules and health
/// on top.
pub trait Router: Send + Sync {
    fn decide(
        &self,
        ctx: &RoutingContext,
        request: &UniversalRequest,
    ) -> RoutingResult<RouteDecision>;
}

/// A router that always routes to the source provider. The MVP path.
pub struct IdentityRouter {
    adapters: Vec<Arc<dyn ProviderAdapter>>,
}

impl IdentityRouter {
    pub fn new(adapters: Vec<Arc<dyn ProviderAdapter>>) -> Self {
        Self { adapters }
    }
    pub fn adapters(&self) -> &[Arc<dyn ProviderAdapter>] {
        &self.adapters
    }
}

impl Router for IdentityRouter {
    fn decide(
        &self,
        ctx: &RoutingContext,
        _request: &UniversalRequest,
    ) -> RoutingResult<RouteDecision> {
        let adapter = self
            .adapters
            .iter()
            .find(|a| a.kind() == ctx.source_provider)
            .ok_or_else(|| {
                RoutingError::NoRoute(format!(
                    "no adapter for source provider {}",
                    ctx.source_provider
                ))
            })?;
        Ok(RouteDecision {
            target_provider: adapter.kind(),
            custom_target: None,
            target_model: String::new(),
            reason: Cow::Borrowed("identity"),
        })
    }
}
