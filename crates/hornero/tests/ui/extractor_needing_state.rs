use hornero::axum::extract::FromRequestParts;
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

#[derive(Clone)]
struct AppState;

struct CurrentUser;

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = HttpError;

    async fn from_request_parts(_: &mut Parts, _: &AppState) -> Result<Self, HttpError> {
        Ok(CurrentUser)
    }
}

#[controller("/me")]
struct MeController;

#[routes]
impl MeController {
    #[get("/")]
    fn me(&self, _user: CurrentUser) -> &'static str {
        "me"
    }
}

fn main() {}
