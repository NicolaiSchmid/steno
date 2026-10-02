//! The seven routes of the wire, matched from method and path.
//! Swift: `Routing/Route.swift`.

use http::Method;
use uuid::Uuid;

use crate::configuration::HandoverConfiguration;
use crate::pairing::parse_uuid;

/// What a request must carry before its body is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// `/v1/hello`: anyone who completed the pinned handshake.
    None,
    /// `Authorization: Pairing <secret>` from the QR code.
    Pairing,
    /// `Authorization: Bearer <token>` of a paired device.
    Bearer,
}

/// The seven routes. Anything else is 404 and never has its body read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Hello,
    Pair,
    Unpair,
    Announce(Uuid),
    Status(Uuid),
    Chunk(Uuid, i64),
    Complete(Uuid),
}

impl Route {
    /// The route for `method` and `uri` (path, optionally with a query).
    #[must_use]
    pub fn matches(method: &Method, uri: &str) -> Option<Route> {
        let path = uri.split_once('?').map_or(uri, |(path, _)| path);
        let parts: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                percent_encoding::percent_decode_str(part)
                    .decode_utf8()
                    .map_or_else(|_| part.to_owned(), std::borrow::Cow::into_owned)
            })
            .collect();
        if parts.first().map(String::as_str) != Some("v1") {
            return None;
        }
        let part = |index: usize| parts.get(index).map(String::as_str);
        match (method, parts.len()) {
            (&Method::GET, 2) if part(1) == Some("hello") => Some(Route::Hello),
            (&Method::POST, 2) if part(1) == Some("pair") => Some(Route::Pair),
            (&Method::DELETE, 2) if part(1) == Some("pairing") => Some(Route::Unpair),
            (&Method::PUT, 3) if part(1) == Some("recordings") => {
                parse_uuid(part(2)?).map(Route::Announce)
            }
            (&Method::GET, 3) if part(1) == Some("recordings") => {
                parse_uuid(part(2)?).map(Route::Status)
            }
            (&Method::PUT, 5) if part(1) == Some("recordings") && part(3) == Some("chunks") => {
                let id = parse_uuid(part(2)?)?;
                let text = part(4)?;
                let index: i64 = text.parse().ok()?;
                (index >= 0 && index.to_string() == text).then_some(Route::Chunk(id, index))
            }
            (&Method::POST, 4) if part(1) == Some("recordings") && part(3) == Some("complete") => {
                parse_uuid(part(2)?).map(Route::Complete)
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn auth(self) -> AuthRequirement {
        match self {
            Route::Hello => AuthRequirement::None,
            Route::Pair => AuthRequirement::Pairing,
            Route::Unpair
            | Route::Announce(_)
            | Route::Status(_)
            | Route::Chunk(..)
            | Route::Complete(_) => AuthRequirement::Bearer,
        }
    }

    /// Chunk bodies may be a chunk plus 64 KiB; everything else is small
    /// JSON.
    #[must_use]
    pub fn body_limit(self, configuration: &HandoverConfiguration) -> i64 {
        match self {
            Route::Chunk(..) => configuration.chunk_body_limit(),
            _ => HandoverConfiguration::JSON_BODY_LIMIT,
        }
    }
}
