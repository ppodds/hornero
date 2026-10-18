use std::fmt;

use hornero::prelude::*;
use sea_orm::DbErr;

use crate::common::problem;

/// What can go wrong in the todo feature. The service raises it without knowing about HTTP.
#[derive(Debug)]
pub enum TodoError {
    NotFound(i32),
    Storage(DbErr),
}

impl fmt::Display for TodoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TodoError::NotFound(id) => write!(f, "todo {id} does not exist"),
            TodoError::Storage(_) => f.write_str("todo storage failed"),
        }
    }
}

impl std::error::Error for TodoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TodoError::Storage(err) => Some(err),
            TodoError::NotFound(_) => None,
        }
    }
}

impl From<DbErr> for TodoError {
    fn from(err: DbErr) -> Self {
        TodoError::Storage(err)
    }
}

/// Turns a `TodoError` that a handler raised with `?` into a response, Nest's
/// `@Catch(TodoError)`. Storage failures stay opaque 500s, logged with their cause.
#[derive(Injectable)]
pub struct TodoErrorFilter;

impl ExceptionFilter for TodoErrorFilter {
    fn catches(&self, err: &HttpError) -> bool {
        err.is::<TodoError>()
    }

    async fn catch(&self, err: &HttpError, ctx: &RouteContext) -> Response {
        match err.downcast_ref::<TodoError>() {
            Some(TodoError::NotFound(id)) => {
                problem(&HttpError::not_found(format!("todo {id} not found")), ctx)
            }
            _ => problem(err, ctx),
        }
    }
}
