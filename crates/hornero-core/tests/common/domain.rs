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

#[derive(Debug)]
pub struct BillingError;

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("card 4242 declined")
    }
}

impl std::error::Error for BillingError {}

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
    Err(BillingError)
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
        c.get("/conflict", "conflict", |_: Arc<Self>| async {
            HttpError::conflict("already exists")
        });
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
