use hornero_core::{Metadata, RouteInfo};
use utoipa::openapi::security::{self, ApiKey, ApiKeyValue, Http, HttpAuthScheme};

/// A guard that authenticates requests, documented as an OpenAPI security scheme: Nest's
/// `@ApiBearerAuth()` and friends, derived from the guard instead of repeated on every route.
///
/// Every route the guard runs on (global, module-wide through `AppGuard`, controller or route)
/// requires the scheme, unless [`required`](ApiSecurity::required) says otherwise. Several such
/// guards on one route are all required.
///
/// ```rust,ignore
/// impl ApiSecurity for AuthGuard {
///     fn scheme() -> (&'static str, SecurityScheme) {
///         ("bearer", SecurityScheme::bearer_jwt())
///     }
///
///     // Routes marked `#[meta(Public)]` skip authentication.
///     fn required(route: &RouteInfo) -> bool {
///         route.meta::<Public>().is_none()
///     }
/// }
/// ```
///
/// With hornero's `openapi` feature, `#[derive(Injectable)]` (and `#[injectable]`) record the scheme
/// on their own. A hand-written `Injectable` impl calls [`describe_security`] from
/// [`Injectable::describe`](hornero_core::Injectable::describe).
pub trait ApiSecurity {
    /// The scheme's name in `components.securitySchemes`, and the scheme. Guards returning the
    /// same name share one scheme.
    fn scheme() -> (&'static str, SecurityScheme);

    /// Whether `route` requires the scheme. Every route by default.
    fn required(_route: &RouteInfo) -> bool {
        true
    }

    /// The OAuth2 or OpenID Connect scopes `route` requires. None by default.
    fn scopes(_route: &RouteInfo) -> Vec<String> {
        Vec::new()
    }
}

/// An OpenAPI security scheme, with shortcuts for the common ones. Any utoipa
/// [`SecurityScheme`](utoipa::openapi::security::SecurityScheme), such as OAuth2 flows, converts
/// into it.
#[derive(Clone, PartialEq)]
pub struct SecurityScheme(security::SecurityScheme);

impl SecurityScheme {
    /// `Authorization: Bearer <token>`.
    pub fn bearer() -> Self {
        Self(security::SecurityScheme::Http(Http::new(
            HttpAuthScheme::Bearer,
        )))
    }

    /// `Authorization: Bearer <token>` where the token is a JWT.
    pub fn bearer_jwt() -> Self {
        Self::bearer().bearer_format("JWT")
    }

    /// `Authorization: Basic <credentials>`.
    pub fn basic() -> Self {
        Self(security::SecurityScheme::Http(Http::new(
            HttpAuthScheme::Basic,
        )))
    }

    /// An API key in the header `name`.
    pub fn api_key_header(name: impl Into<String>) -> Self {
        Self(security::SecurityScheme::ApiKey(ApiKey::Header(
            ApiKeyValue::new(name),
        )))
    }

    /// An API key in the query parameter `name`.
    pub fn api_key_query(name: impl Into<String>) -> Self {
        Self(security::SecurityScheme::ApiKey(ApiKey::Query(
            ApiKeyValue::new(name),
        )))
    }

    /// An API key in the cookie `name`.
    pub fn api_key_cookie(name: impl Into<String>) -> Self {
        Self(security::SecurityScheme::ApiKey(ApiKey::Cookie(
            ApiKeyValue::new(name),
        )))
    }

    /// A hint at the bearer token's format, such as `JWT`. Ignored by other schemes.
    pub fn bearer_format(mut self, format: impl Into<String>) -> Self {
        if let security::SecurityScheme::Http(http) = &mut self.0 {
            http.bearer_format = Some(format.into());
        }
        self
    }

    /// A description of the scheme, shown by Swagger UI's authorize dialog.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        let description = Some(description.into());
        match &mut self.0 {
            security::SecurityScheme::Http(http) => http.description = description,
            security::SecurityScheme::ApiKey(
                ApiKey::Header(value) | ApiKey::Query(value) | ApiKey::Cookie(value),
            ) => value.description = description,
            security::SecurityScheme::OAuth2(oauth2) => oauth2.description = description,
            security::SecurityScheme::OpenIdConnect(oidc) => oidc.description = description,
            security::SecurityScheme::MutualTls { description: d, .. } => *d = description,
        }
        self
    }

    /// The utoipa scheme.
    pub fn into_inner(self) -> security::SecurityScheme {
        self.0
    }
}

impl From<security::SecurityScheme> for SecurityScheme {
    fn from(scheme: security::SecurityScheme) -> Self {
        Self(scheme)
    }
}

/// What [`describe_security`] stores in an enhancer's metadata.
#[derive(Clone, Copy)]
pub(crate) struct SecurityDoc {
    pub(crate) scheme: fn() -> (&'static str, SecurityScheme),
    pub(crate) required: fn(&RouteInfo) -> bool,
    pub(crate) scopes: fn(&RouteInfo) -> Vec<String>,
}

/// Records `G`'s [`ApiSecurity`] scheme in the metadata a guard describes itself with, for a
/// hand-written [`Injectable::describe`](hornero_core::Injectable::describe):
///
/// ```rust,ignore
/// impl Injectable for AuthGuard {
///     // dependencies, construct ...
///     fn describe(meta: &mut Metadata) {
///         hornero::openapi::describe_security::<Self>(meta);
///     }
/// }
/// ```
pub fn describe_security<G: ApiSecurity>(meta: &mut Metadata) {
    meta.insert(SecurityDoc {
        scheme: G::scheme,
        required: G::required,
        scopes: G::scopes,
    });
}
