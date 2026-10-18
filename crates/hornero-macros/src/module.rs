use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, ExprCall, Ident, ItemStruct, Token, Type, parenthesized};

use crate::util::{parse_bracketed, qualified_args};

enum Provider {
    Class(Type),
    /// A `ProviderDef` expression: `value(expr)`, `factory(expr)`, `provider(expr)`,
    /// `bind!(dyn Trait => Impl)` or any other macro, optionally followed by builder calls such as
    /// `.qualified::<Primary>()`.
    Def(Expr),
}

impl Parse for Provider {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let is_def = if input.peek(Ident) && input.peek2(Token![!]) {
            true
        } else if input.peek(Ident) && input.peek2(syn::token::Paren) {
            let ident: Ident = input.fork().parse()?;
            ident == "value" || ident == "factory" || ident == "provider"
        } else {
            false
        };
        if is_def {
            let mut expr: Expr = input.parse()?;
            desugar_root(&mut expr)?;
            return Ok(Provider::Def(expr));
        }
        Ok(Provider::Class(input.parse()?))
    }
}

/// Rewrites the `value(..)`, `factory(..)` or `provider(..)` at the start of a builder chain into
/// the `ProviderDef` expression it stands for.
fn desugar_root(expr: &mut Expr) -> syn::Result<()> {
    match expr {
        Expr::MethodCall(call) => desugar_root(&mut call.receiver),
        Expr::Macro(_) => Ok(()),
        Expr::Call(ExprCall { func, args, .. }) => {
            let Expr::Path(path) = func.as_ref() else {
                return Err(syn::Error::new_spanned(
                    func,
                    "expected `value`, `factory` or `provider`",
                ));
            };
            let ident = path.path.require_ident()?.to_string();
            if args.len() != 1 {
                return Err(syn::Error::new_spanned(
                    &args,
                    format!("`{ident}(..)` takes one expression"),
                ));
            }
            let arg = &args[0];
            *expr = match ident.as_str() {
                "value" => syn::parse_quote!(::hornero::ProviderDef::value(#arg)),
                "factory" => syn::parse_quote!(::hornero::ProviderDef::factory(#arg)),
                _ => arg.clone(),
            };
            Ok(())
        }
        other => Err(syn::Error::new_spanned(
            other,
            "expected `value(..)`, `factory(..)`, `provider(..)` or a macro such as `bind!`",
        )),
    }
}

enum Export {
    /// A provider type, or `Qualified<T, Q>` for the provider of `T` under the qualifier `Q`.
    Provider(Type),
    Module(Type),
}

impl Parse for Export {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Token![mod]) {
            return Err(input.error("use `module(Type)` to re-export a module"));
        }
        if input.peek(Ident) && input.peek2(syn::token::Paren) {
            let fork = input.fork();
            if fork.parse::<Ident>()? == "module" {
                input.parse::<Ident>()?;
                let content;
                parenthesized!(content in input);
                return Ok(Export::Module(content.parse()?));
            }
        }
        Ok(Export::Provider(input.parse()?))
    }
}

#[derive(Default)]
struct ModuleArgs {
    global: bool,
    imports: Vec<Expr>,
    providers: Vec<Provider>,
    controllers: Vec<Type>,
    exports: Vec<Export>,
    middlewares: Vec<Type>,
}

impl Parse for ModuleArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = ModuleArgs::default();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "global" => args.global = true,
                "imports" => {
                    input.parse::<Token![=]>()?;
                    args.imports = parse_bracketed(input)?;
                }
                "providers" => {
                    input.parse::<Token![=]>()?;
                    args.providers = parse_bracketed(input)?;
                }
                "controllers" => {
                    input.parse::<Token![=]>()?;
                    args.controllers = parse_bracketed(input)?;
                }
                "exports" => {
                    input.parse::<Token![=]>()?;
                    args.exports = parse_bracketed(input)?;
                }
                "middlewares" => {
                    input.parse::<Token![=]>()?;
                    args.middlewares = parse_bracketed(input)?;
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "unknown module option; expected `imports`, `providers`, `controllers`, `exports`, `middlewares` or `global`",
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
    let args: ModuleArgs = syn::parse2(args)?;
    let item: ItemStruct = syn::parse2(item)?;

    let global = args.global.then(|| quote!(m.global();));
    let imports = args.imports.iter().map(|import| quote!(m.import(#import);));
    let providers = args.providers.iter().map(|provider| match provider {
        Provider::Class(ty) => quote!(m.provide::<#ty>();),
        Provider::Def(def) => quote!(m.provider(#def);),
    });
    let controllers = args
        .controllers
        .iter()
        .map(|controller| quote!(m.controller::<#controller>();));
    let middlewares = args
        .middlewares
        .iter()
        .map(|middleware| quote!(m.middleware::<#middleware>();));
    let exports = args.exports.iter().map(|export| match export {
        Export::Provider(ty) => match qualified_args(ty) {
            Some((ty, qualifier)) => quote!(m.export_qualified::<#ty, #qualifier>();),
            None => quote!(m.export::<#ty>();),
        },
        Export::Module(ty) => quote!(m.export_module::<#ty>();),
    });

    let name = &item.ident;
    let (impl_generics, ty_generics, where_clause) = item.generics.split_for_impl();
    Ok(quote! {
        #item

        impl #impl_generics ::hornero::Module for #name #ty_generics #where_clause {
            fn define(&self, m: &mut ::hornero::ModuleDef) {
                #global
                #(#imports)*
                #(#providers)*
                #(#controllers)*
                #(#middlewares)*
                #(#exports)*
            }
        }
    })
}
