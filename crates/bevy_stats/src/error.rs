//! Typed errors produced by the stat system.

use bevy_ecs::entity::Entity;
use core::fmt;

/// Any error produced by the stat system.
#[derive(Debug, Clone, PartialEq)]
pub enum StatError {
    /// An expression, stat path, or tag list failed to parse.
    Parse(ParseError),
    /// A tag name was referenced but never registered in the
    /// [`TagRegistry`](crate::TagRegistry).
    UnknownTag(String),
    /// More than 64 distinct leaf tags were registered.
    TagCapacity {
        /// The tag whose registration exceeded the capacity.
        tag: String,
    },
    /// The entity does not carry a [`Stats`](crate::Stats) component.
    NoStats(Entity),
    /// A [`ModifierHandle`](crate::ModifierHandle) did not correspond to a
    /// live modifier on the given stat.
    UnknownModifier {
        /// The stat the removal was attempted on.
        stat: String,
    },
    /// An effect referenced a role that was not provided in the
    /// [`Roles`](crate::Roles) map.
    UnknownRole(String),
    /// A stat path had invalid structure (for example, a tag filter passed to
    /// an API that does not accept one).
    InvalidPath {
        /// The offending path.
        path: String,
        /// Human-readable reason the path was rejected.
        reason: &'static str,
    },
}

impl fmt::Display for StatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatError::Parse(e) => write!(f, "parse error: {e}"),
            StatError::UnknownTag(t) => write!(f, "unknown tag `{t}` (not registered)"),
            StatError::TagCapacity { tag } => {
                write!(f, "cannot register tag `{tag}`: 64-tag capacity exceeded")
            }
            StatError::NoStats(e) => write!(f, "entity {e} has no `Stats` component"),
            StatError::UnknownModifier { stat } => {
                write!(f, "no such modifier on stat `{stat}`")
            }
            StatError::UnknownRole(r) => write!(f, "no entity bound to role `{r}`"),
            StatError::InvalidPath { path, reason } => {
                write!(f, "invalid stat path `{path}`: {reason}")
            }
        }
    }
}

impl core::error::Error for StatError {}

impl From<ParseError> for StatError {
    fn from(value: ParseError) -> Self {
        StatError::Parse(value)
    }
}

/// A syntax error in an expression, stat path, or tag list.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// What went wrong.
    pub kind: ParseErrorKind,
    /// Byte offset into the source string where the error occurred.
    pub offset: usize,
    /// The source text being parsed.
    pub src: String,
}

/// The specific kind of [`ParseError`].
#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
    /// A character that cannot start any token.
    UnexpectedChar(char),
    /// A numeric literal that failed to parse.
    InvalidNumber,
    /// A token that is not valid at this position.
    UnexpectedToken(String),
    /// The source ended while more input was expected.
    UnexpectedEnd,
    /// A call to a function the expression language does not define.
    UnknownFunction(String),
    /// A known function called with the wrong number of arguments.
    WrongArity {
        /// The function name.
        function: &'static str,
        /// Human-readable description of the accepted arity.
        expected: &'static str,
        /// The number of arguments found.
        found: usize,
    },
    /// The whole source parsed but trailing input remained.
    TrailingInput,
    /// An empty expression, path, or tag filter.
    Empty,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ParseErrorKind::UnexpectedChar(c) => write!(f, "unexpected character `{c}`")?,
            ParseErrorKind::InvalidNumber => write!(f, "invalid numeric literal")?,
            ParseErrorKind::UnexpectedToken(t) => write!(f, "unexpected token `{t}`")?,
            ParseErrorKind::UnexpectedEnd => write!(f, "unexpected end of input")?,
            ParseErrorKind::UnknownFunction(name) => write!(f, "unknown function `{name}`")?,
            ParseErrorKind::WrongArity {
                function,
                expected,
                found,
            } => write!(
                f,
                "function `{function}` expects {expected} argument(s), found {found}"
            )?,
            ParseErrorKind::TrailingInput => write!(f, "unexpected trailing input")?,
            ParseErrorKind::Empty => write!(f, "empty input")?,
        }
        write!(f, " at offset {} in `{}`", self.offset, self.src)
    }
}

impl core::error::Error for ParseError {}
