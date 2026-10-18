use std::collections::BTreeMap;

use axum::http::{Method, StatusCode};
use hornero_core::{RouteInfo, Routes};
use serde_json::Value;
use utoipa::openapi::path::{HttpMethod, Operation, Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::request_body::RequestBodyBuilder;
use utoipa::openapi::response::{Response, ResponseBuilder};
use utoipa::openapi::schema::{ObjectBuilder, Schema, SchemaFormat, SchemaType, Type};
use utoipa::openapi::security::SecurityRequirement;
use utoipa::openapi::{
    Components, Content, Deprecated, Info, OpenApi, Paths, Ref, RefOr, Required, Tag,
};

use crate::__private::{BodyDoc, ControllerDoc, OperationDoc, RouteDoc, SchemaDoc};
use crate::config::OpenApiConfig;
use crate::security::{SecurityDoc, SecurityScheme};

/// The component name of hornero's error body.
const PROBLEM: &str = "ProblemDetails";
const PROBLEM_JSON: &str = "application/problem+json";

/// Builds the OpenAPI document for `routes`, the route table from [`Hornero::routes`] (before
/// starting) or [`App::routes`] (after), without serving it: for writing `openapi.json` in a
/// build step or checking it in a test.
///
/// What could not be inferred, such as a body type that does not implement `ToSchema`, is
/// logged as a `tracing` warning; [`warnings`] returns the same list.
///
/// [`Hornero::routes`]: hornero_core::Hornero::routes
/// [`App::routes`]: hornero_core::App::routes
pub fn document(routes: &Routes, config: &OpenApiConfig) -> OpenApi {
    let (openapi, warnings) = build(routes, config);
    for warning in warnings {
        tracing::warn!(target: "hornero_openapi", "{warning}");
    }
    openapi
}

/// What [`document`] could not infer from `routes`, one message per problem, each naming the
/// route. Empty when every route is fully documented, which makes a useful test assertion.
pub fn warnings(routes: &Routes, config: &OpenApiConfig) -> Vec<String> {
    build(routes, config).1
}

fn build(routes: &Routes, config: &OpenApiConfig) -> (OpenApi, Vec<String>) {
    let mut builder = Builder::default();
    for route in routes {
        if config.includes(route) {
            builder.route(route);
        }
    }
    builder.finish(config)
}

#[derive(Default)]
struct Builder {
    paths: Paths,
    schemas: BTreeMap<String, RefOr<Schema>>,
    security_schemes: BTreeMap<String, SecurityScheme>,
    /// Tags in the order routes first use them.
    tags: Vec<String>,
    problem: bool,
    warnings: Vec<String>,
}

impl Builder {
    fn route(&mut self, route: &RouteInfo) {
        let controller = route.meta::<ControllerDoc>().copied();
        let mut doc = route
            .meta::<RouteDoc>()
            .map(RouteDoc::operation)
            .unwrap_or_default();
        if doc.excluded || controller.is_some_and(|controller| controller.excluded) {
            return;
        }
        if let Some(shared) = controller.and_then(|controller| controller.responses) {
            let mut controller_doc = OperationDoc::default();
            shared(&mut controller_doc);
            for response in controller_doc.responses {
                if !doc
                    .responses
                    .iter()
                    .any(|own| own.status == response.status)
                {
                    doc.responses.push(response);
                }
            }
        }
        let label = format!(
            "{} {} ({}::{})",
            route.method(),
            route.path(),
            route.controller(),
            route.handler()
        );
        let Some(method) = http_method(route.method()) else {
            self.warn(
                &label,
                "OpenAPI cannot describe a custom HTTP method, so the route is left out",
            );
            return;
        };

        let mut operation = Operation::new();
        operation.operation_id = Some(format!("{}_{}", route.controller(), route.handler()));
        let tags = doc
            .tags
            .clone()
            .or_else(|| {
                controller
                    .and_then(|c| c.tags)
                    .map(|tags| tags.iter().map(|t| (*t).to_owned()).collect())
            })
            .unwrap_or_else(|| vec![default_tag(route.controller())]);
        for tag in &tags {
            if !self.tags.contains(tag) {
                self.tags.push(tag.clone());
            }
        }
        operation.tags = Some(tags);
        operation.summary = doc.summary.clone();
        operation.description = doc.description.clone();
        if doc.deprecated {
            operation.deprecated = Some(Deprecated::True);
        }

        let (template, names) = path_template(route.path());
        let parameters = self.parameters(&label, &names, &doc);
        if !parameters.is_empty() {
            operation.parameters = Some(parameters.into_iter().map(RefOr::T).collect());
        }
        let OperationDoc {
            body,
            returns,
            responses,
            fallible,
            validated,
            ..
        } = doc;
        if let Some(body) = body {
            let required = if body.required {
                Required::True
            } else {
                Required::False
            };
            let content = self.content(&label, body);
            operation.request_body = Some(RefOr::T(
                RequestBodyBuilder::new()
                    .content(content.0, content.1)
                    .required(Some(required))
                    .build(),
            ));
        }

        let inferred = returns.map(|body| self.content(&label, body));
        let explicit_success = responses
            .iter()
            .any(|response| (200..300).contains(&response.status));
        if !explicit_success {
            operation
                .responses
                .responses
                .insert("200".into(), response("OK", inferred.clone()));
        }
        for explicit in responses {
            let status = explicit.status;
            let content = match explicit.body {
                Some(body) => Some(self.content(&label, body)),
                None if (200..300).contains(&status) && status != 204 => inferred.clone(),
                None if status >= 400 => Some(self.problem()),
                None => None,
            };
            let description = explicit.description.unwrap_or_else(|| reason(status));
            operation
                .responses
                .responses
                .insert(status.to_string(), response(&description, content));
        }
        if validated && !operation.responses.responses.contains_key("422") {
            let content = Some(self.problem());
            operation.responses.responses.insert(
                "422".into(),
                response("The request failed validation", content),
            );
        }
        if fallible {
            let content = Some(self.problem());
            operation
                .responses
                .responses
                .entry("default".into())
                .or_insert_with(|| response("Error", content));
        }

        operation.security = self.security(route);
        self.paths
            .add_path_operation(template, vec![method], operation);
    }

    /// Path parameters in path order, then query parameters in handler order. Path parameters the
    /// handler does not extract, or extracts without a schema, are documented as strings.
    fn parameters(&mut self, label: &str, names: &[String], doc: &OperationDoc) -> Vec<Parameter> {
        let mut path: Vec<Option<Parameter>> = vec![None; names.len()];
        let mut query = Vec::new();

        if !doc.path.is_empty() && doc.path.len() != names.len() {
            self.warn(
                label,
                &format!(
                    "the handler extracts {} path parameter(s), but the path has {}",
                    doc.path.len(),
                    names.len()
                ),
            );
        }
        for (slot, (name, schema)) in path
            .iter_mut()
            .zip(names.iter().zip(doc.path.iter().cloned()))
        {
            let schema = self.absorb(label, schema);
            *slot = Some(path_parameter(name, schema));
        }

        for params in &doc.params {
            self.register_all(label, &params.components, &params.warnings);
            for param in &params.params {
                match param.parameter_in {
                    ParameterIn::Path => match names.iter().position(|name| *name == param.name) {
                        Some(index) => path[index] = Some(param.clone()),
                        None => self.warn(
                            label,
                            &format!(
                                "path parameter `{}` does not appear in the path",
                                param.name
                            ),
                        ),
                    },
                    _ => query.push(param.clone()),
                }
            }
        }

        let string = || -> RefOr<Schema> { ObjectBuilder::new().schema_type(Type::String).into() };
        path.into_iter()
            .zip(names)
            .map(|(param, name)| param.unwrap_or_else(|| path_parameter(name, string())))
            .chain(query)
            .collect()
    }

    fn content(&mut self, label: &str, body: BodyDoc) -> (String, Content) {
        let schema = self.absorb(label, body.schema);
        (body.content_type, Content::new(Some(schema)))
    }

    fn problem(&mut self) -> (String, Content) {
        self.problem = true;
        (
            PROBLEM_JSON.to_owned(),
            Content::new(Some(Ref::from_schema_name(PROBLEM))),
        )
    }

    /// The security requirement of every guard on `route` that describes a scheme and requires
    /// it there: all of them must be met.
    fn security(&mut self, route: &RouteInfo) -> Option<Vec<SecurityRequirement>> {
        let mut requirement: Option<SecurityRequirement> = None;
        let mut names: Vec<&str> = Vec::new();
        for guard in route.guards() {
            let Some(security) = guard.meta::<SecurityDoc>() else {
                continue;
            };
            if !(security.required)(route) {
                continue;
            }
            let (name, scheme) = (security.scheme)();
            match self.security_schemes.get(name) {
                None => {
                    self.security_schemes.insert(name.to_owned(), scheme);
                }
                Some(existing) if *existing == scheme => {}
                Some(_) => {
                    let message = format!(
                        "guard `{}` describes a security scheme named `{name}` that differs from the one already registered under that name; the first one is kept",
                        guard.name()
                    );
                    self.push_warning(message);
                }
            }
            if names.contains(&name) {
                continue;
            }
            names.push(name);
            let scopes = (security.scopes)(route);
            requirement = Some(match requirement {
                None => SecurityRequirement::new(name, scopes),
                Some(requirement) => requirement.add(name, scopes),
            });
        }
        requirement.map(|requirement| vec![requirement])
    }

    fn absorb(&mut self, label: &str, doc: SchemaDoc) -> RefOr<Schema> {
        self.register_all(label, &doc.components, &doc.warnings);
        doc.schema
    }

    fn register_all(
        &mut self,
        label: &str,
        components: &[(String, RefOr<Schema>)],
        warnings: &[String],
    ) {
        for (name, schema) in components {
            self.register(name, schema);
        }
        for warning in warnings {
            self.warn(label, warning);
        }
    }

    fn register(&mut self, name: &str, schema: &RefOr<Schema>) {
        match self.schemas.get(name) {
            None => {
                self.schemas.insert(name.to_owned(), schema.clone());
            }
            Some(existing) if existing == schema => {}
            Some(_) => self.push_warning(format!(
                "two different schemas are named `{name}`; the first one is kept. Rename one with `#[schema(as = ...)]`"
            )),
        }
    }

    fn warn(&mut self, label: &str, message: &str) {
        self.push_warning(format!("{label}: {message}"));
    }

    fn push_warning(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    fn finish(mut self, config: &OpenApiConfig) -> (OpenApi, Vec<String>) {
        for add in &config.schemas {
            let mut components = Vec::new();
            add(&mut components);
            for (name, schema) in &components {
                self.register(name, schema);
            }
        }
        if self.problem {
            self.register(PROBLEM, &problem_schema());
        }

        let mut info = Info::new(config.title.clone(), config.version.clone());
        info.description = config.description.clone();
        let mut openapi = OpenApi::new(info, std::mem::take(&mut self.paths));
        if !config.servers.is_empty() {
            openapi.servers = Some(config.servers.clone());
        }

        let mut tags = config.tags.clone();
        for name in &self.tags {
            if !tags.iter().any(|tag| tag.name == *name) {
                tags.push(Tag::new(name));
            }
        }
        if !tags.is_empty() {
            openapi.tags = Some(tags);
        }

        if !self.schemas.is_empty() || !self.security_schemes.is_empty() {
            let mut components = Components::new();
            components.schemas = std::mem::take(&mut self.schemas);
            components.security_schemes = std::mem::take(&mut self.security_schemes)
                .into_iter()
                .map(|(name, scheme)| (name, RefOr::T(scheme.into_inner())))
                .collect();
            openapi.components = Some(components);
        }

        for modify in &config.modifiers {
            modify(&mut openapi);
        }
        self.check_references(&openapi);
        (openapi, self.warnings)
    }

    /// Warns about `$ref`s to schemas that are not in `components.schemas`, which happens when a
    /// parameter type from `IntoParams` refers to a type no handler mentions.
    fn check_references(&mut self, openapi: &OpenApi) {
        let Ok(value) = serde_json::to_value(openapi) else {
            return;
        };
        let defined = &value["components"]["schemas"];
        let mut missing = Vec::new();
        collect_references(&value, &mut |reference| {
            if let Some(name) = reference.strip_prefix("#/components/schemas/")
                && defined.get(name).is_none()
                && !missing.iter().any(|m| m == name)
            {
                missing.push(name.to_owned());
            }
        });
        for name in missing {
            self.push_warning(format!(
                "the schema `{name}` is referenced but not defined; add it with `OpenApiConfig::schema::<{name}>()`"
            ));
        }
    }
}

fn collect_references(value: &Value, found: &mut impl FnMut(&str)) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("$ref", Value::String(reference)) => found(reference),
                    _ => collect_references(value, found),
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_references(item, found)),
        _ => {}
    }
}

fn response(description: &str, content: Option<(String, Content)>) -> RefOr<Response> {
    let builder = ResponseBuilder::new().description(description);
    match content {
        Some((content_type, content)) => builder.content(content_type, content),
        None => builder,
    }
    .build()
    .into()
}

fn path_parameter(name: &str, schema: RefOr<Schema>) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .schema(Some(schema))
        .build()
}

fn http_method(method: &Method) -> Option<HttpMethod> {
    Some(match *method {
        Method::GET => HttpMethod::Get,
        Method::POST => HttpMethod::Post,
        Method::PUT => HttpMethod::Put,
        Method::DELETE => HttpMethod::Delete,
        Method::PATCH => HttpMethod::Patch,
        Method::HEAD => HttpMethod::Head,
        Method::OPTIONS => HttpMethod::Options,
        Method::TRACE => HttpMethod::Trace,
        _ => return None,
    })
}

/// `TodoController` is tagged `Todo`.
fn default_tag(controller: &str) -> String {
    match controller.strip_suffix("Controller") {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => controller.to_owned(),
    }
}

fn reason(status: u16) -> String {
    StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Response")
        .to_owned()
}

/// Turns an axum path into an OpenAPI path template and its parameter names:
/// `/files/{*rest}` becomes `/files/{rest}` with `["rest"]`.
fn path_template(path: &str) -> (String, Vec<String>) {
    let mut template = String::with_capacity(path.len());
    let mut names = Vec::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                template.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                template.push('}');
            }
            '{' => {
                let mut name: String = chars.by_ref().take_while(|c| *c != '}').collect();
                if let Some(stripped) = name.strip_prefix('*') {
                    name = stripped.to_owned();
                }
                template.push('{');
                template.push_str(&name);
                template.push('}');
                names.push(name);
            }
            c => template.push(c),
        }
    }
    (template, names)
}

/// The body of every hornero error response, as `HttpError::to_problem` renders it.
fn problem_schema() -> RefOr<Schema> {
    let string = |description: &str| {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some(description))
    };
    ObjectBuilder::new()
        .schema_type(Type::Object)
        .description(Some("An RFC 9457 problem details object: the body of every error response."))
        .property("type", string("A URI identifying the problem type; `about:blank` when the status code says it all."))
        .property("title", string("The status code's reason phrase."))
        .property(
            "status",
            ObjectBuilder::new()
                .schema_type(Type::Integer)
                .format(Some(SchemaFormat::Custom("int32".into())))
                .description(Some("The HTTP status code.")),
        )
        .property("detail", string("What went wrong in this occurrence."))
        .property(
            "errors",
            ObjectBuilder::new()
                .schema_type(SchemaType::AnyValue)
                .description(Some("Details, if any; for a validation failure, a list of `{ path, message }` objects.")),
        )
        .required("type")
        .required("title")
        .required("status")
        .required("detail")
        .into()
}

#[cfg(test)]
mod tests {
    use super::{default_tag, path_template};

    #[test]
    fn path_templates_drop_the_wildcard_marker() {
        assert_eq!(
            path_template("/todos/{id}"),
            ("/todos/{id}".to_owned(), vec!["id".to_owned()])
        );
        assert_eq!(
            path_template("/files/{dir}/{*rest}"),
            (
                "/files/{dir}/{rest}".to_owned(),
                vec!["dir".to_owned(), "rest".to_owned()]
            )
        );
    }

    #[test]
    fn controllers_are_tagged_by_their_name() {
        assert_eq!(default_tag("TodoController"), "Todo");
        assert_eq!(default_tag("Controller"), "Controller");
        assert_eq!(default_tag("Health"), "Health");
    }
}
