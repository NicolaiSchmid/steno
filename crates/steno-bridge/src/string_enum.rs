//! [`string_enum!`], the one way this crate spells a Swift `String` enum, and
//! the error its `FromStr` returns.
//!
//! A macro rather than `strum`: the raw values live next to the variants, and
//! the macro gives a `const fn as_str` and a `const ALL` slice, which the
//! dispatcher's routing and the contract tests use at compile time; `strum`
//! would add a dependency for a weaker version of the same four impls. The
//! enums are deliberately not `#[non_exhaustive]`: the dispatcher matches
//! every `BridgeMethod` exhaustively, so a method added to the contract fails
//! to compile until it is routed, which is the check we want.

/// A string enum with the exact raw values Swift's `String` enums encode:
/// serde renames, `ALL` (Swift's `CaseIterable`), `as_str`, `Display` and
/// `FromStr` (Swift's `init?(rawValue:)`).
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $raw:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize,
        )]
        $vis enum $name {
            $( $(#[$vmeta])* #[serde(rename = $raw)] $variant ),+
        }

        impl $name {
            /// Every case, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The raw value on the wire.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $raw),+ }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::string_enum::UnknownRawValue;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                match raw {
                    $($raw => Ok(Self::$variant),)+
                    _ => Err($crate::string_enum::UnknownRawValue {
                        type_name: stringify!($name),
                        raw: raw.to_owned(),
                    }),
                }
            }
        }
    };
}
pub(crate) use string_enum;

/// A raw string that is not a case of the enum (Swift's `init?(rawValue:)`
/// returning nil).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("'{raw}' is not a {type_name}")]
pub struct UnknownRawValue {
    pub type_name: &'static str,
    pub raw: String,
}
