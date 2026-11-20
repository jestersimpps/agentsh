mod envelope;
mod error_code;
mod value;

pub use envelope::{Effect, EffectOp, Envelope, ErrorInfo, Output, Span};
pub use error_code::ErrorCode;
pub use value::{Record, Value};

pub const LANGUAGE_VERSION: &str = "0.1";
