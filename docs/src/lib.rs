//! The hornero documentation, as a crate.
//!
//! The Markdown files (`README.md`, `design.md` and those under `guide`, `api` and `adr`) are the
//! source of truth and read fine on their own. This crate includes each of them as a module so
//! that:
//!
//! * `cargo test -p hornero-docs` compiles (and mostly runs) their Rust code blocks, and checks that
//!   blocks copied from `examples/` and `tests/` still match those files; a block that cannot be
//!   compiled must say why in an `<!-- ignore: reason -->` marker (API reference signatures are
//!   exempt);
//! * `cargo doc -p hornero-docs --open` renders the whole guide offline, next to the API docs.
//!
//! The build script copies the files into `OUT_DIR` first, turning links between them into
//! intra-doc links.
#![doc = include_str!(concat!(env!("OUT_DIR"), "/README.md"))]

/// The guide, chapter by chapter.
pub mod guide {
    #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/README.md"))]

    pub mod getting_started {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/01-getting-started.md"))]
    }
    pub mod providers {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/02-providers.md"))]
    }
    pub mod modules {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/03-modules.md"))]
    }
    pub mod controllers {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/04-controllers.md"))]
    }
    pub mod request_pipeline {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/05-request-pipeline.md"))]
    }
    pub mod scopes {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/06-scopes.md"))]
    }
    pub mod lifecycle {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/07-lifecycle.md"))]
    }
    pub mod errors {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/08-errors.md"))]
    }
    pub mod testing {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/09-testing.md"))]
    }
    pub mod configuration {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/10-configuration.md"))]
    }
    pub mod validation {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/11-validation.md"))]
    }
    pub mod database {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/12-database.md"))]
    }
    pub mod without_macros {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/13-without-macros.md"))]
    }
    pub mod recipes {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/14-recipes.md"))]
    }
    pub mod from_nestjs {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/15-from-nestjs.md"))]
    }
    pub mod openapi {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/guide/16-openapi.md"))]
    }
}

/// The API reference, crate by crate.
pub mod api {
    #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/README.md"))]

    pub mod hornero {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero.md"))]
    }
    pub mod hornero_core {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-core.md"))]
    }
    pub mod hornero_testing {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-testing.md"))]
    }
    pub mod hornero_config {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-config.md"))]
    }
    pub mod hornero_validation {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-validation.md"))]
    }
    pub mod hornero_sea_orm {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-sea-orm.md"))]
    }
    pub mod hornero_openapi {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/api/hornero-openapi.md"))]
    }
}

/// Design notes: the reasoning behind hornero's choices.
pub mod design {
    #![doc = include_str!(concat!(env!("OUT_DIR"), "/design.md"))]
}

/// Architecture decision records.
pub mod adr {
    pub mod runtime_di {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0001-runtime-di.md"))]
    }
    pub mod scope_bubbling {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0002-scope-bubbling.md"))]
    }
    pub mod strict_visibility {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0003-strict-visibility.md"))]
    }
    pub mod no_axum_wrapping {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0004-no-axum-wrapping.md"))]
    }
    pub mod qualifiers {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0005-qualifiers.md"))]
    }
    pub mod openapi_inference {
        #![doc = include_str!(concat!(env!("OUT_DIR"), "/adr/0006-openapi-inference.md"))]
    }
}

/// The repository root `README.md`: only its code blocks matter, so that the sample on the
/// front page is compiled like every other block.
#[cfg(doctest)]
#[doc = include_str!("../../README.md")]
pub struct RootReadme;
