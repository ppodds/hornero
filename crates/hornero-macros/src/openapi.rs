//! OpenAPI descriptions: `#[routes]` infers one per handler from its signature and doc comment,
//! `#[controller]` records controller-wide tags, and `#[derive(Injectable)]` lets a guard record
//! its security scheme. The `api_*` attributes are parsed and removed whether or not the
//! `openapi` feature is on, so code using them compiles either way; code is only emitted with it.
//!
//! The macros only see syntax. Whether a type implements `ToSchema`, `IntoParams` or `ApiSecurity`
//! is left to the autoref probes in `hornero_openapi::__private`, so a missing impl becomes a warning
//! in the document instead of a build error.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, FnArg, GenericArgument, Ident, ImplItemFn, Lit, LitInt, LitStr, Meta,
    PathArguments, ReturnType, Token, Type,
};

/// Attributes `#[routes]` reads on a route method.
pub(crate) const ROUTE_ATTRIBUTES: &[&str] =
    &["api_response", "api_body", "api_tags", "api_exclude"];

/// Attributes `#[controller]` reads below itself.
pub(crate) const CONTROLLER_ATTRIBUTES: &[&str] = &["api_response", "api_tags", "api_exclude"];

fn enabled() -> bool {
    cfg!(feature = "openapi")
}

fn private() -> TokenStream {
    quote!(::hornero::openapi::__private)
}

// --- attributes ------------------------------------------------------------------------------

/// `#[api_response(404, description = "..", body = T, content_type = "..")]`
struct ApiResponse {
    status: u16,
    description: Option<LitStr>,
    body: Option<Type>,
    content_type: Option<LitStr>,
}

impl Parse for ApiResponse {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let status: LitInt = input.parse()?;
        let value: u16 = status.base10_parse()?;
        if !(100..=599).contains(&value) {
            return Err(syn::Error::new(
                status.span(),
                "expected an HTTP status code between 100 and 599",
            ));
        }
        let mut response = ApiResponse {
            status: value,
            description: None,
            body: None,
            content_type: None,
        };
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "description" => response.description = Some(input.parse()?),
                "body" => response.body = Some(input.parse()?),
                "content_type" => response.content_type = Some(input.parse()?),
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "unknown option; expected `description = \"..\"`, `body = Type` or `content_type = \"..\"`",
                    ));
                }
            }
        }
        if let (Some(content_type), None) = (&response.content_type, &response.body) {
            return Err(syn::Error::new_spanned(
                content_type,
                "`content_type` needs a `body = Type`",
            ));
        }
        Ok(response)
    }
}

/// `#[api_body(T)]` or `#[api_body(T, content_type = "..")]`
struct ApiBody {
    ty: Type,
    content_type: Option<LitStr>,
}

impl Parse for ApiBody {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ty = input.parse()?;
        let mut content_type = None;
        if input.parse::<Option<Token![,]>>()?.is_some() && !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "content_type" {
                return Err(syn::Error::new_spanned(
                    key,
                    "unknown option; expected `content_type = \"..\"`",
                ));
            }
            input.parse::<Token![=]>()?;
            content_type = Some(input.parse()?);
            input.parse::<Option<Token![,]>>()?;
        }
        Ok(ApiBody { ty, content_type })
    }
}

/// What the `api_*` attributes on a route method say.
#[derive(Default)]
pub(crate) struct RouteApi {
    responses: Vec<ApiResponse>,
    body: Option<ApiBody>,
    tags: Option<Vec<LitStr>>,
    exclude: bool,
}

impl RouteApi {
    /// Reads one of [`ROUTE_ATTRIBUTES`].
    pub(crate) fn read(&mut self, name: &str, attr: &Attribute) -> syn::Result<()> {
        match name {
            "api_response" => self.responses.push(attr.parse_args()?),
            "api_body" => {
                if self.body.is_some() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "a handler has one request body",
                    ));
                }
                self.body = Some(attr.parse_args()?);
            }
            "api_tags" => self
                .tags
                .get_or_insert_with(Vec::new)
                .extend(parse_tags(attr)?),
            _ => {
                expect_no_args(attr)?;
                self.exclude = true;
            }
        }
        Ok(())
    }
}

/// What `#[api_response]`, `#[api_tags]` and `#[api_exclude]` below `#[controller]` say.
#[derive(Default)]
pub(crate) struct ControllerApi {
    responses: Vec<ApiResponse>,
    tags: Option<Vec<LitStr>>,
    exclude: bool,
}

impl ControllerApi {
    /// Reads one of [`CONTROLLER_ATTRIBUTES`].
    pub(crate) fn read(&mut self, name: &str, attr: &Attribute) -> syn::Result<()> {
        match name {
            "api_response" => self.responses.push(attr.parse_args()?),
            "api_tags" => self
                .tags
                .get_or_insert_with(Vec::new)
                .extend(parse_tags(attr)?),
            _ => {
                expect_no_args(attr)?;
                self.exclude = true;
            }
        }
        Ok(())
    }

    /// `c.meta(ControllerDoc)` for `ControllerAttrs::configure`, if there is anything to say.
    pub(crate) fn configure(self) -> TokenStream {
        if !enabled() || (self.responses.is_empty() && self.tags.is_none() && !self.exclude) {
            return TokenStream::new();
        }
        let private = private();
        let tags = match &self.tags {
            Some(tags) => quote!(::core::option::Option::Some(&[#(#tags),*])),
            None => quote!(::core::option::Option::None),
        };
        let exclude = self.exclude;
        let responses = if self.responses.is_empty() {
            quote!(::core::option::Option::None)
        } else {
            let statements = self.responses.into_iter().map(response);
            quote! {{
                fn responses(__doc: &mut #private::OperationDoc) {
                    #[allow(unused_imports)]
                    use #private::{NoSchemaProbe as _, SchemaProbe as _};
                    #(#statements)*
                }
                ::core::option::Option::Some(responses as fn(&mut #private::OperationDoc))
            }}
        };
        quote!(c.meta(#private::ControllerDoc::new(#tags, #exclude, #responses));)
    }
}

fn parse_tags(attr: &Attribute) -> syn::Result<Vec<LitStr>> {
    Ok(attr
        .parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)?
        .into_iter()
        .collect())
}

fn expect_no_args(attr: &Attribute) -> syn::Result<()> {
    match &attr.meta {
        Meta::Path(_) => Ok(()),
        _ => Err(syn::Error::new_spanned(
            attr,
            "`#[api_exclude]` takes no arguments",
        )),
    }
}

// --- `#[derive(Injectable)]` -----------------------------------------------------------------

/// `Injectable::describe`, recording the type's `ApiSecurity` scheme if it has one.
pub(crate) fn describe() -> TokenStream {
    if !enabled() {
        return TokenStream::new();
    }
    let private = private();
    quote! {
        fn describe(meta: &mut ::hornero::Metadata) {
            #[allow(unused_imports)]
            use #private::{NoSecurityProbe as _, SecurityProbe as _};
            (&&#private::Probe::<Self>::new()).describe_into(meta);
        }
    }
}

// --- `#[routes]` -----------------------------------------------------------------------------

/// `.meta(RouteDoc)` for one handler, or nothing without the feature.
pub(crate) fn route_doc(method: &ImplItemFn, api: RouteApi) -> TokenStream {
    if !enabled() {
        return TokenStream::new();
    }
    let private = private();
    let mut statements = Vec::new();

    let (summary, description) = doc_comment(&method.attrs);
    if let Some(summary) = summary {
        statements.push(quote!(__doc.summary(#summary);));
    }
    if let Some(description) = description {
        statements.push(quote!(__doc.description(#description);));
    }
    if method
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("deprecated"))
    {
        statements.push(quote!(__doc.deprecated();));
    }

    for input in method.sig.inputs.iter().skip(1) {
        if let FnArg::Typed(arg) = input {
            describe_argument(&arg.ty, true, &mut statements);
        }
    }
    if let ReturnType::Type(_, ty) = &method.sig.output {
        describe_return(ty, &mut statements);
    }

    let RouteApi {
        responses,
        body,
        tags,
        exclude,
    } = api;
    if let Some(ApiBody { ty, content_type }) = body {
        let content_type =
            content_type.map_or_else(|| quote!("application/json"), |ct| quote!(#ct));
        let schema = schema(&ty);
        statements.push(quote!(__doc.body(#content_type, #schema, true);));
    }
    statements.extend(responses.into_iter().map(response));
    if let Some(tags) = tags {
        statements.push(quote!(__doc.tags(&[#(#tags),*]);));
    }
    if exclude {
        statements.push(quote!(__doc.exclude();));
    }

    quote! {
        .meta(#private::RouteDoc::new(|__doc: &mut #private::OperationDoc| {
            #[allow(unused_imports)]
            use #private::{
                NoParamsProbe as _, NoSchemaProbe as _, ParamsProbe as _, SchemaParamsProbe as _,
                SchemaProbe as _,
            };
            #(#statements)*
        }))
    }
}

/// `__doc.response(..)` for one `#[api_response(...)]`.
fn response(response: ApiResponse) -> TokenStream {
    let ApiResponse {
        status,
        description,
        body,
        content_type,
    } = response;
    let description = match description {
        Some(description) => quote!(::core::option::Option::Some(#description)),
        None => quote!(::core::option::Option::None),
    };
    let body = match body {
        Some(ty) => {
            let content_type =
                content_type.map_or_else(|| quote!("application/json"), |ct| quote!(#ct));
            let schema = schema(&ty);
            quote!(::core::option::Option::Some((#content_type, #schema)))
        }
        None => quote!(::core::option::Option::None),
    };
    quote!(__doc.response(#status, #description, #body);)
}

/// The summary (first paragraph) and description (the rest) of a doc comment.
fn doc_comment(attrs: &[Attribute]) -> (Option<String>, Option<String>) {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(text),
                    ..
                }) => Some(text.value()),
                _ => None,
            },
            _ => None,
        })
        // `split`, not `lines`: an empty `///` line is `#[doc = ""]`, which `lines` would drop.
        .flat_map(|text| {
            text.split('\n')
                .map(|line| line.strip_prefix(' ').unwrap_or(line).to_owned())
                .collect::<Vec<_>>()
        })
        .collect();

    let mut lines = lines
        .iter()
        .map(|line| line.trim_end())
        .skip_while(|line| line.trim().is_empty());
    let summary: Vec<&str> = lines
        .by_ref()
        .take_while(|line| !line.trim().is_empty())
        .map(str::trim)
        .collect();
    let rest: Vec<&str> = lines.collect();
    let summary = (!summary.is_empty()).then(|| summary.join(" "));
    let description = rest.join("\n").trim().to_owned();
    (summary, (!description.is_empty()).then_some(description))
}

/// Records what an extractor argument says about the request.
fn describe_argument(ty: &Type, required: bool, out: &mut Vec<TokenStream>) {
    let ty = peel(ty);
    let Some((name, args)) = last_segment(ty) else {
        return;
    };
    let private = private();
    match name.as_str() {
        "Valid" => {
            out.push(quote!(__doc.validated();));
            if let Some(inner) = args.first() {
                describe_argument(inner, required, out);
            }
        }
        "Option" => {
            if let Some(inner) = args.first() {
                describe_argument(inner, false, out);
            }
        }
        "Json" | "Form" => {
            if let Some(inner) = args.first() {
                let content_type = if name == "Json" {
                    "application/json"
                } else {
                    "application/x-www-form-urlencoded"
                };
                let schema = schema(inner);
                out.push(quote!(__doc.body(#content_type, #schema, #required);));
            }
        }
        "Query" => {
            if let Some(inner) = args.first() {
                let params = params(inner, quote!(#private::ParameterIn::Query));
                out.push(quote!(__doc.params(#params, #required);));
            }
        }
        "Path" => {
            let Some(inner) = args.first().map(|inner| peel(inner)) else {
                return;
            };
            match inner {
                Type::Tuple(tuple) => {
                    let schemas = tuple.elems.iter().map(schema);
                    out.push(quote!(__doc.path(::std::vec![#(#schemas),*]);));
                }
                _ if is_scalar(inner) => {
                    let schema = schema(inner);
                    out.push(quote!(__doc.path(::std::vec![#schema]);));
                }
                _ if is_named(inner) => {
                    let params = params(inner, quote!(#private::ParameterIn::Path));
                    out.push(quote!(__doc.params(#params, true);));
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Records what a return type says about the success response.
fn describe_return(ty: &Type, out: &mut Vec<TokenStream>) {
    let private = private();
    match peel(ty) {
        Type::Tuple(tuple) => {
            if let Some(last) = tuple.elems.last() {
                describe_return(last, out);
            }
        }
        Type::Reference(reference) if is_ident(&reference.elem, "str") => {
            out.push(quote!(__doc.returns("text/plain", #private::SchemaDoc::string(::core::option::Option::None));));
        }
        ty => {
            let Some((name, args)) = last_segment(ty) else {
                return;
            };
            match name.as_str() {
                "Result" => {
                    out.push(quote!(__doc.fallible();));
                    if let Some(ok) = args.first() {
                        describe_return(ok, out);
                    }
                }
                "Json" => {
                    if let Some(inner) = args.first() {
                        let schema = schema(inner);
                        out.push(quote!(__doc.returns("application/json", #schema);));
                    }
                }
                "String" if args.is_empty() => {
                    out.push(quote!(__doc.returns("text/plain", #private::SchemaDoc::string(::core::option::Option::None));));
                }
                "Html" => {
                    out.push(quote!(__doc.returns("text/html", #private::SchemaDoc::string(::core::option::Option::None));));
                }
                _ => {}
            }
        }
    }
}

/// A `SchemaDoc` expression for `ty`. Containers are unwrapped syntactically so the named types
/// inside them become `$ref`s; the leaves go through the probe.
fn schema(ty: &Type) -> TokenStream {
    let private = private();
    let ty = peel(ty);
    match ty {
        Type::Reference(reference) => schema(&reference.elem),
        Type::Slice(slice) => {
            let items = schema(&slice.elem);
            quote!(#private::SchemaDoc::array(#items))
        }
        Type::Array(array) => {
            let items = schema(&array.elem);
            quote!(#private::SchemaDoc::array(#items))
        }
        Type::Path(path) if path.qself.is_none() => {
            let Some((name, args)) = last_segment(ty) else {
                return any();
            };
            match (name.as_str(), args.as_slice()) {
                (
                    "Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "IndexSet",
                    [items, ..],
                ) => {
                    let items = schema(items);
                    quote!(#private::SchemaDoc::array(#items))
                }
                ("Option", [inner]) => {
                    let inner = schema(inner);
                    quote!(#private::SchemaDoc::nullable(#inner))
                }
                ("Box" | "Arc" | "Rc" | "Cow", [inner]) => schema(inner),
                ("HashMap" | "BTreeMap" | "IndexMap", [_, values, ..]) => {
                    let values = schema(values);
                    quote!(#private::SchemaDoc::map(#values))
                }
                ("Value", []) => any(),
                _ if is_primitive(&name) && args.is_empty() => {
                    quote!((&&#private::Probe::<#ty>::new()).inline_schema())
                }
                _ => match known_format(&name).filter(|_| args.is_empty()) {
                    // A type named like a well-known one is documented as that type, unless it
                    // implements `ToSchema` itself.
                    Some(format) => quote!((&&#private::Probe::<#ty>::new()).ref_schema_or(
                        #private::SchemaDoc::string(#format)
                    )),
                    // Generic instantiations share one `ToSchema::name`, so they are inlined.
                    None if !args.is_empty() => {
                        quote!((&&#private::Probe::<#ty>::new()).inline_schema())
                    }
                    None => quote!((&&#private::Probe::<#ty>::new()).ref_schema()),
                },
            }
        }
        _ => any(),
    }
}

/// A `ParamsDoc` expression for the struct `ty` extracted from `location`.
fn params(ty: &Type, location: TokenStream) -> TokenStream {
    let private = private();
    quote!((&&&#private::Probe::<#ty>::new()).params(#location))
}

fn any() -> TokenStream {
    let private = private();
    quote!(#private::SchemaDoc::any())
}

fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "String"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    )
}

/// The string format of well-known types that do not implement `ToSchema`.
fn known_format(name: &str) -> Option<TokenStream> {
    let format = match name {
        "Uuid" => Some("uuid"),
        "Ulid" => Some("ulid"),
        "Url" => Some("uri"),
        "DateTime" | "NaiveDateTime" | "OffsetDateTime" | "PrimitiveDateTime" => Some("date-time"),
        "NaiveDate" => Some("date"),
        "NaiveTime" => Some("time"),
        "Decimal" | "BigDecimal" => None,
        _ => return None,
    };
    Some(match format {
        Some(format) => quote!(::core::option::Option::Some(#format)),
        None => quote!(::core::option::Option::None),
    })
}

/// A single path parameter: a primitive or a well-known string type.
fn is_scalar(ty: &Type) -> bool {
    last_segment(ty).is_some_and(|(name, args)| {
        args.is_empty() && (is_primitive(&name) || known_format(&name).is_some())
    })
}

/// A plain or generic named type, such as `Params` or `Page<Todo>`.
fn is_named(ty: &Type) -> bool {
    last_segment(ty).is_some_and(|(name, _)| {
        !matches!(
            name.as_str(),
            "Vec" | "Option" | "HashMap" | "BTreeMap" | "Value"
        )
    })
}

fn is_ident(ty: &Type, ident: &str) -> bool {
    last_segment(peel(ty)).is_some_and(|(name, args)| name == ident && args.is_empty())
}

/// Removes parentheses and invisible groups.
fn peel(ty: &Type) -> &Type {
    match ty {
        Type::Paren(paren) => peel(&paren.elem),
        Type::Group(group) => peel(&group.elem),
        ty => ty,
    }
}

/// The last path segment's name and type arguments.
fn last_segment(ty: &Type) -> Option<(String, Vec<&Type>)> {
    let Type::Path(path) = peel(ty) else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    let segment = path.path.segments.last()?;
    let args = match &segment.arguments {
        PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|arg| match arg {
                GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Some((segment.ident.to_string(), args))
}

/// The error for an `api_*` attribute left over outside `#[routes]` or `#[controller]`.
pub(crate) fn misplaced(name: &str, item: TokenStream) -> TokenStream {
    if CONTROLLER_ATTRIBUTES.contains(&name) {
        return crate::enhancer::misplaced(name, item);
    }
    let message =
        format!("`#[{name}(...)]` goes on a route method of an impl block marked `#[routes]`");
    let error = syn::Error::new(Span::call_site(), message).into_compile_error();
    quote!(#error #item)
}
