use std::error::Error;
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerError {
    message: String,
    position: Option<usize>,
}

impl CompilerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: None,
        }
    }

    pub fn at(position: usize, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: Some(position),
        }
    }
}

impl Display for CompilerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.position {
            Some(position) => write!(f, "compile error at byte {position}: {}", self.message),
            None => write!(f, "compile error: {}", self.message),
        }
    }
}

impl Error for CompilerError {}

pub type CompilerResult<T> = Result<T, CompilerError>;
