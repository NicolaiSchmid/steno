//! `string_enum!`, the one way Steno's Rust crates spell a Swift `String`
//! enum, and the error its `FromStr` returns.
//!
//! A macro rather than `strum`: the case names live next to the variants,
//! and the macro gives a `const fn as_str` and a `const ALL` slice (Swift's
//! `CaseIterable`) that routing tables and contract tests use at compile
//! time. The enums are deliberately not `#[non_exhaustive]`: a case Swift
//! adds fails to compile here until every match handles it.

use thiserror::Error;

/// A case name neither Swift nor Rust knows (Swift's `init?(rawValue:)`
/// returning nil); an unreadable row or message fails instead of becoming
/// a default case.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown {type_name} case {value:?}")]
pub struct UnknownCase {
    pub type_name: &'static str,
    pub value: String,
}

/// A string-backed enum: the case names Swift writes to JSON and to the
/// database, with `ALL`, `as_str`, `FromStr`, `Display` and serde in that
/// form.
#[macro_export]
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every case, in declaration order (Swift's `allCases`).
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The case name Swift writes to JSON and to the database.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::string_enum::UnknownCase;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    other => Err($crate::string_enum::UnknownCase {
                        type_name: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <String as ::serde::Deserialize>::deserialize(deserializer)?;
                text.parse().map_err(::serde::de::Error::custom)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    string_enum! {
        enum Colour {
            Red = "red",
            Green = "green",
        }
    }

    #[test]
    fn cases_round_trip_by_name() {
        assert_eq!(Colour::ALL, &[Colour::Red, Colour::Green]);
        assert_eq!("green".parse::<Colour>(), Ok(Colour::Green));
        assert_eq!(Colour::Red.to_string(), "red");
        assert_eq!(serde_json::to_string(&Colour::Red).unwrap(), "\"red\"");
        let error = "blue".parse::<Colour>().unwrap_err();
        assert_eq!(error.to_string(), "unknown Colour case \"blue\"");
    }
}
