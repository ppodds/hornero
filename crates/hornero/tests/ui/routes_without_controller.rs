use hornero::prelude::*;

#[derive(Injectable)]
struct XController;

#[routes]
impl XController {
    #[get("/")]
    async fn handle(&self) -> &'static str {
        "x"
    }
}

fn main() {}
