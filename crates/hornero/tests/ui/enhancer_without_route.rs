use hornero::prelude::*;

#[derive(Injectable)]
struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

#[controller("/x")]
struct XController;

#[routes]
impl XController {
    #[guards(AuthGuard)]
    async fn handle(&self) -> &'static str {
        "x"
    }
}

fn main() {}
