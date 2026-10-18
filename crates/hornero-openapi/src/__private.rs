//! Support for the code `#[routes]`, `#[controller]` and `#[derive(Injectable)]` generate with
//! hornero's `openapi` feature. Not public API: it changes without notice.
//!
//! The macros only see syntax, so whether a type implements `ToSchema`, `IntoParams` or
//! `ApiSecurity` is decided by autoref specialization on [`Probe`]: the generated code calls a
//! method on `&&Probe<T>` (or `&&&Probe<T>`), and method resolution picks the most specific
//! trait whose bounds `T` meets, falling back to one that only records a warning. A missing impl
//! never fails the build.

use std::any::type_name;
use std::marker::PhantomData;

use hornero_core::Metadata;
use utoipa::openapi::path::{Parameter, ParameterBuilder};
use utoipa::openapi::schema::{
    ArrayBuilder, ObjectBuilder, OneOfBuilder, Schema, SchemaFormat, SchemaType, Type,
};
use utoipa::openapi::{Ref, RefOr, Required};

pub use utoipa::openapi::path::ParameterIn;
pub use utoipa::{IntoParams, PartialSchema, ToSchema};

use crate::security::{ApiSecurity, describe_security};

/// Named schemas, as `ToSchema::schemas` collects them.
pub(crate) type Components = Vec<(String, RefOr<Schema>)>;

/// A schema, the named schemas it refers to and what could not be inferred.
#[derive(Clone)]
pub struct SchemaDoc {
    pub(crate) schema: RefOr<Schema>,
    pub(crate) components: Components,
    pub(crate) warnings: Vec<String>,
}

impl SchemaDoc {
    /// An inline schema that refers to nothing.
    pub fn inline(schema: RefOr<Schema>) -> Self {
        Self {
            schema,
            components: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Any JSON value (`serde_json::Value`, tuples).
    pub fn any() -> Self {
        Self::inline(
            ObjectBuilder::new()
                .schema_type(SchemaType::AnyValue)
                .into(),
        )
    }

    /// A string, optionally in a format such as `uuid` or `date-time`.
    pub fn string(format: Option<&str>) -> Self {
        let format = format.map(|format| SchemaFormat::Custom(format.to_owned()));
        Self::inline(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .format(format)
                .into(),
        )
    }

    /// An array of `items` (`Vec`, slices, sets).
    pub fn array(items: SchemaDoc) -> Self {
        items.wrap(|schema| ArrayBuilder::new().items(schema).into())
    }

    /// `inner` or `null` (`Option`).
    pub fn nullable(inner: SchemaDoc) -> Self {
        inner.wrap(|schema| match schema {
            RefOr::T(Schema::Object(mut object)) => {
                object.schema_type = match object.schema_type {
                    SchemaType::Type(Type::Null) => SchemaType::Type(Type::Null),
                    SchemaType::Type(ty) => [ty, Type::Null].into_iter().collect(),
                    SchemaType::Array(mut types) => {
                        if !types.contains(&Type::Null) {
                            types.push(Type::Null);
                        }
                        SchemaType::Array(types)
                    }
                    SchemaType::AnyValue => SchemaType::AnyValue,
                };
                RefOr::T(Schema::Object(object))
            }
            other => OneOfBuilder::new()
                .item(other)
                .item(ObjectBuilder::new().schema_type(Type::Null))
                .into(),
        })
    }

    /// An object with arbitrary keys whose values are `values` (`HashMap`, `BTreeMap`).
    pub fn map(values: SchemaDoc) -> Self {
        values.wrap(|schema| {
            ObjectBuilder::new()
                .schema_type(Type::Object)
                .additional_properties(Some(schema))
                .into()
        })
    }

    fn wrap(mut self, f: impl FnOnce(RefOr<Schema>) -> RefOr<Schema>) -> Self {
        self.schema = f(self.schema);
        self
    }

    fn unknown(type_name: &str) -> Self {
        let mut doc = Self::any();
        doc.warnings.push(format!(
            "`{}` does not implement `utoipa::ToSchema`, so it is documented as any value; add `#[derive(ToSchema)]` to it",
            short_type_name(type_name)
        ));
        doc
    }
}

/// Parameters, the named schemas they refer to and what could not be inferred.
pub struct ParamsDoc {
    pub(crate) params: Vec<Parameter>,
    pub(crate) components: Components,
    pub(crate) warnings: Vec<String>,
}

/// Selects what `T` can be documented as; see the module documentation.
pub struct Probe<T: ?Sized>(PhantomData<fn() -> PhantomData<T>>);

impl<T: ?Sized> Probe<T> {
    /// The probe for `T`.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: ?Sized> Default for Probe<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Picked for `(&&Probe::<T>::new())` when `T: ToSchema`.
pub trait SchemaProbe {
    /// A `$ref` to `T`'s component schema, registering it and what it refers to.
    fn ref_schema(&self) -> SchemaDoc;
    /// `T`'s schema inline, registering what it refers to. Used for generic types, whose
    /// `ToSchema::name` does not tell instantiations apart.
    fn inline_schema(&self) -> SchemaDoc;
    /// Like [`ref_schema`](SchemaProbe::ref_schema); `fallback` is for types without `ToSchema`.
    fn ref_schema_or(&self, fallback: SchemaDoc) -> SchemaDoc;
}

impl<T: ToSchema + ?Sized> SchemaProbe for &Probe<T> {
    fn ref_schema(&self) -> SchemaDoc {
        let name = T::name().into_owned();
        let mut components = vec![(name.clone(), T::schema())];
        T::schemas(&mut components);
        SchemaDoc {
            schema: Ref::from_schema_name(name).into(),
            components,
            warnings: Vec::new(),
        }
    }

    fn inline_schema(&self) -> SchemaDoc {
        let mut components = Vec::new();
        T::schemas(&mut components);
        SchemaDoc {
            schema: T::schema(),
            components,
            warnings: Vec::new(),
        }
    }

    fn ref_schema_or(&self, _fallback: SchemaDoc) -> SchemaDoc {
        self.ref_schema()
    }
}

/// The fallback of [`SchemaProbe`].
pub trait NoSchemaProbe {
    /// Any value, with a warning.
    fn ref_schema(&self) -> SchemaDoc;
    /// Any value, with a warning.
    fn inline_schema(&self) -> SchemaDoc;
    /// `fallback`, without a warning: a well-known type documented by its name.
    fn ref_schema_or(&self, fallback: SchemaDoc) -> SchemaDoc;
}

impl<T: ?Sized> NoSchemaProbe for Probe<T> {
    fn ref_schema(&self) -> SchemaDoc {
        SchemaDoc::unknown(type_name::<T>())
    }

    fn inline_schema(&self) -> SchemaDoc {
        SchemaDoc::unknown(type_name::<T>())
    }

    fn ref_schema_or(&self, fallback: SchemaDoc) -> SchemaDoc {
        fallback
    }
}

/// Picked for `(&&&Probe::<T>::new())` when `T: IntoParams`.
pub trait ParamsProbe {
    /// `T`'s parameters.
    fn params(&self, location: ParameterIn) -> ParamsDoc;
}

impl<T: IntoParams + ?Sized> ParamsProbe for &&Probe<T> {
    fn params(&self, location: ParameterIn) -> ParamsDoc {
        ParamsDoc {
            params: T::into_params(|| Some(location.clone())),
            components: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Picked for `(&&&Probe::<T>::new())` when `T: ToSchema` but not `IntoParams`: one parameter
/// per property of `T`'s object schema.
pub trait SchemaParamsProbe {
    /// `T`'s properties as parameters.
    fn params(&self, location: ParameterIn) -> ParamsDoc;
}

impl<T: ToSchema + ?Sized> SchemaParamsProbe for &Probe<T> {
    fn params(&self, location: ParameterIn) -> ParamsDoc {
        let mut components = Vec::new();
        T::schemas(&mut components);
        let mut doc = ParamsDoc {
            params: Vec::new(),
            components,
            warnings: Vec::new(),
        };
        let RefOr::T(Schema::Object(object)) = T::schema() else {
            doc.warnings.push(format!(
                "`{}` is extracted as parameters, but its schema is not an object; derive `utoipa::IntoParams` for it",
                short_type_name(type_name::<T>())
            ));
            return doc;
        };
        for (name, schema) in object.properties {
            let required = matches!(location, ParameterIn::Path) || object.required.contains(&name);
            let description = match &schema {
                RefOr::T(Schema::Object(property)) => property.description.clone(),
                _ => None,
            };
            doc.params.push(
                ParameterBuilder::new()
                    .name(name)
                    .parameter_in(location.clone())
                    .required(if required {
                        Required::True
                    } else {
                        Required::False
                    })
                    .description(description)
                    .schema(Some(schema))
                    .build(),
            );
        }
        doc
    }
}

/// The fallback of [`ParamsProbe`] and [`SchemaParamsProbe`].
pub trait NoParamsProbe {
    /// No parameters, with a warning.
    fn params(&self, location: ParameterIn) -> ParamsDoc;
}

impl<T: ?Sized> NoParamsProbe for Probe<T> {
    fn params(&self, _location: ParameterIn) -> ParamsDoc {
        ParamsDoc {
            params: Vec::new(),
            components: Vec::new(),
            warnings: vec![format!(
                "`{}` implements neither `utoipa::IntoParams` nor `utoipa::ToSchema`, so its parameters are not documented; derive one of them",
                short_type_name(type_name::<T>())
            )],
        }
    }
}

/// Picked for `(&&Probe::<T>::new())` when `T: ApiSecurity`.
pub trait SecurityProbe {
    /// Stores `T`'s security scheme in `meta`.
    fn describe_into(&self, meta: &mut Metadata);
}

impl<T: ApiSecurity> SecurityProbe for &Probe<T> {
    fn describe_into(&self, meta: &mut Metadata) {
        describe_security::<T>(meta);
    }
}

/// The fallback of [`SecurityProbe`].
pub trait NoSecurityProbe {
    /// Does nothing.
    fn describe_into(&self, meta: &mut Metadata);
}

impl<T: ?Sized> NoSecurityProbe for Probe<T> {
    fn describe_into(&self, _meta: &mut Metadata) {}
}

/// A request or response body.
pub(crate) struct BodyDoc {
    pub(crate) content_type: String,
    pub(crate) schema: SchemaDoc,
    pub(crate) required: bool,
}

/// One `#[api_response(...)]`.
pub(crate) struct ResponseDoc {
    pub(crate) status: u16,
    pub(crate) description: Option<String>,
    pub(crate) body: Option<BodyDoc>,
}

/// What `#[routes]` inferred from a handler's signature and read from its attributes.
#[derive(Default)]
pub struct OperationDoc {
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) deprecated: bool,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) excluded: bool,
    pub(crate) path: Vec<SchemaDoc>,
    pub(crate) params: Vec<ParamsDoc>,
    pub(crate) body: Option<BodyDoc>,
    pub(crate) returns: Option<BodyDoc>,
    pub(crate) fallible: bool,
    pub(crate) validated: bool,
    pub(crate) responses: Vec<ResponseDoc>,
}

impl OperationDoc {
    /// The first paragraph of the handler's doc comment.
    pub fn summary(&mut self, text: &str) {
        self.summary = Some(text.to_owned());
    }

    /// The rest of the handler's doc comment.
    pub fn description(&mut self, text: &str) {
        self.description = Some(text.to_owned());
    }

    /// `#[deprecated]`.
    pub fn deprecated(&mut self) {
        self.deprecated = true;
    }

    /// `#[api_tags(...)]`.
    pub fn tags(&mut self, tags: &[&str]) {
        self.tags = Some(tags.iter().map(|tag| (*tag).to_owned()).collect());
    }

    /// `#[api_exclude]`.
    pub fn exclude(&mut self) {
        self.excluded = true;
    }

    /// `Path<T>` where `T` is a scalar or a tuple: one schema per path parameter, in order.
    pub fn path(&mut self, schemas: Vec<SchemaDoc>) {
        self.path = schemas;
    }

    /// `Path<T>` or `Query<T>` where `T` is a struct; `required` is `false` under `Option`.
    pub fn params(&mut self, mut params: ParamsDoc, required: bool) {
        if !required {
            for param in &mut params.params {
                param.required = Required::False;
            }
        }
        self.params.push(params);
    }

    /// The request body: `Json<T>`, `Form<T>` or `#[api_body(...)]`, which comes last and wins.
    pub fn body(&mut self, content_type: &str, schema: SchemaDoc, required: bool) {
        self.body = Some(BodyDoc {
            content_type: content_type.to_owned(),
            schema,
            required,
        });
    }

    /// `Valid<E>`: a validation failure is a 422.
    pub fn validated(&mut self) {
        self.validated = true;
    }

    /// The success body inferred from the return type.
    pub fn returns(&mut self, content_type: &str, schema: SchemaDoc) {
        self.returns = Some(BodyDoc {
            content_type: content_type.to_owned(),
            schema,
            required: true,
        });
    }

    /// The handler returns `Result`, so it can fail with any problem response.
    pub fn fallible(&mut self) {
        self.fallible = true;
    }

    /// `#[api_response(status, description = "..", body = T, content_type = "..")]`.
    pub fn response(
        &mut self,
        status: u16,
        description: Option<&str>,
        body: Option<(&str, SchemaDoc)>,
    ) {
        self.responses.push(ResponseDoc {
            status,
            description: description.map(str::to_owned),
            body: body.map(|(content_type, schema)| BodyDoc {
                content_type: content_type.to_owned(),
                schema,
                required: true,
            }),
        });
    }
}

/// Route metadata `#[routes]` adds to every handler: builds its [`OperationDoc`] on demand.
#[derive(Clone, Copy)]
pub struct RouteDoc(fn(&mut OperationDoc));

impl RouteDoc {
    /// Wraps the generated description function.
    pub const fn new(describe: fn(&mut OperationDoc)) -> Self {
        Self(describe)
    }

    pub(crate) fn operation(&self) -> OperationDoc {
        let mut doc = OperationDoc::default();
        (self.0)(&mut doc);
        doc
    }
}

/// Controller metadata from `#[api_response(...)]`, `#[api_tags(...)]` and `#[api_exclude]`
/// below `#[controller]`.
#[derive(Clone, Copy)]
pub struct ControllerDoc {
    pub(crate) tags: Option<&'static [&'static str]>,
    pub(crate) excluded: bool,
    pub(crate) responses: Option<fn(&mut OperationDoc)>,
}

impl ControllerDoc {
    /// The controller's tags, if given, whether its routes are left out, and the responses
    /// every route has.
    pub const fn new(
        tags: Option<&'static [&'static str]>,
        excluded: bool,
        responses: Option<fn(&mut OperationDoc)>,
    ) -> Self {
        Self {
            tags,
            excluded,
            responses,
        }
    }
}

/// Strips module paths from a `std::any::type_name` string, keeping generics intact.
pub(crate) fn short_type_name(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut segment = String::new();
    let mut chars = full.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            segment.clear();
        } else if c.is_alphanumeric() || c == '_' {
            segment.push(c);
        } else {
            out.push_str(&segment);
            segment.clear();
            out.push(c);
        }
    }
    out.push_str(&segment);
    out
}
