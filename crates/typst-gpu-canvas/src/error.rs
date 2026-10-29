//! Errors returned by the GPU canvas renderer.

use std::fmt;

/// An error that prevented a page from being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderError {
    /// The requested scale was not finite (`NaN` or infinite).
    NonFiniteScale,
    /// The requested page position was not finite (`NaN` or infinite).
    NonFinitePosition,
    /// The GPU canvas renderer cannot draw some content in the page; the host
    /// should fall back to the raster path for the whole page.
    UnsupportedCanvasContent(&'static str),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteScale => f.write_str("scale must be finite"),
            Self::NonFinitePosition => f.write_str("page position must be finite"),
            Self::UnsupportedCanvasContent(kind) => {
                write!(f, "gpu canvas does not support {kind}")
            }
        }
    }
}

impl std::error::Error for RenderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_scale_displays_a_message() {
        assert_eq!(
            RenderError::NonFiniteScale.to_string(),
            "scale must be finite"
        );
    }

    #[test]
    fn non_finite_position_displays_a_message() {
        assert_eq!(
            RenderError::NonFinitePosition.to_string(),
            "page position must be finite"
        );
    }

    #[test]
    fn render_error_is_a_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<RenderError>();
    }
}
