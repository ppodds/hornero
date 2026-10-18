use hornero::prelude::*;

#[controller("/x")]
struct XController;

#[routes]
impl XController {
    #[route("GE T", "/")]
    async fn handle(&self) -> &'static str {
        "x"
    }
}

fn main() {}
