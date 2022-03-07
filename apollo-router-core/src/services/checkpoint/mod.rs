pub mod async_checkpoint;
pub mod sync_checkpoint;

pub use async_checkpoint::{AsyncCheckpointLayer, AsyncCheckpointService};
pub use sync_checkpoint::{CheckpointLayer, CheckpointService};

/// An enum representing the next step
/// A service should follow
pub enum Step<Request, Response>
where
    Request: Send + 'static,
    Response: Send + 'static,
{
    /// `CheckpointService` should
    /// Continue and call the next service
    /// `CheckpointService` should not call the next service.
    Continue(Request),
    /// Return the provided Response instead
    Return(Response),
}
