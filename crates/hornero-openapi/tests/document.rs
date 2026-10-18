//! The document `hornero-openapi` infers from the route table, handler signatures, attributes and
//! guards.

use hornero::openapi::{
    ApiSecurity, IntoParams, OpenApiConfig, SecurityScheme, ToSchema, document, warnings,
};
use hornero::prelude::*;
use hornero::{BoxError, Key, Metadata, Resolver, RouteInfo};
use hornero_validation::Valid;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// --- the API -----------------------------------------------------------------------------------

/// A thing to do.
#[derive(Serialize, Deserialize, ToSchema)]
struct Todo {
    id: u64,
    /// What to do.
    title: String,
    done: bool,
    labels: Vec<Label>,
}

#[derive(Serialize, Deserialize, ToSchema)]
struct Label {
    name: String,
}

#[derive(Deserialize, ToSchema, garde::Validate)]
struct CreateTodo {
    #[garde(length(min = 1))]
    title: String,
}

#[allow(dead_code)] // only deserialized
#[derive(Deserialize, IntoParams)]
struct ListQuery {
    /// Only finished, or only open, todos.
    done: Option<bool>,
    limit: Option<u32>,
}

#[derive(Serialize, ToSchema)]
struct Page<T> {
    items: Vec<T>,
    total: u64,
}

struct Public;

#[derive(Injectable)]
struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

impl ApiSecurity for AuthGuard {
    fn scheme() -> (&'static str, SecurityScheme) {
        ("bearer", SecurityScheme::bearer_jwt())
    }

    fn required(route: &RouteInfo) -> bool {
        route.meta::<Public>().is_none()
    }
}

fn todo() -> Todo {
    Todo {
        id: 1,
        title: "write docs".into(),
        done: false,
        labels: Vec::new(),
    }
}

#[controller("/todos")]
#[guards(AuthGuard)]
struct TodoController;

#[routes]
impl TodoController {
    /// Lists todos.
    ///
    /// Newest first. Filter with `done`.
    #[get("/")]
    async fn list(&self, Query(_query): Query<ListQuery>) -> Json<Vec<Todo>> {
        Json(vec![todo()])
    }

    /// Finds one todo.
    #[get("/{id}")]
    #[api_response(404, description = "No todo has this id")]
    async fn find_one(&self, Path(_id): Path<u64>) -> Result<Json<Todo>, HttpError> {
        Ok(Json(todo()))
    }

    /// Creates a todo.
    #[post("/")]
    #[api_response(201)]
    async fn create(
        &self,
        Valid(Json(_input)): Valid<Json<CreateTodo>>,
    ) -> (StatusCode, Json<Todo>) {
        (StatusCode::CREATED, Json(todo()))
    }

    #[delete("/{id}")]
    #[api_response(204)]
    async fn remove(&self, Path(_id): Path<u64>) -> StatusCode {
        StatusCode::NO_CONTENT
    }

    /// Counts todos; no token needed.
    #[get("/stats")]
    #[meta(Public)]
    async fn stats(&self) -> String {
        "1".into()
    }

    #[get("/paged")]
    async fn paged(&self) -> Json<Page<Todo>> {
        Json(Page {
            items: vec![todo()],
            total: 1,
        })
    }
}

#[module(controllers = [TodoController], providers = [AuthGuard])]
struct TodoModule;

fn config() -> OpenApiConfig {
    OpenApiConfig::new("Todo API", "1.0")
}

fn json_of(openapi: &hornero::openapi::OpenApi) -> Value {
    serde_json::to_value(openapi).unwrap()
}

fn todo_document() -> Value {
    let routes = Hornero::new(TodoModule)
        .global_prefix("/api")
        .routes()
        .unwrap();
    assert_eq!(warnings(&routes, &config()), Vec::<String>::new());
    json_of(&document(&routes, &config()))
}

// --- inference ---------------------------------------------------------------------------------

#[test]
fn the_document_is_inferred_from_handler_signatures() {
    let routes = Hornero::new(TodoModule)
        .global_prefix("/api")
        .routes()
        .unwrap();
    // The served text: paths in route order, properties in field order.
    insta::assert_snapshot!(document(&routes, &config()).to_pretty_json().unwrap());
}

#[test]
fn doc_comments_become_summary_and_description() {
    let doc = todo_document();
    let list = &doc["paths"]["/api/todos"]["get"];
    assert_eq!(list["summary"], "Lists todos.");
    assert_eq!(list["description"], "Newest first. Filter with `done`.");
    assert_eq!(list["operationId"], "TodoController_list");
    assert_eq!(list["tags"], json!(["Todo"]));
    assert_eq!(
        doc["paths"]["/api/todos/{id}"]["get"]["description"],
        Value::Null
    );
}

#[test]
fn bodies_become_references_to_component_schemas() {
    let doc = todo_document();
    let list = &doc["paths"]["/api/todos"]["get"];
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"],
        json!({ "type": "array", "items": { "$ref": "#/components/schemas/Todo" } })
    );
    let create = &doc["paths"]["/api/todos"]["post"];
    assert_eq!(
        create["requestBody"]["content"]["application/json"]["schema"],
        json!({ "$ref": "#/components/schemas/CreateTodo" })
    );
    assert_eq!(create["requestBody"]["required"], true);

    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let names: Vec<&str> = schemas.keys().map(String::as_str).collect();
    assert_eq!(names, ["CreateTodo", "Label", "ProblemDetails", "Todo"]);
    assert_eq!(
        schemas["Todo"]["properties"]["title"]["description"],
        "What to do."
    );
}

#[test]
fn generic_bodies_are_inlined() {
    let doc = todo_document();
    let schema = &doc["paths"]["/api/todos/paged"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["total"]["type"], "integer");
}

#[test]
fn path_and_query_parameters_are_inferred() {
    let doc = todo_document();
    assert_eq!(
        doc["paths"]["/api/todos/{id}"]["get"]["parameters"],
        json!([{
            "name": "id",
            "in": "path",
            "required": true,
            "schema": { "type": "integer", "format": "int64", "minimum": 0 },
        }])
    );
    let query = &doc["paths"]["/api/todos"]["get"]["parameters"];
    assert_eq!(query[0]["name"], "done");
    assert_eq!(query[0]["in"], "query");
    assert_eq!(query[0]["required"], false);
    assert_eq!(
        query[0]["description"],
        "Only finished, or only open, todos."
    );
    assert_eq!(query[1]["name"], "limit");
}

#[test]
fn responses_follow_the_return_type_and_api_response() {
    let doc = todo_document();
    let responses = |path: &str, method: &str| doc["paths"][path][method]["responses"].clone();

    // `Result` adds a problem response for every other status; `#[api_response(404)]` documents one.
    let find_one = responses("/api/todos/{id}", "get");
    assert_eq!(find_one["200"]["description"], "OK");
    assert_eq!(find_one["404"]["description"], "No todo has this id");
    let problem = json!({ "application/problem+json": { "schema": { "$ref": "#/components/schemas/ProblemDetails" } } });
    assert_eq!(find_one["404"]["content"], problem);
    assert_eq!(find_one["default"]["content"], problem);

    // An explicit 2xx replaces the 200 and keeps the inferred body; `Valid` adds a 422.
    let create = responses("/api/todos", "post");
    assert_eq!(create["200"], Value::Null);
    assert_eq!(create["201"]["description"], "Created");
    assert_eq!(
        create["201"]["content"]["application/json"]["schema"],
        json!({ "$ref": "#/components/schemas/Todo" })
    );
    assert_eq!(create["422"]["content"], problem);

    // 204 has no body; a `String` is text.
    let remove = responses("/api/todos/{id}", "delete");
    assert_eq!(remove["204"], json!({ "description": "No Content" }));
    let stats = responses("/api/todos/stats", "get");
    assert_eq!(
        stats["200"]["content"]["text/plain"]["schema"]["type"],
        "string"
    );
}

#[test]
fn guards_implementing_api_security_secure_their_routes() {
    let doc = todo_document();
    assert_eq!(
        doc["paths"]["/api/todos"]["get"]["security"],
        json!([{ "bearer": [] }])
    );
    // `AuthGuard::required` exempts routes marked `Public`.
    assert_eq!(
        doc["paths"]["/api/todos/stats"]["get"]["security"],
        Value::Null
    );
    assert_eq!(
        doc["components"]["securitySchemes"]["bearer"],
        json!({ "type": "http", "scheme": "bearer", "bearerFormat": "JWT" })
    );
}

// --- more extractors ---------------------------------------------------------------------------

#[allow(dead_code)] // only deserialized
#[derive(Deserialize, ToSchema)]
struct Paging {
    page: u32,
    per_page: Option<u32>,
}

#[allow(dead_code)] // only deserialized
#[derive(Deserialize, ToSchema)]
struct FileRef {
    bucket: String,
    key: String,
}

#[allow(dead_code)] // only deserialized
#[derive(Deserialize, ToSchema)]
struct Upload {
    name: String,
}

#[controller("/files")]
struct FileController;

#[routes]
impl FileController {
    #[get("/{bucket}/{key}")]
    async fn by_tuple(&self, Path((_bucket, _key)): Path<(String, u32)>) -> &'static str {
        ""
    }

    #[get("/named/{bucket}/{key}")]
    async fn by_struct(&self, Path(_file): Path<FileRef>) -> &'static str {
        ""
    }

    #[get("/raw/{*rest}")]
    async fn raw(&self) -> &'static str {
        ""
    }

    #[get("/paged")]
    async fn paged(&self, Query(_paging): Query<Paging>) -> &'static str {
        ""
    }

    #[post("/upload")]
    #[api_body(Upload, content_type = "multipart/form-data")]
    #[api_response(200, body = Vec<String>, description = "The stored names")]
    #[api_response(413)]
    async fn upload(&self) -> StatusCode {
        StatusCode::OK
    }

    /// Superseded by `upload`.
    #[post("/legacy")]
    #[deprecated]
    async fn legacy(&self, Json(_body): Json<Option<Upload>>) -> StatusCode {
        StatusCode::OK
    }
}

#[module(controllers = [FileController])]
struct FileModule;

fn file_document() -> Value {
    let routes = Hornero::new(FileModule).routes().unwrap();
    assert_eq!(warnings(&routes, &config()), Vec::<String>::new());
    json_of(&document(&routes, &config()))
}

#[test]
fn tuple_and_struct_path_parameters_are_named_after_the_path() {
    let doc = file_document();
    let tuple = &doc["paths"]["/files/{bucket}/{key}"]["get"]["parameters"];
    assert_eq!(tuple[0]["name"], "bucket");
    assert_eq!(tuple[0]["schema"]["type"], "string");
    assert_eq!(tuple[1]["name"], "key");
    assert_eq!(tuple[1]["schema"]["type"], "integer");

    let named = &doc["paths"]["/files/named/{bucket}/{key}"]["get"]["parameters"];
    assert_eq!(named[0]["name"], "bucket");
    assert_eq!(named[1]["name"], "key");
    assert_eq!(named[1]["in"], "path");
    assert_eq!(named[1]["required"], true);

    // A wildcard is a plain parameter in OpenAPI; an unextracted one is a string.
    let raw = &doc["paths"]["/files/raw/{rest}"]["get"]["parameters"];
    assert_eq!(
        raw,
        &json!([{ "name": "rest", "in": "path", "required": true, "schema": { "type": "string" } }])
    );
}

#[test]
fn query_structs_without_into_params_use_their_schema() {
    let doc = file_document();
    let params = &doc["paths"]["/files/paged"]["get"]["parameters"];
    assert_eq!(params[0]["name"], "page");
    assert_eq!(params[0]["in"], "query");
    assert_eq!(params[0]["required"], true);
    assert_eq!(params[1]["name"], "per_page");
    assert_eq!(params[1]["required"], false);
}

#[test]
fn api_body_and_api_response_bodies_override_the_signature() {
    let doc = file_document();
    let upload = &doc["paths"]["/files/upload"]["post"];
    assert_eq!(
        upload["requestBody"]["content"]["multipart/form-data"]["schema"],
        json!({ "$ref": "#/components/schemas/Upload" })
    );
    assert_eq!(
        upload["responses"]["200"]["description"],
        "The stored names"
    );
    assert_eq!(
        upload["responses"]["200"]["content"]["application/json"]["schema"],
        json!({ "type": "array", "items": { "type": "string" } })
    );
    assert_eq!(
        upload["responses"]["413"]["description"],
        "Payload Too Large"
    );
}

#[test]
fn deprecated_handlers_are_deprecated_operations() {
    let doc = file_document();
    let legacy = &doc["paths"]["/files/legacy"]["post"];
    assert_eq!(legacy["deprecated"], true);
    assert_eq!(legacy["summary"], "Superseded by `upload`.");
    assert_eq!(
        legacy["requestBody"]["content"]["application/json"]["schema"],
        json!({ "oneOf": [{ "$ref": "#/components/schemas/Upload" }, { "type": "null" }] })
    );
}

// --- tags and exclusion ------------------------------------------------------------------------

#[controller("/admin")]
#[api_tags("Admin", "Internal")]
#[api_response(401, description = "Not signed in")]
struct AdminController;

#[routes]
impl AdminController {
    #[get("/users")]
    async fn users(&self) -> &'static str {
        ""
    }

    #[get("/health")]
    #[api_tags("Health")]
    #[api_response(401, description = "Health checks need a probe token")]
    async fn health(&self) -> &'static str {
        ""
    }

    #[get("/debug")]
    #[api_exclude]
    async fn debug(&self) -> &'static str {
        ""
    }
}

#[controller("/hidden")]
#[api_exclude]
struct HiddenController;

#[routes]
impl HiddenController {
    #[get("/")]
    async fn index(&self) -> &'static str {
        ""
    }
}

#[module(controllers = [AdminController, HiddenController])]
struct AdminModule;

#[test]
fn controller_attributes_apply_to_every_route() {
    let routes = Hornero::new(AdminModule).routes().unwrap();
    let openapi = document(&routes, &config().tag("Admin", "Back office"));
    // Paths keep the route table's order.
    let paths: Vec<&String> = openapi.paths.paths.keys().collect();
    assert_eq!(paths, ["/admin/users", "/admin/health"]);
    let doc = json_of(&openapi);
    assert_eq!(
        doc["paths"]["/admin/users"]["get"]["tags"],
        json!(["Admin", "Internal"])
    );
    assert_eq!(
        doc["paths"]["/admin/health"]["get"]["tags"],
        json!(["Health"])
    );
    // A controller's responses apply to each route, unless the route documents the same status.
    let unauthorized =
        |path: &str| doc["paths"][path]["get"]["responses"]["401"]["description"].clone();
    assert_eq!(unauthorized("/admin/users"), "Not signed in");
    assert_eq!(
        unauthorized("/admin/health"),
        "Health checks need a probe token"
    );
    assert_eq!(
        doc["paths"]["/admin/users"]["get"]["responses"]["200"]["description"],
        "OK"
    );
    // Declared tags come first, with their description, then the others in order of use.
    assert_eq!(
        doc["tags"],
        json!([{ "name": "Admin", "description": "Back office" }, { "name": "Internal" }, { "name": "Health" }])
    );
}

// --- security from global and hand-written guards ----------------------------------------------

#[controller("/ping")]
struct PingController;

#[routes]
impl PingController {
    #[get("/")]
    async fn ping(&self) -> &'static str {
        "pong"
    }
}

/// An API key guard written without macros.
struct ApiKeyGuard;

impl Injectable for ApiKeyGuard {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(ApiKeyGuard)
    }
    fn describe(meta: &mut Metadata) {
        hornero::openapi::describe_security::<Self>(meta);
    }
}

impl Guard for ApiKeyGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

impl ApiSecurity for ApiKeyGuard {
    fn scheme() -> (&'static str, SecurityScheme) {
        (
            "api_key",
            SecurityScheme::api_key_header("x-api-key").description("Issued per client"),
        )
    }
}

#[module(controllers = [PingController], providers = [AppGuard<AuthGuard>])]
struct SecuredModule;

#[test]
fn global_and_module_guards_secure_every_route_together() {
    let routes = Hornero::new(SecuredModule)
        .guard::<ApiKeyGuard>()
        .routes()
        .unwrap();
    let doc = json_of(&document(&routes, &config()));
    // Both guards run, so both schemes are required.
    assert_eq!(
        doc["paths"]["/ping"]["get"]["security"],
        json!([{ "api_key": [], "bearer": [] }])
    );
    assert_eq!(
        doc["components"]["securitySchemes"]["api_key"],
        json!({ "type": "apiKey", "in": "header", "name": "x-api-key", "description": "Issued per client" })
    );
}

// --- what cannot be inferred -------------------------------------------------------------------

#[derive(Deserialize)]
struct Opaque {}

#[derive(Deserialize)]
struct OpaqueQuery {}

#[controller("/opaque")]
struct OpaqueController;

#[routes]
impl OpaqueController {
    #[post("/")]
    async fn create(
        &self,
        Query(_query): Query<OpaqueQuery>,
        Json(_body): Json<Opaque>,
    ) -> &'static str {
        ""
    }
}

#[module(controllers = [OpaqueController])]
struct OpaqueModule;

#[test]
fn types_without_schemas_are_reported_not_rejected() {
    let routes = Hornero::new(OpaqueModule).routes().unwrap();
    assert_eq!(
        warnings(&routes, &config()),
        [
            "POST /opaque (OpaqueController::create): `OpaqueQuery` implements neither `utoipa::IntoParams` nor `utoipa::ToSchema`, so its parameters are not documented; derive one of them",
            "POST /opaque (OpaqueController::create): `Opaque` does not implement `utoipa::ToSchema`, so it is documented as any value; add `#[derive(ToSchema)]` to it",
        ]
    );
    let doc = json_of(&document(&routes, &config()));
    assert_eq!(
        doc["paths"]["/opaque"]["post"]["requestBody"]["content"]["application/json"]["schema"],
        json!({})
    );
}

#[test]
fn hand_written_controllers_are_documented_from_the_route_table_alone() {
    struct Hand;
    impl Injectable for Hand {
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Hand)
        }
    }
    impl Controller for Hand {
        fn routes(c: &mut hornero::ControllerDef<Self>) {
            c.get("/items/{id}", "find", |_: Arc<Self>| async { "" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Hand>();
        }
    }
    let routes = Hornero::new(App).routes().unwrap();
    let doc = json_of(&document(&routes, &config()));
    assert_eq!(
        doc["paths"]["/items/{id}"]["get"],
        json!({
            "tags": ["Hand"],
            "operationId": "Hand_find",
            "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }],
            "responses": { "200": { "description": "OK" } },
        })
    );
}

// --- configuration -----------------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
struct Event {
    name: String,
}

#[test]
fn the_config_describes_the_api_and_can_edit_the_document() {
    let routes = Hornero::new(TodoModule).routes().unwrap();
    let config = config()
        .description("Manages todos.")
        .server("https://api.example.com")
        .schema::<Event>()
        .filter(|route| route.handler() != "stats")
        .modify(|openapi| openapi.info.title.push_str(" (beta)"));
    let doc = json_of(&document(&routes, &config));
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        doc["info"],
        json!({ "title": "Todo API (beta)", "version": "1.0", "description": "Manages todos." })
    );
    assert_eq!(
        doc["servers"],
        json!([{ "url": "https://api.example.com" }])
    );
    assert!(doc["components"]["schemas"]["Event"].is_object());
    assert_eq!(doc["paths"]["/todos/stats"], Value::Null);
}
