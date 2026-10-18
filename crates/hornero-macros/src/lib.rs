//! Procedural macros for hornero. Every macro expands to the public builder API of `hornero-core`,
//! so anything written with them can also be written by hand.

#![warn(missing_docs)]

mod controller;
mod enhancer;
mod injectable;
mod module;
mod openapi;
mod routes;
mod util;

use proc_macro::TokenStream;

/// Implements `Injectable` for a struct. Every `Arc<T>` field is a dependency.
///
/// ```ignore
/// #[derive(Injectable)]
/// #[inject(scope = request, hooks)]        // optional
/// struct TodoService {
///     repo: Arc<dyn TodoRepo>,
///     #[inject(default)]
///     cache: Mutex<HashMap<i64, Todo>>,
///     #[inject(with = Instant::now())]
///     started: Instant,
/// }
/// ```
#[proc_macro_derive(Injectable, attributes(inject))]
pub fn derive_injectable(input: TokenStream) -> TokenStream {
    injectable::derive(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// On an `impl` block: constructor injection through its `new` function (or the one marked
/// `#[constructor]`), which may be `async` and may return `Result<Self, E>`.
/// On a struct: shorthand for `#[derive(Injectable)]` with options.
///
/// ```ignore
/// #[injectable(scope = request)]
/// impl AuditLog {
///     async fn new(ctx: Arc<RequestContext>, db: Arc<Db>) -> Result<Self, DbErr> { ... }
/// }
/// ```
#[proc_macro_attribute]
pub fn injectable(args: TokenStream, item: TokenStream) -> TokenStream {
    injectable::attribute(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declares a module.
///
/// ```ignore
/// #[module(
///     imports = [AuthModule, ConfigModule::for_root(source)],
///     controllers = [TodoController],
///     providers = [TodoService, bind!(dyn TodoRepo => SeaOrmTodoRepo), value(Clock::system())],
///     exports = [TodoService, module(AuthModule)],
///     middlewares = [RequestIdMiddleware],
///     global,
/// )]
/// pub struct TodoModule;
/// ```
#[proc_macro_attribute]
pub fn module(args: TokenStream, item: TokenStream) -> TokenStream {
    module::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declares a controller struct: injectable, with a route prefix and controller-level enhancers.
///
/// Enhancers are stacked below it, one attribute per line like Nest's decorators:
///
/// ```ignore
/// #[controller("/todos")]
/// #[guards(AuthGuard)]
/// #[interceptors(Logging)]
/// #[meta(Public)]
/// pub struct TodoController { todos: Arc<TodoService> }
/// ```
///
/// They can also be given as arguments: `#[controller("/todos", guards = [AuthGuard])]`.
#[proc_macro_attribute]
pub fn controller(args: TokenStream, item: TokenStream) -> TokenStream {
    controller::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Guards, Nest's `@UseGuards`: below `#[controller(...)]` for every route of the controller, or
/// on a route method inside `#[routes]`. Listed guards run in order.
///
/// `#[controller]` and `#[routes]` read this attribute; on its own it only reports where it belongs.
#[proc_macro_attribute]
pub fn guards(_args: TokenStream, item: TokenStream) -> TokenStream {
    enhancer::misplaced("guards", item.into()).into()
}

/// Interceptors, Nest's `@UseInterceptors`: below `#[controller(...)]` or on a route method inside
/// `#[routes]`. The first listed is the outermost.
///
/// `#[controller]` and `#[routes]` read this attribute; on its own it only reports where it belongs.
#[proc_macro_attribute]
pub fn interceptors(_args: TokenStream, item: TokenStream) -> TokenStream {
    enhancer::misplaced("interceptors", item.into()).into()
}

/// Exception filters, Nest's `@UseFilters`: below `#[controller(...)]` or on a route method inside
/// `#[routes]`. The first listed is tried first.
///
/// `#[controller]` and `#[routes]` read this attribute; on its own it only reports where it belongs.
#[proc_macro_attribute]
pub fn filters(_args: TokenStream, item: TokenStream) -> TokenStream {
    enhancer::misplaced("filters", item.into()).into()
}

/// One route metadata value, Nest's `@SetMetadata` or a custom decorator like `@Roles('admin')`:
/// below `#[controller(...)]` or on a route method inside `#[routes]`.
///
/// `#[controller]` and `#[routes]` read this attribute; on its own it only reports where it belongs.
#[proc_macro_attribute]
pub fn meta(_args: TokenStream, item: TokenStream) -> TokenStream {
    enhancer::misplaced("meta", item.into()).into()
}

/// A documented response, Nest's `@ApiResponse`: on a route method inside `#[routes]`, or below
/// `#[controller(...)]` for every route of the controller (a route's own response with the same
/// status wins). Read with hornero's `openapi` feature.
///
/// ```ignore
/// #[post("/")]
/// #[api_response(201, description = "The created todo")]   // the inferred body, under 201
/// #[api_response(409, description = "The title is taken")]  // a problem+json body
/// #[api_response(200, body = Vec<Todo>, content_type = "application/json")]
/// ```
///
/// Without `body`, a 2xx response (except 204) carries the body inferred from the return type
/// and a 4xx or 5xx one the problem details schema. Declaring any 2xx replaces the inferred 200.
#[proc_macro_attribute]
pub fn api_response(_args: TokenStream, item: TokenStream) -> TokenStream {
    openapi::misplaced("api_response", item.into()).into()
}

/// The request body of a route when its extractors do not say, Nest's `@ApiBody`: on a route
/// method inside `#[routes]`. `#[api_body(Upload, content_type = "multipart/form-data")]`;
/// the content type defaults to `application/json`.
#[proc_macro_attribute]
pub fn api_body(_args: TokenStream, item: TokenStream) -> TokenStream {
    openapi::misplaced("api_body", item.into()).into()
}

/// The tags of a route, Nest's `@ApiTags`: below `#[controller(...)]` for every route of the
/// controller, or on a route method inside `#[routes]`. Replaces the default tag, the controller's
/// name without `Controller`.
#[proc_macro_attribute]
pub fn api_tags(_args: TokenStream, item: TokenStream) -> TokenStream {
    openapi::misplaced("api_tags", item.into()).into()
}

/// Leaves routes out of the OpenAPI document, Nest's `@ApiExcludeController` and
/// `@ApiExcludeEndpoint`: below `#[controller(...)]` or on a route method inside `#[routes]`.
#[proc_macro_attribute]
pub fn api_exclude(_args: TokenStream, item: TokenStream) -> TokenStream {
    openapi::misplaced("api_exclude", item.into()).into()
}

/// Turns the annotated methods of a controller's `impl` block into routes.
///
/// ```ignore
/// #[routes]
/// impl TodoController {
///     #[get("/{id}")]
///     async fn find_one(&self, Path(id): Path<i64>) -> Result<Json<Todo>, HttpError> { ... }
///
///     #[post("/")]
///     #[guards(RolesGuard)]
///     #[meta(Roles(&["admin"]))]
///     async fn create(&self, Json(input): Json<NewTodo>) -> Result<Json<Todo>, HttpError> { ... }
/// }
/// ```
#[proc_macro_attribute]
pub fn routes(args: TokenStream, item: TokenStream) -> TokenStream {
    routes::expand(args.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
