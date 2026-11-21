//! Errors produced by the rendering layer.

use std::{error::Error, fmt};

/// Everything that can go wrong while setting up or driving the renderer.
///
/// Deliberately free of graphics API types, so code outside this crate never
/// depends on the backend that produced the error.
#[derive(Debug)]
pub enum RenderError {
    /// The graphics backend reported a failure. The details are in the wrapped error.
    Backend(Box<dyn Error + Send + Sync>),
    /// No GPU supports everything the renderer needs.
    NoSuitableGpu,
    /// The window surface lacks a capability the renderer requires.
    Unsupported(&'static str),
    /// A shader could not be read or compiled. The text names the file and
    /// carries the compiler's message.
    Shader(String),
    /// Mesh data from the caller is malformed, for example an index points past the
    /// last vertex. The text says what is wrong.
    InvalidMesh(&'static str),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(e) => write!(f, "graphics backend error: {e}"),
            Self::NoSuitableGpu => write!(f, "no GPU with the required support was found"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::Shader(details) => write!(f, "shader error in {details}"),
            Self::InvalidMesh(what) => write!(f, "invalid mesh: {what}"),
        }
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(e) => Some(e.as_ref()),
            Self::NoSuitableGpu | Self::Unsupported(_) | Self::Shader(_) | Self::InvalidMesh(_) => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_mesh_names_the_problem() {
        let text = RenderError::InvalidMesh("an index points past the last vertex").to_string();
        assert_eq!(text, "invalid mesh: an index points past the last vertex");
    }
}
