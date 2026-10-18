# hornero

> **⚠️ Experimental.** hornero is an experiment in what a NestJS-style, DI-driven web framework can look like in Rust. It is not published to crates.io, its API may change incompatibly at any time, and it has not been battle-tested. **Do not use it in production.**

The name comes from the hornero (ovenbird), a bird that builds its nest out of clay in the shape of an oven, a nod to NestJS's "Nest".

A NestJS-style web framework for Rust, built on tokio and axum 0.8: modules, dependency injection, controllers, guards, interceptors and exception filters, without hiding axum.

```rust,no_run
use hornero::prelude::*;

#[derive(Injectable)]
struct Greeter;

impl Greeter {
    fn greet(&self, name: &str) -> String {
        format!("hello, {name}")
    }
}

#[controller("/hello")]
struct HelloController {
    greeter: Arc<Greeter>,
}

#[routes]
impl HelloController {
    #[get("/{name}")]
    async fn hello(&self, Path(name): Path<String>) -> String {
        self.greeter.greet(&name)
    }
}

#[module(controllers = [HelloController], providers = [Greeter])]
struct AppModule;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    Hornero::new(AppModule).listen("0.0.0.0:3000").await
}

#[test]
fn dependency_graph_is_valid() {
    Hornero::new(AppModule).validate().unwrap();
}
```

## Features

- **The whole dependency graph is validated before startup.** Missing dependencies, cycles, visibility errors, ambiguity, scope violations and route conflicts are reported together, with hints for fixing them.
- **No DI cost per request.** Singletons are injected at startup; only request-scoped chains are built for each request.
- **Nest-style scopes.** Singleton / Request / Transient. Request scope propagates up the dependency chain, and a scope report is printed at startup.
- **Strict module encapsulation.** A provider must be exported before importers can use it. Global modules are supported.
- **Incremental.** Every macro expands to a public builder API, and any axum `Router` can be mounted into a module.
- **OpenAPI without writing it twice.** Paths, parameters, bodies, responses and authentication are inferred from the route table, handler signatures and guards, with Swagger UI included (`hornero-openapi`).
- **Small integration crates.** `hornero-config`, `hornero-validation`, `hornero-sea-orm`, `hornero-openapi` and `hornero-testing`.

## Crates

| crate | purpose |
|---|---|
| `hornero` | facade: core + macros + axum re-exports |
| `hornero-core` | DI container, module system, HTTP pipeline |
| `hornero-macros` | `#[module]`, `#[derive(Injectable)]`, `#[injectable]`, `#[controller]`, `#[routes]` |
| `hornero-testing` | `TestApp`, provider / guard overrides, in-process client |
| `hornero-config` | typed `ConfigModule` built on figment |
| `hornero-validation` | `Valid<E>` extractor built on garde |
| `hornero-sea-orm` | `SeaOrmModule` and `Db<Tag>` |
| `hornero-openapi` | OpenAPI documents and Swagger UI generated with utoipa (the `openapi` / `swagger-ui` features of `hornero`) |

## Documentation

> The documentation is currently written in Traditional Chinese. Translations are welcome.

| | |
|---|---|
| [Getting started](docs/guide/01-getting-started.md) | Build your first API in ten minutes |
| [Guide](docs/guide/README.md) | 15 chapters: providers, modules, controllers, testing, databases, and migrating from NestJS |
| [API reference](docs/api/README.md) | Every public type, method and macro syntax, per crate |
| [Recipes](docs/guide/14-recipes.md) | tracing, CORS, request ids, background jobs, embedding an existing axum app, … |

Rust code in the docs is checked (`cargo test -p hornero-docs`): it is either a compiled doctest or a snippet compared verbatim against a real file, with a few documented exceptions and the signatures in the API reference. See [docs/README.md](docs/README.md) for details. Design notes are in [docs/design.md](docs/design.md) and major decisions in [docs/adr](docs/adr).

## Example

`examples/todo` demonstrates the main concepts: configuration, SeaORM (sqlite), an `AuthModule` that exports a guard, typed route metadata, validation, a request-scoped audit log, a global interceptor and exception filter, and domain errors raised with `?` and converted by type in the exception filter. `/docs` serves the inferred Swagger UI.

```sh
# Run from examples/todo: config.toml and the sqlite file todo.db are relative to the working directory
cd examples/todo && cargo run
curl -X POST localhost:3000/todos -H 'authorization: Bearer user-token' \
     -H 'content-type: application/json' -d '{"title":"try hornero"}'
```

## Development

Common tasks are defined in the [justfile](justfile) (run `just` to list them):

```sh
just check          # fmt-check + clippy + tests
just test
just lint
just coverage       # requires cargo-llvm-cov
just coverage-html
```

Without `just`, the underlying commands are `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.

## License

Licensed under either of MIT or Apache-2.0, at your option.
