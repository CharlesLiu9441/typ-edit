//! Single interpretation of a parameter's [`CastInfo`].
//!
//! Both the hover renderer ([`crate::hover`]) and the value-slot candidates
//! ([`crate::values`]) answer "what does this parameter accept", so the walk
//! over the cast tree and the type→name/units mapping live here once.

use typst::foundations::{CastInfo, Repr, Type, Value};

/// One alternative a cast accepts, flattened out of any unions.
pub(crate) enum Accepted<'a> {
    /// Any value is accepted.
    Any,
    /// One specific value, with its short documentation.
    Value(&'a Value, &'static str),
    /// A whole type.
    Type(&'a Type),
}

impl Accepted<'_> {
    /// The display name of this alternative: `any`, a value's repr, or the
    /// type's long name (which is what [`Type`]'s `Display` prints).
    pub(crate) fn display(&self) -> String {
        match self {
            Accepted::Any => "any".to_owned(),
            Accepted::Value(value, _) => value.repr().into(),
            Accepted::Type(ty) => ty.to_string(),
        }
    }
}

/// Walks `info` in declaration order, flattening unions, and yields each
/// accepted alternative. Union nodes are descended into, never yielded.
pub(crate) fn for_each_accepted(info: &CastInfo, mut f: impl FnMut(Accepted<'_>)) {
    info.walk(|part| match part {
        CastInfo::Any => f(Accepted::Any),
        CastInfo::Value(value, docs) => f(Accepted::Value(value, docs)),
        CastInfo::Type(ty) => f(Accepted::Type(ty)),
        CastInfo::Union(_) => {}
    });
}

/// The hardcoded candidate set a primitive type accepts, keyed by its long name.
pub(crate) struct TypeSpec {
    /// The type's long name.
    name: &'static str,
    /// Fixed values `(insert, docs)`.
    pub(crate) plain: &'static [(&'static str, &'static str)],
    /// Constructor candidates `(insert, docs)`; the caret lands inside `()`.
    pub(crate) constructors: &'static [(&'static str, &'static str)],
    /// Bare unit-bearing candidates, inserted as typed, without docs.
    pub(crate) units: &'static [&'static str],
}

/// The candidate spec for `ty`, or `None` when the type has no hardcoded table.
pub(crate) fn type_spec(ty: &Type) -> Option<&'static TypeSpec> {
    SPECS.iter().find(|spec| spec.name == ty.long_name())
}

static SPECS: &[TypeSpec] = &[
    TypeSpec {
        name: "none",
        plain: &[("none", "Nothing.")],
        constructors: &[],
        units: &[],
    },
    TypeSpec {
        name: "auto",
        plain: &[("auto", "A smart default.")],
        constructors: &[],
        units: &[],
    },
    TypeSpec {
        name: "boolean",
        plain: &[("false", "No / Disabled."), ("true", "Yes / Enabled.")],
        constructors: &[],
        units: &[],
    },
    TypeSpec {
        name: "color",
        plain: &[],
        constructors: &[
            ("luma()", "A custom grayscale color."),
            ("rgb()", "A custom RGBA color."),
            ("cmyk()", "A custom CMYK color."),
            ("oklab()", "A custom Oklab color."),
            ("oklch()", "A custom Oklch color."),
            ("color.linear-rgb()", "A custom linear RGBA color."),
            ("color.hsv()", "A custom HSVA color."),
            ("color.hsl()", "A custom HSLA color."),
        ],
        units: &[],
    },
    TypeSpec {
        name: "gradient",
        plain: &[],
        constructors: &[
            ("gradient.linear()", "A linear gradient."),
            ("gradient.radial()", "A radial gradient."),
            ("gradient.conic()", "A conic gradient."),
        ],
        units: &[],
    },
    TypeSpec {
        name: "length",
        plain: &[],
        constructors: &[],
        units: &["0pt", "0mm", "0cm", "0in", "0em"],
    },
    TypeSpec {
        name: "relative length",
        plain: &[],
        constructors: &[],
        units: &["0%", "0em"],
    },
    TypeSpec {
        name: "angle",
        plain: &[],
        constructors: &[],
        units: &["0deg", "0rad", "0grad", "0turn"],
    },
    TypeSpec {
        name: "ratio",
        plain: &[],
        constructors: &[],
        units: &["0%"],
    },
    TypeSpec {
        name: "fraction",
        plain: &[],
        constructors: &[],
        units: &["1fr"],
    },
    TypeSpec {
        name: "stroke",
        plain: &[],
        constructors: &[],
        units: &["none", "1pt"],
    },
];
