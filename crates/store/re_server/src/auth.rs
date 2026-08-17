//! Token authentication for the catalog server.
//!
//! Enabled by starting the server with a signing secret (`--token-secret` /
//! `RERUN_SERVER_TOKEN_SECRET`). Tokens are minted offline with the same secret via
//! `rerun server generate-token`; clients pass them as
//! `CatalogClient(url, token=...)` — the SDK attaches the standard
//! `Authorization: Bearer` header on every request.

#[cfg(not(target_arch = "wasm32"))]
use re_auth::server::{Authenticator, UserContext};
#[cfg(not(target_arch = "wasm32"))]
use tonic::{Request, Status, service::Interceptor};

/// No-op on wasm: token authentication is a native-server concern.
#[cfg(target_arch = "wasm32")]
pub fn require_write_permission<T>(_req: &tonic::Request<T>) -> tonic::Result<()> {
    Ok(())
}

/// Authentication interceptor: every gRPC request must carry a valid Bearer token.
///
/// Wraps `re_auth`'s [`Authenticator`] — which validates tokens when present but lets
/// token-less requests through — and turns "no token" into a hard `Unauthenticated`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct RequireAuth {
    inner: Authenticator,
}

#[cfg(not(target_arch = "wasm32"))]
impl RequireAuth {
    pub fn new(provider: re_auth::RedapProvider) -> Self {
        Self {
            inner: Authenticator::new(provider),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Interceptor for RequireAuth {
    fn call(&mut self, req: Request<()>) -> tonic::Result<Request<()>> {
        let req = self.inner.call(req)?;
        if req.extensions().get::<UserContext>().is_none() {
            return Err(Status::unauthenticated(
                re_auth::ERROR_MESSAGE_MISSING_CREDENTIALS,
            ));
        }
        Ok(req)
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Reject a mutating request unless the caller's token carries read-write permission.
///
/// Call at the top of every RPC handler that creates, modifies, or deletes anything.
/// No-op when token authentication is disabled (no [`UserContext`] in the request —
/// with authentication on, [`RequireAuth`] guarantees the context is present).
pub fn require_write_permission<T>(req: &tonic::Request<T>) -> tonic::Result<()> {
    match req.extensions().get::<UserContext>() {
        None => Ok(()), // token authentication is disabled
        Some(ctx) if ctx.has_write_permission() => Ok(()),
        Some(ctx) => Err(Status::permission_denied(format!(
            "user '{}' has read-only access; this operation requires a read-write token",
            ctx.user_id
        ))),
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Verify the `Authorization: Bearer` header of a plain HTTP request.
///
/// For the axum-served HTTP routes (e.g. `/catalog/sources`), which bypass the gRPC
/// interceptor. Returns the offending status code + message on failure.
pub fn verify_http_bearer(
    provider: &re_auth::RedapProvider,
    headers: &axum::http::HeaderMap,
) -> Result<(), (axum::http::StatusCode, &'static str)> {
    use axum::http::StatusCode;

    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            re_auth::ERROR_MESSAGE_MISSING_CREDENTIALS,
        ));
    };
    let token = value
        .to_str()
        .ok()
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or((
            StatusCode::UNAUTHORIZED,
            re_auth::ERROR_MESSAGE_MALFORMED_CREDENTIALS,
        ))?;
    let jwt = re_auth::Jwt::try_from(token.to_owned()).map_err(|_err| {
        (
            StatusCode::UNAUTHORIZED,
            re_auth::ERROR_MESSAGE_MALFORMED_CREDENTIALS,
        )
    })?;
    provider
        .verify(&jwt, re_auth::VerificationOptions::default())
        .map_err(|_err| (StatusCode::UNAUTHORIZED, "invalid token"))?;
    Ok(())
}
