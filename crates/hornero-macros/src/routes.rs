use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, FnArg, ImplItem, ImplItemFn, ItemImpl, LitStr, Token, Type};

use crate::openapi::{self, ROUTE_ATTRIBUTES, RouteApi};
use crate::util::{ENHANCERS, arc_inner};

const METHODS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("head", "HEAD"),
    ("options", "OPTIONS"),
];

struct RouteSpec {
    method: TokenStream,
    path: LitStr,
    guards: Vec<Type>,
    interceptors: Vec<Type>,
    filters: Vec<Type>,
    meta: Vec<Expr>,
    api: RouteApi,
}

pub(crate) fn expand(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !args.is_empty() {
        return Err(syn::Error::new_spanned(
            args,
            "#[routes] takes no arguments",
        ));
    }
    let mut item: ItemImpl = syn::parse2(item)?;
    if let Some((_, path, _)) = &item.trait_ {
        return Err(syn::Error::new_spanned(
            path,
            "#[routes] goes on the controller's inherent impl block",
        ));
    }

    let mut routes = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        if let Some(spec) = take_route_attributes(method)? {
            routes.push(register_route(method, spec)?);
        }
    }

    let self_ty = &item.self_ty;
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics ::hornero::Controller for #self_ty #where_clause {
            fn routes(c: &mut ::hornero::ControllerDef<Self>) {
                <Self as ::hornero::ControllerAttrs>::configure(c);
                #(#routes)*
            }
        }
    })
}

/// Rejects a `#[route("METHOD", ..)]` literal that is not an RFC 9110 token (`tchar`s only).
fn validate_method(method: &LitStr) -> syn::Result<()> {
    let value = method.value();
    let is_tchar = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if value.is_empty() || !value.chars().all(is_tchar) {
        return Err(syn::Error::new(
            method.span(),
            format!(
                "invalid HTTP method `{value}`: a method is a non-empty token of letters, digits and `!#$%&'*+-.^_`|~`"
            ),
        ));
    }
    Ok(())
}

/// Removes hornero's attributes from `method` and returns its route, if it has one.
fn take_route_attributes(method: &mut ImplItemFn) -> syn::Result<Option<RouteSpec>> {
    let mut route: Option<(TokenStream, LitStr, Span)> = None;
    let mut guards = Vec::new();
    let mut interceptors = Vec::new();
    let mut filters = Vec::new();
    let mut meta = Vec::new();
    let mut api = RouteApi::default();
    let mut first_enhancer: Option<Span> = None;

    let mut kept: Vec<Attribute> = Vec::new();
    for attr in std::mem::take(&mut method.attrs) {
        let Some(ident) = attr.path().get_ident().map(|i| i.to_string()) else {
            kept.push(attr);
            continue;
        };

        let parsed_route = if let Some((_, method_name)) =
            METHODS.iter().find(|(name, _)| *name == ident)
        {
            let path: LitStr = attr.parse_args()?;
            let method_ident = format_ident!("{}", method_name);
            Some((quote!(::hornero::__private::Method::#method_ident), path))
        } else if ident == "route" {
            let args = attr.parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)?;
            let [method_name, path]: [LitStr; 2] = args
                .into_iter()
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| {
                    syn::Error::new_spanned(&attr, "expected `#[route(\"METHOD\", \"/path\")]`")
                })?;
            validate_method(&method_name)?;
            Some((
                // Cannot fail: `validate_method` accepted the literal at compile time, and
                // `Method::from_bytes` accepts exactly RFC 9110 tokens.
                quote!(::hornero::__private::Method::from_bytes(#method_name.as_bytes()).expect("method validated by #[route]")),
                path,
            ))
        } else {
            None
        };

        if let Some((method_tokens, path)) = parsed_route {
            if route.is_some() {
                return Err(syn::Error::new_spanned(
                    attr,
                    "a handler can only have one route attribute",
                ));
            }
            route = Some((method_tokens, path, attr.span()));
            continue;
        }

        if ENHANCERS.contains(&ident.as_str()) {
            first_enhancer.get_or_insert(attr.span());
            match ident.as_str() {
                "guards" => guards
                    .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
                "interceptors" => interceptors
                    .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
                "filters" => filters
                    .extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?),
                _ => meta.push(attr.parse_args::<Expr>()?),
            }
            continue;
        }
        if ROUTE_ATTRIBUTES.contains(&ident.as_str()) {
            first_enhancer.get_or_insert(attr.span());
            api.read(&ident, &attr)?;
            continue;
        }
        kept.push(attr);
    }
    method.attrs = kept;

    match route {
        Some((method, path, _)) => Ok(Some(RouteSpec {
            method,
            path,
            guards,
            interceptors,
            filters,
            meta,
            api,
        })),
        None => match first_enhancer {
            Some(span) => Err(syn::Error::new(
                span,
                "this attribute needs a route attribute such as `#[get(\"/path\")]` on the same method",
            )),
            None => Ok(None),
        },
    }
}

fn register_route(method: &ImplItemFn, spec: RouteSpec) -> syn::Result<TokenStream> {
    let sig = &method.sig;
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.generics,
            "route handlers cannot be generic",
        ));
    }

    let mut inputs = sig.inputs.iter();
    let this = match inputs.next() {
        Some(FnArg::Receiver(receiver)) => {
            if receiver.colon_token.is_some() {
                match arc_inner(&receiver.ty) {
                    Some(_) => quote!(__hornero_this),
                    None => {
                        return Err(syn::Error::new_spanned(
                            receiver,
                            "route handlers take `&self` or `self: Arc<Self>`",
                        ));
                    }
                }
            } else if receiver.reference.is_some() && receiver.mutability.is_none() {
                quote!(&__hornero_this)
            } else {
                return Err(syn::Error::new_spanned(
                    receiver,
                    "route handlers take `&self`: one controller instance serves many requests concurrently",
                ));
            }
        }
        _ => {
            return Err(syn::Error::new_spanned(
                sig,
                "route handlers take `&self` as their first parameter",
            ));
        }
    };

    let mut params = Vec::new();
    let mut args = Vec::new();
    for (index, input) in inputs.enumerate() {
        let FnArg::Typed(arg) = input else {
            unreachable!("only the first input can be a receiver")
        };
        let binding = format_ident!("__hornero_arg{}", index);
        let ty = &arg.ty;
        params.push(quote_spanned!(ty.span()=> #binding: #ty));
        args.push(binding);
    }

    let name = &sig.ident;
    let name_str = name.to_string();
    let call = if sig.asyncness.is_some() {
        quote!(Self::#name(#this, #(#args),*).await)
    } else {
        quote!(Self::#name(#this, #(#args),*))
    };

    let RouteSpec {
        method: http_method,
        path,
        guards,
        interceptors,
        filters,
        meta,
        api,
    } = spec;
    let doc = openapi::route_doc(method, api);
    // The handler's deprecation is for its callers, not for the registration calling it.
    let allow_deprecated = method
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("deprecated"))
        .then(|| quote!(#[allow(deprecated)]));
    Ok(quote_spanned! {sig.span()=>
        #allow_deprecated
        c.route(
            #http_method,
            #path,
            #name_str,
            |__hornero_this: ::std::sync::Arc<Self>, #(#params),*| async move { #call },
        )
        #(.guard::<#guards>())*
        #(.interceptor::<#interceptors>())*
        #(.filter::<#filters>())*
        #(.meta(#meta))*
        #doc;
    })
}
