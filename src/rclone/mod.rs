//! Everything that talks to rclone.

pub mod api;
mod backend;
#[cfg(feature = "embedded")]
mod ffi;
pub(crate) mod obscure;
pub mod ssh_passwords;
pub mod system_ssh;

#[cfg(feature = "embedded")]
pub use backend::Embedded;
#[cfg(feature = "rcd")]
pub use backend::Http;
pub use backend::{Backend, FakeBackend, RpcError};
