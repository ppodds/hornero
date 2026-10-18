use hornero::prelude::*;

struct Primary;

#[derive(Injectable)]
struct Service {
    #[inject(default, qualifier = Primary)]
    name: String,
}

fn main() {}
