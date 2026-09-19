//! Protocol-neutral frontend services that can execute ordinary model tools.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use tokio_util::sync::CancellationToken;

/// A foreground terminal command requested by the model tool runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendTerminalRequest {
    pub tool_call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub output_byte_limit: u64,
}

/// Terminal output returned by a frontend-owned process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendTerminalResult {
    pub output: String,
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
    pub truncated: bool,
}

/// A text-file write requested after the model tool runtime authorizes a mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendWriteTextFileRequest {
    pub path: PathBuf,
    pub content: String,
}

/// Optional filesystem implementation installed by a protocol frontend.
///
/// The runtime remains responsible for planning, stale-file checks, workspace
/// boundaries, and permission authorization before calling this service.
pub trait FrontendFileSystem: Send + Sync {
    fn write_text_file(
        &self,
        request: FrontendWriteTextFileRequest,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

/// Optional terminal implementation installed by a protocol frontend.
///
/// The runtime remains responsible for command authorization. Implementations
/// own process cancellation and resource cleanup.
pub trait FrontendTerminal: Send + Sync {
    fn execute(
        &self,
        request: FrontendTerminalRequest,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<FrontendTerminalResult, String>> + Send + '_>>;
}
