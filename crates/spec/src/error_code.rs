use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ParseError,
    UnknownCommand,
    UnknownArgument,
    MissingArgument,
    TypeError,
    InvalidValue,
    NotFound,
    AlreadyExists,
    OutsideWorkspace,
    ProtectedPath,
    CapabilityDenied,
    ExpectationFailed,
    Stale,
    AmbiguousMatch,
    NoMatch,
    BinaryFile,
    ExecFailed,
    Timeout,
    Conflict,
    NothingToUndo,
    SessionBusy,
    IoError,
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }
}
