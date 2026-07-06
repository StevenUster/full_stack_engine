use std::fmt;

/// A schema error: unparseable source, an unsupported type, an invalid
/// attribute combination or an unresolvable reference. Always carries a
/// message that names the offending struct/field so the CLI and the derive
/// can surface it directly.
#[derive(Debug, Clone)]
pub struct Error {
    pub message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
