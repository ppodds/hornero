//! A controller whose handlers raise domain errors with `?`, and filters that catch them by type.

use std::sync::Arc;

use axum::extract::Path;
use hornero_core::{
    BoxError, Controller, ControllerDef, ExceptionFilter, HttpError, Injectable, Key, Module,
    ModuleDef, Resolver, RouteContext,
};

#[derive(Debug)]
pub enum TodoError {
    NotFound(u32),
    Locked,
}

impl std::fmt::Display for TodoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoError::NotFound(id) => write!(f, "no row for todo {id}"),
            TodoError::Locked => f.write_str("todo table is locked"),
        }
    }
}

impl std::error::Error for TodoError {}

/// A domain error with a cause of its own, which no filter catches.
#[derive(Debug)]
pub struct BillingError(GatewayTimeout);

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("card 4242 declined")
    }
}

impl std::error::Error for BillingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Debug)]
pub struct GatewayTimeout;

impl std::fmt::Display for GatewayTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("payment gateway timed out")
    }
}

impl std::error::Error for GatewayTimeout {}

macro_rules! unit_injectable {
    ($($name:ident),+) => {$(
        pub struct $name;
        impl Injectable for $name {
            fn dependencies() -> Vec<Key> { Vec::new() }
            async fn construct(_: &Resolver) -> Result<Self, BoxError> { Ok($name) }
        }
    )+};
}

unit_injectable!(TodoErrorFilter, FmtErrorFilter, DomainController);

/// Catches `TodoError` only, the way Nest's `@Catch(TodoError)` would.
impl ExceptionFilter for TodoErrorFilter {
    type Error = TodoError;

    async fn catch(&self, err: &TodoError, _: &RouteContext) -> HttpError {
        match err {
            TodoError::NotFound(id) => HttpError::not_found(format!("todo {id} not found")),
            TodoError::Locked => HttpError::internal("internal server error"),
        }
    }
}

/// A controller-level filter for an error these handlers never raise.
impl ExceptionFilter for FmtErrorFilter {
    type Error = std::fmt::Error;

    async fn catch(&self, _: &std::fmt::Error, _: &RouteContext) -> HttpError {
        HttpError::conflict("formatting failed")
    }
}

pub async fn find_todo(id: u32) -> Result<&'static str, TodoError> {
    Err(if id == 0 {
        TodoError::Locked
    } else {
        TodoError::NotFound(id)
    })
}

pub async fn charge() -> Result<(), BillingError> {
    Err(BillingError(GatewayTimeout))
}

impl Controller for DomainController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/domain").filter::<FmtErrorFilter>();
        c.get(
            "/todos/{id}",
            "todo",
            |_: Arc<Self>, Path(id): Path<u32>| async move {
                let title = find_todo(id).await?;
                Ok::<_, HttpError>(title)
            },
        );
        c.get("/billing", "billing", |_: Arc<Self>| async {
            charge().await?;
            Ok::<_, HttpError>("charged")
        });
        // A raised error with a header around it, which the filter's answer keeps.
        c.get(
            "/signed-out-todo",
            "signed_out_todo",
            |_: Arc<Self>| async {
                (
                    [(axum::http::header::SET_COOKIE, "session=; Max-Age=0")],
                    HttpError::from(TodoError::NotFound(3)),
                )
            },
        );
        c.get("/conflict", "conflict", |_: Arc<Self>| async {
            HttpError::conflict("already exists")
        });
        // An answer whose source is a type a filter catches: still the answer.
        c.get(
            "/sourced-conflict",
            "sourced_conflict",
            |_: Arc<Self>| async {
                HttpError::conflict("already exists").with_source(TodoError::NotFound(7))
            },
        );
        c.get("/panic", "panic", |_: Arc<Self>| async {
            if true {
                panic!("boom");
            }
            "unreachable"
        });
    }
}

pub struct DomainApp;

impl Module for DomainApp {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<DomainController>();
    }
}
