use hornero::prelude::*;

#[derive(Injectable)]
struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

// Above `#[controller]`: expanded before the controller could read it.
#[guards(AuthGuard)]
#[controller("/above")]
struct AboveController;

#[routes]
impl AboveController {
    #[get("/")]
    async fn handle(&self) -> &'static str {
        "above"
    }
}

// On a method of an impl block without `#[routes]`.
struct Plain;

impl Plain {
    #[interceptors(AuthGuard)]
    fn helper(&self) {}
}

// On a struct that is not a controller.
#[meta(1)]
struct NotAController;

fn main() {}
