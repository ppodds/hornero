use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::meta::ParseNestedMeta;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{GenericArgument, Ident, PathArguments, Token, Type, bracketed};

/// Enhancer attributes, accepted on route methods and below `#[controller]`.
pub(crate) const ENHANCERS: &[&str] = &["guards", "interceptors", "filters", "meta"];

/// Options shared by `#[derive(Injectable)]` (`#[inject(...)]`) and `#[injectable(...)]`.
#[derive(Default)]
pub(crate) struct InjectOptions {
    scope: Option<Ident>,
    hooks: bool,
}

impl InjectOptions {
    pub(crate) fn parse_meta(&mut self, meta: ParseNestedMeta) -> syn::Result<()> {
        if meta.path.is_ident("scope") {
            let scope: Ident = meta.value()?.parse()?;
            match scope.to_string().as_str() {
                "singleton" | "request" | "transient" => self.scope = Some(scope),
                _ => {
                    return Err(syn::Error::new_spanned(
                        scope,
                        "expected `singleton`, `request` or `transient`",
                    ));
                }
            }
            Ok(())
        } else if meta.path.is_ident("hooks") {
            self.hooks = true;
            Ok(())
        } else {
            Err(meta.error("unsupported option; expected `scope = ...` or `hooks`"))
        }
    }

    /// Associated consts of the generated `Injectable` impl.
    pub(crate) fn consts(&self) -> TokenStream {
        let mut out = TokenStream::new();
        if let Some(scope) = &self.scope {
            let variant = match scope.to_string().as_str() {
                "request" => quote!(Request),
                "transient" => quote!(Transient),
                _ => quote!(Singleton),
            };
            out.extend(quote! {
                const SCOPE: ::hornero::Scope = ::hornero::Scope::#variant;
                // An explicit scope is a promise: bubbling into request scope becomes an error.
                const SCOPE_PINNED: bool = true;
            });
        }
        if self.hooks {
            out.extend(quote! {
                const LIFECYCLE: ::core::option::Option<::hornero::LifecycleFn<Self>> = ::hornero::hooks::<Self>();
            });
        }
        out
    }

    /// Re-emits the options as a struct-level `#[inject(...)]` attribute.
    pub(crate) fn to_attribute(&self) -> TokenStream {
        let mut items = Vec::new();
        if let Some(scope) = &self.scope {
            items.push(quote!(scope = #scope));
        }
        if self.hooks {
            items.push(quote!(hooks));
        }
        if items.is_empty() {
            TokenStream::new()
        } else {
            quote!(#[inject(#(#items),*)])
        }
    }
}

/// An injected field or constructor parameter: `Arc<T>`, or `Option<Arc<T>>` for an optional
/// dependency, possibly marked `#[inject(qualifier = Q)]`.
pub(crate) struct Injected<'a> {
    pub(crate) inner: &'a Type,
    pub(crate) optional: bool,
    pub(crate) qualifier: Option<Type>,
}

impl<'a> Injected<'a> {
    /// Recognises `Arc<T>` and `Option<Arc<T>>` (with any path prefix).
    pub(crate) fn of(ty: &'a Type, qualifier: Option<Type>) -> Option<Self> {
        let (inner, optional) = match arc_inner(ty) {
            Some(inner) => (inner, false),
            None => (arc_inner(generic_arg(ty, "Option")?)?, true),
        };
        Some(Self {
            inner,
            optional,
            qualifier,
        })
    }

    /// The `Key` expression listed in `dependencies()`.
    pub(crate) fn key(&self) -> TokenStream {
        let inner = self.inner;
        let mut key = quote!(::hornero::Key::of::<#inner>());
        if let Some(qualifier) = &self.qualifier {
            key = quote!(#key.qualified::<#qualifier>());
        }
        if self.optional {
            key = quote!(#key.optional());
        }
        key
    }

    /// The expression taking the dependency out of `resolver`, typed like the field.
    pub(crate) fn value(&self, span: Span) -> TokenStream {
        let inner = self.inner;
        match (&self.qualifier, self.optional) {
            (None, false) => quote_spanned!(span=> resolver.get::<#inner>()?),
            (None, true) => quote_spanned!(span=>
                resolver.resolve::<::core::option::Option<::std::sync::Arc<#inner>>>()?
            ),
            (Some(qualifier), false) => quote_spanned!(span=>
                resolver.resolve::<::hornero::Qualified<#inner, #qualifier>>()?.into_inner()
            ),
            (Some(qualifier), true) => quote_spanned!(span=>
                resolver
                    .resolve::<::core::option::Option<::hornero::Qualified<#inner, #qualifier>>>()?
                    .map(::hornero::Qualified::into_inner)
            ),
        }
    }
}

/// `T` for a type spelled `Arc<T>` (with any path prefix), the shape of an injected dependency.
pub(crate) fn arc_inner(ty: &Type) -> Option<&Type> {
    generic_arg(ty, "Arc")
}

/// `(T, Q)` for a type spelled `Qualified<T, Q>` (with any path prefix).
pub(crate) fn qualified_args(ty: &Type) -> Option<(&Type, &Type)> {
    match generic_args(ty, "Qualified")?.as_slice() {
        [
            GenericArgument::Type(inner),
            GenericArgument::Type(qualifier),
        ] => Some((inner, qualifier)),
        _ => None,
    }
}

/// `T` for a type spelled `Name<T>` (with any path prefix).
fn generic_arg<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    match generic_args(ty, name)?.as_slice() {
        [GenericArgument::Type(inner)] => Some(inner),
        _ => None,
    }
}

fn generic_args<'a>(ty: &'a Type, name: &str) -> Option<Vec<&'a GenericArgument>> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    Some(args.args.iter().collect())
}

/// Whether a return type is spelled `Result<...>` (including aliases like `anyhow::Result<Self>`).
pub(crate) fn is_result(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|s| s.ident == "Result"))
}

/// `[A, B, C]`
pub(crate) fn parse_bracketed<T: Parse>(input: ParseStream) -> syn::Result<Vec<T>> {
    let content;
    bracketed!(content in input);
    Ok(Punctuated::<T, Token![,]>::parse_terminated(&content)?
        .into_iter()
        .collect())
}
