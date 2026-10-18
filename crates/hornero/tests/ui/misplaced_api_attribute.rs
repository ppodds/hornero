use hornero::prelude::*;

struct Upload;

// A request body belongs to one route, not to a whole controller.
#[controller("/files")]
#[api_body(Upload)]
struct FileController;

#[routes]
impl FileController {
    #[get("/")]
    async fn list(&self) -> &'static str {
        ""
    }
}

#[controller("/a")]
struct AController;

#[routes]
impl AController {
    #[get("/")]
    #[api_response(700)]
    async fn handle(&self) -> &'static str {
        ""
    }
}

#[controller("/b")]
struct BController;

#[routes]
impl BController {
    #[get("/")]
    #[api_exclude(true)]
    async fn handle(&self) -> &'static str {
        ""
    }
}

#[controller("/c")]
struct CController;

#[routes]
impl CController {
    #[get("/")]
    #[api_response(200, content_type = "text/csv")]
    async fn handle(&self) -> &'static str {
        ""
    }
}

// Outside `#[routes]`.
struct Plain;

impl Plain {
    #[api_response(200)]
    fn helper(&self) {}
}

fn main() {}
