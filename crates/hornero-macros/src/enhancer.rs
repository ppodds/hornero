//! `#[guards]`, `#[interceptors]`, `#[filters]` and `#[meta]` are read and removed by
//! `#[controller]` and `#[routes]`. These attribute macros only expand when one is left over, that
//! is misplaced, and replace rustc's "cannot find attribute" with where it belongs.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Attribute, Item};

pub(crate) fn misplaced(name: &str, item: TokenStream) -> TokenStream {
    let message = match syn::parse2::<Item>(item.clone()) {
        Ok(Item::Struct(s)) if s.attrs.iter().any(is_controller) => format!(
            "`#[{name}(...)]` must come after `#[controller(...)]`: put `#[controller]` first and the enhancers below it"
        ),
        Ok(Item::Struct(_)) => {
            format!("`#[{name}(...)]` on a struct needs `#[controller(...)]` above it")
        }
        _ => format!(
            "`#[{name}(...)]` goes below `#[controller(...)]` on a controller struct, or on a route method of an impl block marked `#[routes]`"
        ),
    };
    let error = syn::Error::new(Span::call_site(), message).into_compile_error();
    // Keep the item so the misplaced attribute is the only error reported.
    quote!(#error #item)
}

fn is_controller(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "controller")
}
