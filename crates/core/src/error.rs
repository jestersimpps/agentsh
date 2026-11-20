use agentsh_lang::ParseError;
use agentsh_spec::{ErrorCode, ErrorInfo, Span, Value};
use std::io;
use std::ops::{Deref, DerefMut};

#[derive(Debug, Clone)]
pub struct ShellError(Box<ErrorData>);

#[derive(Debug, Clone)]
pub struct ErrorData {
    pub code: ErrorCode,
    pub message: String,
    pub arg: Option<String>,
    pub value: Option<String>,
    pub hint: Option<String>,
    pub command: Option<String>,
    pub span: Option<Span>,
}

pub type Result<T> = std::result::Result<T, ShellError>;

impl ShellError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(Box::new(ErrorData {
            code,
            message: message.into(),
            arg: None,
            value: None,
            hint: None,
            command: None,
            span: None,
        }))
    }

    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.arg = Some(arg.into());
        self
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_optional_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint.or(self.hint.take());
        self
    }

    pub fn with_command_if_missing(mut self, command: &str, span: Span) -> Self {
        if self.command.is_none() {
            self.command = Some(command.to_string());
        }
        if self.span.is_none() {
            self.span = Some(span);
        }
        self
    }

    pub fn io(err: &io::Error, path: &str) -> Self {
        let code = match err.kind() {
            io::ErrorKind::NotFound => ErrorCode::NotFound,
            io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
            _ => ErrorCode::IoError,
        };
        ShellError::new(code, format!("{path}: {err}")).with_value(path)
    }

    pub fn type_error(what: &str, expected: &str, got: &Value) -> Self {
        ShellError::new(ErrorCode::TypeError, format!("{what} must be {expected}, got {}", got.type_name()))
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ShellError::new(ErrorCode::Internal, message)
    }

    pub fn to_info(&self, rolled_back: bool) -> ErrorInfo {
        ErrorInfo {
            code: self.code,
            message: self.message.clone(),
            arg: self.arg.clone(),
            value: self.value.clone(),
            hint: self.hint.clone(),
            command: self.command.clone(),
            span: self.span,
            rolled_back,
            kept: false,
            restored: Vec::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        let mut pairs = vec![("code", Value::str(self.code.as_str())), ("message", Value::str(&self.message))];
        if let Some(hint) = &self.hint {
            pairs.push(("hint", Value::str(hint)));
        }
        if let Some(command) = &self.command {
            pairs.push(("command", Value::str(command)));
        }
        Value::record(pairs)
    }
}

impl From<ParseError> for ShellError {
    fn from(err: ParseError) -> Self {
        let mut shell_error = ShellError::new(ErrorCode::ParseError, err.message);
        shell_error.span = Some(err.span);
        shell_error.hint = err.hint;
        shell_error
    }
}

impl Deref for ShellError {
    type Target = ErrorData;

    fn deref(&self) -> &ErrorData {
        &self.0
    }
}

impl DerefMut for ShellError {
    fn deref_mut(&mut self) -> &mut ErrorData {
        &mut self.0
    }
}
