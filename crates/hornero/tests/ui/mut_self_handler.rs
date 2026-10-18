use hornero::prelude::*;

#[controller("/x")]
struct XController;

#[routes]
impl XController {
    #[get("/")]
    async fn handle(&mut self) -> &'static str {
        "x"
    }
}

fn main() {}
