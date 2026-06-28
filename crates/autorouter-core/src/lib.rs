#![deny(unused_crate_dependencies)]
//! autorouter-core
//!
//! Provider-neutral universal schema used internally by AutoRouter.

pub mod error;
pub mod ids;
pub mod message;
pub mod model;
pub mod request;
pub mod response;
pub mod streaming;
pub mod tool;
pub mod usage;

pub use error::{CoreError, CoreResult};
pub use ids::{RequestId, SessionId};
pub use message::{ContentPart, ImageSource, Message, MessageRole, ToolResultPayload};
pub use model::{ModelDescriptor, ModelFamily, ProviderKind};
pub use request::{RequestContext, UniversalRequest};
pub use response::{FinishReason, UniversalResponse};
pub use streaming::{StreamChunk, StreamEvent};
pub use tool::{ToolCall, ToolDefinition, ToolResult, ToolResultBody};
pub use usage::{TokenBreakdown, Usage};
