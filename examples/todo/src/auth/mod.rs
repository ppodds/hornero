//! Authentication: guards are exported providers, so feature modules use them without knowing
//! about `TokenStore` or `AuthConfig`.

use std::collections::HashMap;

use hornero::axum::extract::FromRequestParts;
use hornero::axum::http::request::Parts;
use hornero::openapi::{ApiSecurity, SecurityScheme};
use hornero::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AuthConfig {
    pub tokens: HashMap<String, TokenEntry>,
}

#[derive(Deserialize, Clone)]
pub struct TokenEntry {
    pub user: String,
    pub roles: Vec<String>,
}

/// The authenticated caller. Taking it as a handler argument is proof the request is
/// authenticated: there is no way to get one without passing [`AuthGuard`].
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub name: String,
    pub roles: Vec<String>,
}

impl AuthUser {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or_else(|| HttpError::unauthorized("authentication required"))
    }
}

/// Route metadata read by [`RolesGuard`]: `#[meta(Roles(&["admin"]))]`.
pub struct Roles(pub &'static [&'static str]);

#[derive(Injectable)]
pub struct TokenStore {
    config: Arc<AuthConfig>,
}

impl TokenStore {
    pub fn verify(&self, token: &str) -> Option<AuthUser> {
        self.config.tokens.get(token).map(|entry| AuthUser {
            name: entry.user.clone(),
            roles: entry.roles.clone(),
        })
    }
}

#[derive(Injectable)]
pub struct AuthGuard {
    tokens: Arc<TokenStore>,
}

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let token = ctx
            .parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| HttpError::unauthorized("missing bearer token"))?;
        let user = self
            .tokens
            .verify(token)
            .ok_or_else(|| HttpError::unauthorized("invalid token"))?;
        ctx.parts.extensions.insert(user);
        Ok(())
    }
}

/// Every route `AuthGuard` runs on needs a bearer token, and the OpenAPI document says so:
/// Swagger UI shows a lock on them and an "Authorize" button for the token.
impl ApiSecurity for AuthGuard {
    fn scheme() -> (&'static str, SecurityScheme) {
        (
            "bearer",
            SecurityScheme::bearer().description("A token from the `auth.tokens` configuration"),
        )
    }
}

#[derive(Injectable)]
pub struct RolesGuard;

impl Guard for RolesGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let Some(Roles(required)) = ctx.route.meta::<Roles>() else {
            return Ok(());
        };
        let user = ctx
            .parts
            .extensions
            .get::<AuthUser>()
            .ok_or_else(|| HttpError::unauthorized("authentication required"))?;
        if required.iter().any(|role| user.has_role(role)) {
            Ok(())
        } else {
            Err(HttpError::forbidden(format!(
                "requires one of the roles: {}",
                required.join(", ")
            )))
        }
    }
}

#[module(providers = [TokenStore, AuthGuard, RolesGuard], exports = [AuthGuard, RolesGuard])]
pub struct AuthModule;
