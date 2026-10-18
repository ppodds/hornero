use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, ItemStruct, LitStr, Token, Type};

use crate::openapi::{CONTROLLER_ATTRIBUTES, ControllerApi};
use crate::util::{InjectOptions, parse_bracketed};

#[derive(Default)]
struct ControllerArgs {
    prefix: Option<LitStr>,
    guards: Vec<Type>,
    interceptors: Vec<Type>,
    filters: Vec<Type>,
    meta: Vec<Expr>,
}

impl Parse for ControllerArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = ControllerArgs::default();
        if input.peek(LitStr) {
            args.prefix = Some(input.parse()?);
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "guards" => args.guards = parse_bracketed(input)?,
                "interceptors" => args.interceptors = parse_bracketed(input)?,
                "filters" => args.filters = parse_bracketed(input)?,
                "meta" => args.meta = parse_bracketed(input)?,
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "unknown controller option; expected a path literal, `guards`, `interceptors`, `filters` or `meta`",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(args)
    }
}

pub(crate) fn expand(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: ControllerArgs = syn::parse2(args)?;
    let mut item: ItemStruct = syn::parse2(item)?;

    // `#[injectable(...)]` below `#[controller]` would derive `Injectable` a second time;
    // fold its options into the derive's `#[inject(...)]` instead.
    let mut options = InjectOptions::default();
    let mut folded = false;
    for attr in item
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("injectable"))
    {
        folded = true;
        if !matches!(attr.meta, syn::Meta::Path(_)) {
            attr.parse_nested_meta(|meta| options.parse_meta(meta))?;
        }
    }
    item.attrs.retain(|a| !a.path().is_ident("injectable"));
    let inject = if folded {
        options.to_attribute()
    } else {
        TokenStream::new()
    };

    // Enhancers stacked below `#[controller]`, one attribute per line like Nest's decorators.
    // They follow the ones given as arguments, top to bottom.
    let mut args = args;
    let mut api = ControllerApi::default();
    let mut kept = Vec::new();
    for attr in std::mem::take(&mut item.attrs) {
        match attr.path().get_ident().map(Ident::to_string).as_deref() {
            Some("guards") => args
                .guards
                .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
            Some("interceptors") => args
                .interceptors
                .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
            Some("filters") => args
                .filters
                .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
            Some("meta") => args.meta.push(attr.parse_args::<Expr>()?),
            Some(name) if CONTROLLER_ATTRIBUTES.contains(&name) => api.read(name, &attr)?,
            _ => kept.push(attr),
        }
    }
    item.attrs = kept;

    let prefix = args.prefix.map(|prefix| quote!(c.prefix(#prefix);));
    let guards = args.guards.iter().map(|guard| quote!(c.guard::<#guard>();));
    let interceptors = args
        .interceptors
        .iter()
        .map(|interceptor| quote!(c.interceptor::<#interceptor>();));
    let filters = args
        .filters
        .iter()
        .map(|filter| quote!(c.filter::<#filter>();));
    let meta = args.meta.iter().map(|value| quote!(c.meta(#value);));
    let doc = api.configure();

    let name = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #[derive(::hornero::Injectable)]
        #inject
        #item

        impl #impl_generics ::hornero::ControllerAttrs for #name #ty_generics #where_clause {
            fn configure(c: &mut ::hornero::ControllerDef<Self>) {
                #prefix
                #(#guards)*
                #(#interceptors)*
                #(#filters)*
                #(#meta)*
                #doc
            }
        }
    })
}
