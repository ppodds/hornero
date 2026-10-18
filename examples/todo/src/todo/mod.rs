//! The todo feature module.

pub mod audit;
pub mod controller;
pub mod dto;
pub mod entity;
pub mod error;
pub mod repo;
pub mod schema;
pub mod service;

use hornero::prelude::*;

use self::audit::{AuditLog, AuditTrail};
use self::controller::TodoController;
use self::repo::{SeaOrmTodoRepo, TodoRepo};
use self::schema::TodoSchema;
use self::service::TodoService;
use crate::auth::AuthModule;

#[module(
    imports = [AuthModule],
    controllers = [TodoController],
    providers = [
        TodoService,
        bind!(dyn TodoRepo => SeaOrmTodoRepo),
        AuditLog,
        value(AuditTrail::default()),
        TodoSchema,
    ],
    exports = [AuditTrail],
)]
pub struct TodoModule;
