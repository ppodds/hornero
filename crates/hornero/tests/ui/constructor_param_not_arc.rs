use hornero::prelude::*;

struct Service;

#[injectable]
impl Service {
    fn new(name: String) -> Self {
        let _ = name;
        Service
    }
}

fn main() {}
