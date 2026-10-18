use std::sync::Arc;

use hornero_core::RouteInfo;
use utoipa::ToSchema;
use utoipa::openapi::{OpenApi, Server, Tag};

use crate::__private::Components;

type Filter = Arc<dyn Fn(&RouteInfo) -> bool + Send + Sync>;
type Modifier = Arc<dyn Fn(&mut OpenApi) + Send + Sync>;

/// What the document says about the API as a whole, and where it is served: Nest's
/// `DocumentBuilder` plus the path given to `SwaggerModule.setup`.
///
/// ```rust,ignore
/// OpenApiConfig::new("Todo API", "1.0")
///     .description("Manages todos.")
///     .path("/docs")              // the default
///     .server("https://api.example.com")
///     .tag("Todo", "Creating and finishing todos")
/// ```
#[derive(Clone)]
pub struct OpenApiConfig {
    pub(crate) title: String,
    pub(crate) version: String,
    pub(crate) description: Option<String>,
    pub(crate) path: String,
    pub(crate) servers: Vec<Server>,
    pub(crate) tags: Vec<Tag>,
    pub(crate) schemas: Vec<fn(&mut Components)>,
    pub(crate) filter: Option<Filter>,
    pub(crate) modifiers: Vec<Modifier>,
}

impl OpenApiConfig {
    /// A document titled `title` for version `version` of the API, served under `/docs`.
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: None,
            path: "/docs".to_owned(),
            servers: Vec::new(),
            tags: Vec::new(),
            schemas: Vec::new(),
            filter: None,
            modifiers: Vec::new(),
        }
    }

    /// The API's description; Markdown is allowed.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Where `OpenApiModule` serves Swagger UI, `/docs` by default. The document is at
    /// `{path}/openapi.json`. Both live under `Hornero::global_prefix`.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        let trimmed = path.trim_matches('/');
        self.path = if trimmed.is_empty() {
            String::new()
        } else {
            format!("/{trimmed}")
        };
        self
    }

    /// A server the API is reachable at, such as `https://api.example.com`. Without one,
    /// clients use the host serving the document.
    pub fn server(mut self, url: impl Into<String>) -> Self {
        self.servers.push(Server::new(url));
        self
    }

    /// Describes a tag. Tags are listed in the order they are declared here, then in the order
    /// routes first use them.
    pub fn tag(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        let mut tag = Tag::new(name.into());
        tag.description = Some(description.into());
        self.tags.push(tag);
        self
    }

    /// Adds `T`'s schema, and the schemas it refers to, to `components.schemas` even though no
    /// handler signature mentions it, for example a type only a custom extractor reads.
    pub fn schema<T: ToSchema>(mut self) -> Self {
        self.schemas.push(|components| {
            components.push((T::name().into_owned(), T::schema()));
            T::schemas(components);
        });
        self
    }

    /// Documents only the routes for which `filter` returns `true`, for example to split public
    /// and admin APIs into two documents served by two `OpenApiModule`s.
    pub fn filter(mut self, filter: impl Fn(&RouteInfo) -> bool + Send + Sync + 'static) -> Self {
        self.filter = Some(Arc::new(filter));
        self
    }

    /// Edits the finished document, the escape hatch for anything hornero does not infer.
    /// Modifiers run in the order they were added.
    pub fn modify(mut self, modify: impl Fn(&mut OpenApi) + Send + Sync + 'static) -> Self {
        self.modifiers.push(Arc::new(modify));
        self
    }

    pub(crate) fn includes(&self, route: &RouteInfo) -> bool {
        self.filter.as_ref().is_none_or(|filter| filter(route))
    }
}
