//! A local sink for blocked hostnames.
//!
//! # Why this exists
//!
//! Answering a blocked name with `0.0.0.0` or `NXDOMAIN` tells the client the
//! resource is unreachable, and a browser turns that into a failed request. Two
//! things follow, and both are worse than they sound:
//!
//! * The page waits. Depending on the client's network stack, connecting to
//!   `0.0.0.0` is refused instantly on one machine and hangs until timeout on
//!   another, so a blocked third-party script can stall rendering for seconds.
//! * The failure is loud. `<script src=…>` fires `onerror`, and "did the ad
//!   script fail to load?" is the single most common way a site detects a
//!   blocker and puts up a wall.
//!
//! Pointing blocked names at this responder instead means the connection is
//! accepted and answered immediately with a valid, empty resource. Nothing
//! hangs, `onerror` does not fire, and the console stays quiet.
//!
//! # What this deliberately does NOT do
//!
//! It does not fetch ads, and it does not report impressions. A DNS resolver
//! hands back an address; it never loads a page or fires a tracking pixel, so
//! there is no impression for it to signal. Fabricating one would mean asking
//! advertisers to pay for something no person ever saw, which is fraud against
//! the advertiser rather than a defence against tracking.
//!
//! # The limit worth knowing before enabling it
//!
//! This only helps for plain **HTTP**. An `https://` ad script makes the client
//! open TLS to this responder for a hostname we hold no certificate for, so the
//! handshake fails and the browser reports an error exactly as before. Most
//! third-party ad and tracker resources are HTTPS today, so a site determined to
//! detect blocking still can. Serving a valid certificate would mean issuing one
//! per blocked domain from a CA installed on every device in the house — a
//! machine-in-the-middle of your own network, which is a far bigger security
//! decision than ad blocking and is not something this does.
//!
//! What it reliably buys is the first point: failures become instant and
//! uniform instead of stalling, which is the difference most people actually
//! notice.

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

/// The smallest valid transparent GIF, 43 bytes. Serving a real image rather
/// than an empty body matters: an `<img>` with a zero-length body fires
/// `onerror` in some browsers, which is the signal being avoided.
const TRANSPARENT_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xFF, 0xFF, 0xFF, 0x21, 0xF9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3B,
];

/// A 1x1 transparent PNG, 67 bytes, for callers that asked for a `.png`.
const TRANSPARENT_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// What to answer a blocked request with, chosen from the requested path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkResponse {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

/// Pick a response body from the request path.
///
/// The content type has to be plausible for what was asked for. A script tag
/// served `text/html` is a console error and, in strict-MIME browsers, a load
/// failure — which puts the `onerror` signal right back.
#[must_use]
pub fn response_for_path(path: &str) -> SinkResponse {
    // Query strings and fragments are not part of the extension.
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();

    let extension = path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");

    match extension {
        "js" | "mjs" | "jsonp" => SinkResponse {
            // An empty script is a *successful* script: it parses, runs, and
            // does nothing, so the loader's success path is taken.
            content_type: "application/javascript",
            body: b"",
        },
        "css" => SinkResponse {
            content_type: "text/css",
            body: b"",
        },
        "json" => SinkResponse {
            // `{}` rather than empty: an empty body is a JSON parse error, and
            // a parse error is an exception the caller can notice.
            content_type: "application/json",
            body: b"{}",
        },
        "png" => SinkResponse {
            content_type: "image/png",
            body: TRANSPARENT_PNG,
        },
        "gif" | "jpg" | "jpeg" | "webp" | "bmp" | "ico" => SinkResponse {
            content_type: "image/gif",
            body: TRANSPARENT_GIF,
        },
        "svg" => SinkResponse {
            content_type: "image/svg+xml",
            body: b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        },
        "xml" => SinkResponse {
            content_type: "application/xml",
            body: b"<?xml version=\"1.0\"?><root/>",
        },
        "txt" => SinkResponse {
            content_type: "text/plain",
            body: b"",
        },
        // Extensionless paths are overwhelmingly tracking beacons and XHR
        // endpoints. An empty 200 is the quietest thing to hand back.
        _ => SinkResponse {
            content_type: "text/html",
            body: b"",
        },
    }
}

async fn handle(request: Request) -> Response {
    let sink = response_for_path(request.uri().path());
    let mut response = (StatusCode::OK, Body::from(sink.body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(sink.content_type),
    );
    // Never cached: the block decision belongs to the resolver, and a cached
    // empty script would keep being served after the domain is allowed again.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    // Says plainly what happened, for anyone reading a HAR file and wondering
    // why an ad script is 0 bytes.
    headers.insert(
        "x-cogwheel-blocked",
        HeaderValue::from_static("1; served by the local sinkhole"),
    );
    response
}

/// The router: every method, every path, one answer.
pub fn router() -> Router {
    Router::new().fallback(any(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_served_as_empty_javascript() {
        // The important case: a *successful* empty script means onerror never
        // fires, which is the detection signal being avoided.
        for path in ["/ads.js", "/a/b/tracker.JS", "/pagead/show_ads.js?client=x"] {
            let sink = response_for_path(path);
            assert_eq!(sink.content_type, "application/javascript", "{path}");
            assert!(sink.body.is_empty(), "{path}");
        }
    }

    #[test]
    fn images_are_real_images_not_empty_bodies() {
        // A zero-length image fires onerror in some browsers, which would put
        // the signal straight back.
        let gif = response_for_path("/pixel.gif");
        assert_eq!(gif.content_type, "image/gif");
        assert_eq!(&gif.body[..3], b"GIF");

        let png = response_for_path("/beacon.png");
        assert_eq!(png.content_type, "image/png");
        assert_eq!(&png.body[1..4], b"PNG");
    }

    #[test]
    fn json_is_parseable_rather_than_empty() {
        let sink = response_for_path("/api/track.json");
        assert_eq!(sink.content_type, "application/json");
        assert_eq!(sink.body, b"{}");
        serde_json::from_slice::<serde_json::Value>(sink.body).expect("must parse as JSON");
    }

    #[test]
    fn a_query_string_does_not_confuse_the_extension() {
        let sink = response_for_path("/t.gif?u=https://example.com/thing.js");
        assert_eq!(sink.content_type, "image/gif");
    }

    #[test]
    fn extensionless_beacons_get_an_empty_ok() {
        for path in ["/collect", "/", "/v1/track/event"] {
            let sink = response_for_path(path);
            assert_eq!(sink.content_type, "text/html", "{path}");
            assert!(sink.body.is_empty(), "{path}");
        }
    }

    #[test]
    fn the_embedded_pixels_are_valid_files() {
        assert_eq!(TRANSPARENT_GIF.len(), 43);
        assert_eq!(&TRANSPARENT_GIF[..6], b"GIF89a");
        assert_eq!(&TRANSPARENT_GIF[TRANSPARENT_GIF.len() - 1..], b";");
        assert_eq!(
            &TRANSPARENT_PNG[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        assert_eq!(
            &TRANSPARENT_PNG[TRANSPARENT_PNG.len() - 8..],
            b"IEND\xAE\x42\x60\x82"
        );
    }
}
