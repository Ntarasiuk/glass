//! Port forwarding: an owner's tailnet device opens `/fwd/<port>/…` on the host's web
//! server and gets that local port (say a dev server on 3000), without exposing it.
//!
//! TODO(forward agent): the proxy. Interface is fixed.

/// Routes merged into the web server.
pub fn routes() -> axum::Router {
    axum::Router::new()
}
