use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Expr, Fields, FnArg, ImplItem, ImplItemFn, Item, ItemImpl, ReturnType, Type,
};

use crate::openapi;
use crate::util::{InjectOptions, Injected, is_result};

pub(crate) fn derive(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let mut options = InjectOptions::default();
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("inject")) {
        attr.parse_nested_meta(|meta| options.parse_meta(meta))?;
    }

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Injectable)] only supports structs; implement `hornero::Injectable` by hand for other types",
        ));
    };

    let mut deps = Vec::new();
    let mut values = Vec::new();
    for (index, field) in data.fields.iter().enumerate() {
        let mut default = false;
        let mut with: Option<Expr> = None;
        let mut qualifier: Option<Type> = None;
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("inject")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    default = true;
                    Ok(())
                } else if meta.path.is_ident("with") {
                    with = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("qualifier") {
                    qualifier = Some(meta.value()?.parse()?);
                    Ok(())
                } else {
                    Err(meta.error(
                        "unsupported field option; expected `default`, `with = expr` or `qualifier = Type`",
                    ))
                }
            })?;
        }
        if let Some(qualifier) = qualifier.as_ref().filter(|_| default || with.is_some()) {
            return Err(syn::Error::new_spanned(
                qualifier,
                "`qualifier` selects which provider to inject, so it cannot be combined with `default` or `with`",
            ));
        }

        let value = if let Some(expr) = with {
            quote_spanned!(expr.span()=> #expr)
        } else if default {
            quote_spanned!(field.ty.span()=> ::core::default::Default::default())
        } else if let Some(injected) = Injected::of(&field.ty, qualifier) {
            deps.push(injected.key());
            injected.value(field.ty.span())
        } else {
            let name = field
                .ident
                .as_ref()
                .map(|ident| format!("`{ident}`"))
                .unwrap_or_else(|| format!("{index}"));
            return Err(syn::Error::new_spanned(
                &field.ty,
                format!(
                    "field {name} is not an `Arc<T>`, so it cannot be injected; wrap it in `Arc`, or add `#[inject(default)]` or `#[inject(with = expr)]`"
                ),
            ));
        };
        values.push((field.ident.clone(), value));
    }

    let construct = match &data.fields {
        Fields::Named(_) => {
            let fields = values.iter().map(|(ident, value)| quote!(#ident: #value));
            quote!(Self { #(#fields),* })
        }
        Fields::Unnamed(_) => {
            let fields = values.iter().map(|(_, value)| value);
            quote!(Self(#(#fields),*))
        }
        Fields::Unit => quote!(Self),
    };

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let consts = options.consts();
    let describe = openapi::describe();
    Ok(quote! {
        impl #impl_generics ::hornero::Injectable for #name #ty_generics #where_clause {
            #consts

            fn dependencies() -> ::std::vec::Vec<::hornero::Key> {
                ::std::vec![#(#deps),*]
            }

            #[allow(unused_variables)]
            async fn construct(resolver: &::hornero::Resolver) -> ::core::result::Result<Self, ::hornero::BoxError> {
                ::core::result::Result::Ok(#construct)
            }

            #describe
        }
    })
}

pub(crate) fn attribute(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mut options = InjectOptions::default();
    syn::meta::parser(|meta| options.parse_meta(meta)).parse2(args)?;

    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => {
            let attribute = options.to_attribute();
            Ok(quote! {
                #[derive(::hornero::Injectable)]
                #attribute
                #item
            })
        }
        Item::Impl(item) => constructor_injection(options, item),
        other => Err(syn::Error::new_spanned(
            other,
            "#[injectable] goes on a struct or on an impl block with a `new` constructor",
        )),
    }
}

fn constructor_injection(options: InjectOptions, mut item: ItemImpl) -> syn::Result<TokenStream> {
    if let Some((_, path, _)) = &item.trait_ {
        return Err(syn::Error::new_spanned(
            path,
            "#[injectable] goes on an inherent impl block, not a trait impl",
        ));
    }

    let marked: Vec<usize> = item
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, impl_item)| match impl_item {
            ImplItem::Fn(f) if f.attrs.iter().any(|a| a.path().is_ident("constructor")) => {
                Some(index)
            }
            _ => None,
        })
        .collect();
    let index = match marked.as_slice() {
        [index] => *index,
        [] => item
            .items
            .iter()
            .position(|impl_item| matches!(impl_item, ImplItem::Fn(f) if f.sig.ident == "new"))
            .ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    "#[injectable] needs a constructor: a `fn new(...)`, or a function marked `#[constructor]`",
                )
            })?,
        [_, second, ..] => {
            return Err(syn::Error::new_spanned(
                &item.items[*second],
                "only one function can be marked `#[constructor]`",
            ));
        }
    };

    let ImplItem::Fn(constructor) = &mut item.items[index] else {
        unreachable!("index points at a function");
    };
    constructor
        .attrs
        .retain(|a| !a.path().is_ident("constructor"));
    let body = constructor_call(constructor)?;
    // `#[inject(...)]` on parameters is read above; rustc would reject it as an unknown attribute.
    for input in &mut constructor.sig.inputs {
        if let FnArg::Typed(arg) = input {
            arg.attrs.retain(|a| !a.path().is_ident("inject"));
        }
    }

    let self_ty = &item.self_ty;
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();
    let consts = options.consts();
    let deps = &body.deps;
    let call = &body.call;
    let describe = openapi::describe();
    Ok(quote! {
        #item

        impl #impl_generics ::hornero::Injectable for #self_ty #where_clause {
            #consts

            fn dependencies() -> ::std::vec::Vec<::hornero::Key> {
                ::std::vec![#(#deps),*]
            }

            #[allow(unused_variables)]
            async fn construct(resolver: &::hornero::Resolver) -> ::core::result::Result<Self, ::hornero::BoxError> {
                #call
            }

            #describe
        }
    })
}

struct ConstructorCall {
    /// `Key` expressions.
    deps: Vec<TokenStream>,
    call: TokenStream,
}

fn constructor_call(constructor: &ImplItemFn) -> syn::Result<ConstructorCall> {
    let sig = &constructor.sig;
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.generics,
            "constructors cannot be generic",
        ));
    }

    let mut deps = Vec::new();
    let mut bindings = Vec::new();
    let mut args = Vec::new();
    for (index, input) in sig.inputs.iter().enumerate() {
        let FnArg::Typed(arg) = input else {
            return Err(syn::Error::new_spanned(
                input,
                "a constructor cannot take `self`",
            ));
        };
        let mut qualifier: Option<Type> = None;
        for attr in arg.attrs.iter().filter(|a| a.path().is_ident("inject")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("qualifier") {
                    qualifier = Some(meta.value()?.parse()?);
                    Ok(())
                } else {
                    Err(meta.error("unsupported parameter option; expected `qualifier = Type`"))
                }
            })?;
        }
        let injected = Injected::of(&arg.ty, qualifier).ok_or_else(|| {
            syn::Error::new_spanned(
                &arg.ty,
                "constructor parameters must be `Arc<T>`; each `T` is resolved from the container",
            )
        })?;
        let binding = format_ident!("__hornero_arg{}", index);
        let value = injected.value(arg.ty.span());
        bindings.push(quote_spanned!(arg.ty.span()=> let #binding = #value;));
        args.push(binding);
        deps.push(injected.key());
    }

    let name = &sig.ident;
    let awaited = if sig.asyncness.is_some() {
        quote!(Self::#name(#(#args),*).await)
    } else {
        quote!(Self::#name(#(#args),*))
    };
    let result = match &sig.output {
        ReturnType::Type(_, ty) if is_result(ty) => {
            quote!(#awaited.map_err(::core::convert::Into::into))
        }
        _ => quote!(::core::result::Result::Ok(#awaited)),
    };
    Ok(ConstructorCall {
        deps,
        call: quote! {
            #(#bindings)*
            #result
        },
    })
}
