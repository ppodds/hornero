//! File uploads through axum's `Multipart`, with the `multipart` feature.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::multipart::MultipartError;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::send;
use hornero_core::{
    BoxError, Controller, ControllerDef, ExceptionFilter, Hornero, HttpError, Injectable, Key,
    Module, ModuleDef, Resolver, RouteContext,
};

macro_rules! unit_injectable {
    ($($name:ident),+) => {$(
        struct $name;
        impl Injectable for $name {
            fn dependencies() -> Vec<Key> { Vec::new() }
            async fn construct(_: &Resolver) -> Result<Self, BoxError> { Ok($name) }
        }
    )+};
}

const BOUNDARY: &str = "hornero-boundary";

/// The body limit of the tests that set one: `form()` is over it, `truncated()` under it.
const LIMIT: usize = 100;

fn upload(uri: &str, body: impl Into<String>) -> Request<Body> {
    Request::post(uri)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body.into()))
        .unwrap()
}

/// A text field and a file, over `LIMIT` bytes in all.
fn form() -> String {
    format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nreport\r\n\
         --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"r.txt\"\r\n\
         Content-Type: text/plain\r\n\r\nhello\r\n\
         --{BOUNDARY}--\r\n"
    )
}

/// A field whose data never ends.
fn truncated() -> String {
    format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nrep")
}

/// Wraps `Multipart` in an extractor of its own, forwarding its rejection with `?`.
struct Upload(#[allow(dead_code)] Multipart);

impl<S: Send + Sync> FromRequest<S> for Upload {
    type Rejection = HttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, HttpError> {
        Ok(Upload(Multipart::from_request(req, state).await?))
    }
}

unit_injectable!(UploadController, EchoController);

impl Controller for UploadController {
    fn routes(c: &mut ControllerDef<Self>) {
        // Reads every part with `?`: `name=text` for fields, `name=filename:len` for files.
        c.post(
            "/upload",
            "upload",
            |_: Arc<Self>, mut form: Multipart| async move {
                let mut parts = Vec::new();
                while let Some(field) = form.next_field().await? {
                    let name = field.name().unwrap_or_default().to_owned();
                    let file = field.file_name().map(str::to_owned);
                    let bytes = field.bytes().await?;
                    parts.push(match file {
                        Some(file) => format!("{name}={file}:{}", bytes.len()),
                        None => format!("{name}={}", String::from_utf8_lossy(&bytes)),
                    });
                }
                Ok::<_, HttpError>(parts.join(","))
            },
        );
        c.post(
            "/maybe-upload",
            "maybe_upload",
            |_: Arc<Self>, form: Option<Multipart>| async move {
                if form.is_some() { "form" } else { "no form" }
            },
        );
        c.post(
            "/wrapped-upload",
            "wrapped_upload",
            |_: Arc<Self>, _: Upload| async { "in" },
        );
    }
}

impl Controller for EchoController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.post("/echo", "echo", |_: Arc<Self>, body: String| async move {
            body.len().to_string()
        });
    }
}

/// The upload routes, with the body limit set on their module only.
struct LimitedUploads;

impl Module for LimitedUploads {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<UploadController>()
            .layer(DefaultBodyLimit::max(LIMIT));
    }
}

struct EchoModule;

impl Module for EchoModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<EchoController>();
    }
}

struct App;

impl Module for App {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<UploadController>();
    }
}

/// Renders errors as their bare message, so assertions read the text.
fn plain(err: &HttpError, _: &RouteContext) -> Response {
    (err.status(), err.message().to_owned()).into_response()
}

#[tokio::test]
async fn uploads_read_with_the_question_mark() {
    let router = Hornero::new(App).build().await.unwrap().router();

    let res = send(&router, upload("/upload", form())).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::OK, "title=report,file=r.txt:5")
    );

    // `Option<Multipart>`: a request that is not multipart is `None`.
    let res = send(&router, upload("/maybe-upload", form())).await;
    assert_eq!(res.body, "form");
    let plain_post = Request::post("/maybe-upload").body(Body::empty()).unwrap();
    assert_eq!(send(&router, plain_post).await.body, "no form");
}

#[tokio::test]
async fn multipart_errors_keep_axum_s_status() {
    assert!(truncated().len() < LIMIT && form().len() > LIMIT);
    let router = Hornero::new(App)
        .layer(DefaultBodyLimit::max(LIMIT))
        .error_renderer(plain)
        .build()
        .await
        .unwrap()
        .router();

    // Not multipart: the parameter's rejection.
    let no_boundary = Request::post("/upload").body(Body::empty()).unwrap();
    let res = send(&router, no_boundary).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (
            StatusCode::BAD_REQUEST,
            "Invalid `boundary` for `multipart/form-data` request"
        )
    );

    // The same rejection forwarded with `?` from an extractor of the user's.
    let no_boundary = Request::post("/wrapped-upload")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&router, no_boundary).await.status,
        StatusCode::BAD_REQUEST
    );

    // A malformed body, read with `?`.
    let res = send(&router, upload("/upload", truncated())).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(!res.body.is_empty());

    // Past the body limit, read with `?`.
    let res = send(&router, upload("/upload", form())).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "Request payload is too large"
        )
    );
}

#[tokio::test]
async fn a_module_s_body_limit_covers_its_routes_only() {
    struct Both;
    impl Module for Both {
        fn define(&self, m: &mut ModuleDef) {
            m.import(LimitedUploads).import(EchoModule);
        }
    }
    let router = Hornero::new(Both).build().await.unwrap().router();

    let res = send(&router, upload("/upload", form())).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);

    let echo = Request::post("/echo")
        .body(Body::from("x".repeat(LIMIT + 1)))
        .unwrap();
    let res = send(&router, echo).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, (LIMIT + 1).to_string());
}

unit_injectable!(TeapotUploads);

impl ExceptionFilter for TeapotUploads {
    type Error = MultipartError;

    async fn catch(&self, _: &MultipartError, _: &RouteContext) -> HttpError {
        HttpError::new(StatusCode::IM_A_TEAPOT, "teapot")
    }
}

/// Renders `<status> multipart=<whether the source is a MultipartError>`.
fn multipart_cause(err: &HttpError, _: &RouteContext) -> Response {
    let multipart = err.is::<MultipartError>();
    (
        err.status(),
        format!("{} multipart={multipart}", err.status().as_u16()),
    )
        .into_response()
}

#[tokio::test]
async fn multipart_errors_are_answers_that_keep_their_source() {
    let router = Hornero::new(App)
        .layer(DefaultBodyLimit::max(LIMIT))
        .filter::<TeapotUploads>()
        .error_renderer(multipart_cause)
        .build()
        .await
        .unwrap()
        .router();

    // Answers, as axum's rejections are: no filter maps them, and the renderer finds the cause.
    let res = send(&router, upload("/upload", truncated())).await;
    assert_eq!(res.body, "400 multipart=true");
    let res = send(&router, upload("/upload", form())).await;
    assert_eq!(res.body, "413 multipart=true");
}
