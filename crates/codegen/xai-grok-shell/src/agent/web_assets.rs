//! The browser client, as bytes the gateway can serve.
//!
//! # Why the page is inside the binary
//!
//! Until now `sdk/web` was reachable only through `vite`, which made the browser client two
//! processes: one for the page and one for the agent. That is a development arrangement — the user
//! asked for the web UI to be *part of the application*, started the way the rest of it is started.
//! A single self-contained binary is also how this product ships, and a page served out of the same
//! process as `/ws` needs no address typed into it, because it already knows its own origin.
//!
//! The cost is that the bundle has to exist before `cargo build` runs. That cost is paid where it
//! belongs: `build.rs` *reads* `sdk/web/dist`, it never builds it. A `build.rs` that shelled out to
//! `bun` would make every Rust build — `cargo check` included, and the network-free nix build
//! especially — depend on a JavaScript toolchain, to regenerate an artifact that changes far less
//! often than the Rust around it.
//!
//! # What happens when it was not built
//!
//! The binary still builds and `/ws` still works; only [`ASSETS`] is empty. Every page request then
//! answers [`NOT_BUILT_PAGE`] — 503, naming both ways to fix it — because a blank page from a
//! server that is running perfectly well is the worst of the available failures: nothing on screen
//! says whether the agent is down, the secret is wrong, or the bundle was never made.
//!
//! Under nix this is the normal outcome rather than an edge case: `sdk/web/dist` is gitignored
//! (`sdk/web/.gitignore`, "a committed bundle is a second copy of the source that nothing checks"),
//! and a flake's source is its *tracked* files, so `nix build` cannot see a bundle even when one is
//! sitting in the working tree. A nix build that wants the page has to build the client in its own
//! derivation and hand the path over in `GROK_WEB_DIST`, the same shape as `GROK_SHELL_BUNDLE_RG_PATH`
//! for the ripgrep the sandbox cannot download.
//!
//! # `GROK_WEB_ROOT`
//!
//! A directory read at *runtime*, which wins over the embedded copy when it is set. Two uses, both
//! narrow: rebuilding the client without recompiling Rust while working on `sdk/web`, and giving a
//! binary built without a bundle (a nix one) a page to serve. It is off unless the variable is set,
//! so it is not a second shipping mechanism — the embedded copy is the product.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

#[cfg(web_ui)]
include!(concat!(env!("OUT_DIR"), "/web_assets_generated.rs"));

/// No bundle was baked in. See the module docs; the page route says so out loud.
#[cfg(not(web_ui))]
pub(super) static ASSETS: &[(&str, &[u8])] = &[];

/// Runtime override for the embedded bundle.
pub const WEB_ROOT_ENV: &str = "GROK_WEB_ROOT";

/// The entry every unmatched route falls back to, so a reloaded `/s/<id>` is still the app.
const INDEX: &str = "index.html";

/// True when this binary carries a browser client.
pub fn is_built_in() -> bool {
    !ASSETS.is_empty()
}

/// The directory being served instead of the embedded bundle, if any.
pub fn web_root_override() -> Option<PathBuf> {
    std::env::var_os(WEB_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Shown when neither an embedded bundle nor [`WEB_ROOT_ENV`] can produce a page.
///
/// Deliberately plain HTML with no assets of its own: it has to render from a server whose entire
/// reason for existing here is that it has no assets.
const NOT_BUILT_PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>grok — the browser client was not built in</title>
<style>
  body { font: 15px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; margin: 4rem auto; max-width: 46rem; padding: 0 1.5rem; }
  code { background: rgba(127,127,127,.18); padding: .1em .35em; border-radius: 3px; }
  li { margin: .6rem 0; }
</style>
<h1>The browser client was not built into this binary.</h1>
<p>The gateway itself is running: <code>/ws</code> accepts clients as usual, so a
client started some other way still works. There is just no page to serve.</p>
<p>Two ways to get one:</p>
<ul>
  <li>build the bundle, then rebuild grok:
      <code>cd sdk/web &amp;&amp; bun install &amp;&amp; bun run build</code></li>
  <li>or point this process at a bundle you already have, with no rebuild:
      <code>GROK_WEB_ROOT=/path/to/sdk/web/dist</code></li>
</ul>
"#;

/// [`NOT_BUILT_PAGE`], plus the one sentence that only applies when an override is pointing
/// somewhere empty — otherwise the page would tell someone to set a variable they already set.
fn not_built_page() -> String {
    match web_root_override() {
        Some(root) => format!(
            "{NOT_BUILT_PAGE}<p>{WEB_ROOT_ENV} is set to <code>{}</code>, and there is no \
             <code>index.html</code> there. That directory wins over the copy built into this \
             binary, so unsetting it is a third way out.</p>\n",
            root.display()
        ),
        None => NOT_BUILT_PAGE.to_string(),
    }
}

/// Answer one page request.
///
/// Unknown paths fall back to `index.html` because the client routes on the path — `/s/<id>` and
/// `/d/<cwd>` are pages, not files, and a reload of one has to reach the app rather than a 404.
/// Anything under `assets/` is excluded from that fallback: those names are content-hashed by the
/// bundler, so a miss there is a stale or wrong build, and answering it with HTML would surface as
/// an unreadable syntax error in the console instead of a plain 404.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { INDEX } else { path };

    let Some(relative) = safe_relative_path(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(body) = load(&relative) {
        return asset_response(&relative, body);
    }
    if relative.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match load(INDEX) {
        Some(body) => asset_response(INDEX, body),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            not_built_page(),
        )
            .into_response(),
    }
}

/// Reject anything that is not a plain relative path under the bundle.
///
/// The page route carries no authentication (see [`crate::agent::web_gateway`]), and with
/// [`WEB_ROOT_ENV`] set it reaches the filesystem, so `..` and absolute paths have to die here
/// rather than in whatever `join` does with them.
///
/// The escapes are decoded **before** the path is split, which is the safe direction rather than
/// the letter of the URI spec: `%2f` is not a separator to a parser, so `..%2f..%2fetc` would
/// otherwise arrive as one innocent-looking segment. `Uri::path` hands over the raw path — axum
/// only decodes inside its `Path` extractor, which is not what a fallback handler gets — so doing
/// it here is not a second decode of an already-decoded string.
fn safe_relative_path(path: &str) -> Option<String> {
    let decoded = urlencoding::decode(path).ok()?;
    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        if segment.contains('\\') || segment.contains('\0') {
            return None;
        }
        segments.push(segment);
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments.join("/"))
}

/// The bytes for one bundle-relative path, from the override directory or the embedded table.
fn load(relative: &str) -> Option<Vec<u8>> {
    if let Some(root) = web_root_override() {
        let mut path = root;
        for segment in relative.split('/') {
            path.push(segment);
        }
        return std::fs::read(path).ok();
    }
    ASSETS
        .iter()
        .find(|(name, _)| *name == relative)
        .map(|(_, bytes)| bytes.to_vec())
}

fn asset_response(relative: &str, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(relative)),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(relative)),
    );
    // The page holds a credential for this machine's agent in `localStorage`, and every control on
    // it does something. Framing it from another origin cannot read that storage, but it can put a
    // transparent copy of it under someone's cursor, and nothing here is worth embedding elsewhere.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

/// `assets/` is content-hashed by the bundler; everything else has a stable name and must not stick.
fn cache_control(relative: &str) -> &'static str {
    if relative.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Media type by extension.
///
/// A table rather than a crate: this serves one bundler's output, the whole of which is the handful
/// of types below, and a wrong guess on a script or a stylesheet is a page that silently does not
/// run. `application/octet-stream` is the honest answer for anything unlisted.
fn content_type(relative: &str) -> &'static str {
    let extension = Path::new(relative)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    match extension {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page route is unauthenticated and, with an override set, reads real files. Every way of
    /// spelling "leave the bundle" has to be refused before a path is built at all.
    #[test]
    fn a_path_may_not_leave_the_bundle() {
        assert_eq!(
            safe_relative_path("assets/app.js").as_deref(),
            Some("assets/app.js")
        );
        assert_eq!(safe_relative_path(".."), None);
        assert_eq!(safe_relative_path("../../etc/passwd"), None);
        assert_eq!(safe_relative_path("assets/../../etc/passwd"), None);
        assert_eq!(safe_relative_path("assets//app.js"), None);
        assert_eq!(safe_relative_path("."), None);
        assert_eq!(safe_relative_path(""), None);
        // Backslashes are path separators on Windows and would otherwise pass as one segment.
        assert_eq!(safe_relative_path("assets\\..\\secret"), None);
        // Escaped separators are decoded first, so this is `../../etc/passwd` and not one segment.
        assert_eq!(safe_relative_path("..%2f..%2fetc%2fpasswd"), None);
        assert_eq!(safe_relative_path("%2e%2e/etc"), None);
        // A path that is not UTF-8 once decoded names nothing in the bundle.
        assert_eq!(safe_relative_path("%ff%fe"), None);
    }

    /// A script served as `text/plain` is a page that loads and does nothing, with no error that
    /// points at the server.
    #[test]
    fn scripts_and_styles_get_types_a_browser_will_execute() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-abc.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-abc.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("assets/logo.svg"), "image/svg+xml");
        assert_eq!(content_type("unknown.bin"), "application/octet-stream");
    }

    /// `index.html` names the hashed bundle, so a cached copy of it pins the whole app to an old
    /// build across an upgrade of this binary.
    #[test]
    fn only_hashed_assets_are_cached_forever() {
        assert_eq!(cache_control("index.html"), "no-cache");
        assert!(cache_control("assets/index-abc.js").contains("immutable"));
    }

    #[tokio::test]
    async fn a_missing_asset_is_a_404_rather_than_the_app() {
        let response = serve("/assets/not-here.js".parse::<Uri>().unwrap()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
