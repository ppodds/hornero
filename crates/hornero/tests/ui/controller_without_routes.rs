use hornero::prelude::*;

#[controller("/x")]
struct XController;

#[module(controllers = [XController])]
struct AppModule;

fn main() {}
