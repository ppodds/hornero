//! Typed configuration for hornero.
//!
//! Nest's `configService.get('DB_HOST')` becomes plain serde structs injected as `Arc<T>`:
//!
//! ```ignore
//! #[derive(Deserialize)]
//! struct AppConfig { port: u16, database: DbConfig }
//!
//! #[module(imports = [
//!     ConfigModule::for_root(Source::new().toml("config.toml").env("APP_"))
//!         .load::<AppConfig>()
//!         .section::<DbConfig>("database"),
//!     UsersModule,
//! ])]
//! struct AppModule;
//! ```
//!
//! Values are extracted when the application boots, so a missing or malformed key is a bootstrap
//! error naming the type and the key, not a runtime surprise. `for_root` is global by default;
//! feature modules register their own types with `ConfigModule::for_feature()`.

#![warn(missing_docs)]

use std::any::TypeId;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::{Figment, Metadata, Profile, Provider};
use hornero::{BoxError, Module, ModuleDef, ModuleId};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Where configuration comes from. Later sources override earlier ones.
#[derive(Clone, Default)]
pub struct Source {
    figment: Figment,
    /// Files added with [`toml`](Self::toml), reported when extraction fails and they were not found.
    optional_files: Vec<PathBuf>,
}

impl Source {
    /// An empty source.
    pub fn new() -> Self {
        Self::default()
    }

    /// A TOML file, searched for in the working directory and its parents. Missing files are
    /// skipped; if loading a type then fails, the error notes that the file was not found. Use
    /// [`toml_required`](Self::toml_required) when the application cannot run without it.
    pub fn toml(mut self, path: impl AsRef<Path>) -> Self {
        self.optional_files.push(path.as_ref().to_path_buf());
        self.figment = self.figment.merge(Toml::file(path));
        self
    }

    /// Like [`toml`](Self::toml), but a missing file is a bootstrap error naming the file.
    pub fn toml_required(mut self, path: impl AsRef<Path>) -> Self {
        self.figment = self
            .figment
            .merge(RequiredToml(path.as_ref().to_path_buf()));
        self
    }

    /// Inline TOML, handy for defaults and tests.
    pub fn toml_str(mut self, toml: &str) -> Self {
        self.figment = self.figment.merge(Toml::string(toml));
        self
    }

    /// Environment variables starting with `prefix`; `__` separates nested keys
    /// (`APP_DATABASE__URL` sets `database.url`).
    pub fn env(mut self, prefix: &str) -> Self {
        self.figment = self.figment.merge(Env::prefixed(prefix).split("__"));
        self
    }

    /// Defaults from a serializable value, usually placed first.
    pub fn defaults<T: Serialize>(mut self, value: T) -> Self {
        self.figment = self.figment.merge(Serialized::defaults(value));
        self
    }

    /// Any other figment provider.
    pub fn with(mut self, configure: impl FnOnce(Figment) -> Figment) -> Self {
        self.figment = configure(self.figment);
        self
    }
}

/// figment's `Toml::file` search: absolute paths as is, relative ones from the working directory
/// upward. figment does not expose it and silently skips files it cannot find.
fn find(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
}

/// A TOML file that must exist: loads like `Toml::file`, but fails instead of yielding nothing.
struct RequiredToml(PathBuf);

impl Provider for RequiredToml {
    fn metadata(&self) -> Metadata {
        Toml::file(&self.0).metadata()
    }

    fn data(&self) -> Result<figment::value::Map<Profile, figment::value::Dict>, figment::Error> {
        if find(&self.0).is_none() {
            return Err(figment::Error::from(format!(
                "required configuration file `{}` not found (searched from the working directory upward)",
                self.0.display()
            )));
        }
        Toml::file(&self.0).data()
    }
}

/// The merged configuration plus what is needed to explain a failure to load it.
struct Loaded {
    figment: Figment,
    optional_files: Vec<PathBuf>,
}

/// The root's merged source, shared with `for_feature` modules through the container. Only
/// `ConfigModule::for_root` provides it. The name is deliberately a sentence: it is what the
/// missing-dependency error prints when a feature is used without a root.
struct RegisterConfigModuleForRootInTheRootModule(Arc<Loaded>);

/// Registers `Arc<T>` in the root module, extracting straight from the source.
type RegisterRoot = fn(&mut ModuleDef, &Arc<Loaded>, Option<&str>);
/// Registers `Arc<T>` in a feature module, extracting from the root's source.
type RegisterFeature = fn(&mut ModuleDef, Option<&str>);

struct Entry {
    root: RegisterRoot,
    feature: RegisterFeature,
    type_id: TypeId,
    key: Option<String>,
}

impl Entry {
    fn new<T: DeserializeOwned + Send + Sync + 'static>(key: Option<String>) -> Self {
        Self {
            root: register_root::<T>,
            feature: register_feature::<T>,
            type_id: TypeId::of::<T>(),
            key,
        }
    }
}

/// Bit set on a `for_feature` module's discriminator so it can never equal a `for_root` counter value.
const FEATURE_DISCRIMINATOR: u64 = 1 << 63;

/// Source of the unique ids of `for_root` instances. Stored in the module so `validate()` and
/// `build()` (which both call [`Module::id`]) agree.
static NEXT_ROOT_ID: AtomicU64 = AtomicU64::new(1);

/// Provides and exports one `Arc<T>` per configuration type, like Nest's `ConfigModule`.
///
/// [`for_root`](Self::for_root) is registered once in the application's root module and owns the
/// [`Source`]. [`for_feature`](Self::for_feature) lets a feature module register its own typed
/// configuration, read from the root's source.
pub struct ConfigModule {
    /// `Some` for `for_root`; features read the root's source from the container instead.
    loaded: Option<Arc<Loaded>>,
    entries: Vec<Entry>,
    global: bool,
    /// Distinguishes `for_root` instances; unused (derived from the entries) for features.
    root_id: u64,
}

impl ConfigModule {
    /// The configuration root, reading from `source`. Register it once, in the root module. It is
    /// global by default; add types with [`load`](Self::load) and [`section`](Self::section).
    ///
    /// Every call is a distinct module, so registering two roots is reported as an ambiguity
    /// instead of one silently replacing the other.
    pub fn for_root(source: Source) -> Self {
        Self {
            loaded: Some(Arc::new(Loaded {
                figment: source.figment,
                optional_files: source.optional_files,
            })),
            entries: Vec::new(),
            global: true,
            root_id: NEXT_ROOT_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Typed configuration for one feature module, read from the source of
    /// [`for_root`](Self::for_root) (which must be registered, and global). The types are
    /// available to the module that imports this one only.
    pub fn for_feature() -> Self {
        Self {
            loaded: None,
            entries: Vec::new(),
            global: false,
            root_id: 0,
        }
    }

    /// Provides `T`, deserialized from the whole configuration.
    pub fn load<T: DeserializeOwned + Send + Sync + 'static>(mut self) -> Self {
        self.entries.push(Entry::new::<T>(None));
        self
    }

    /// Provides `T`, deserialized from the section at `key` (dotted paths like `"database.pool"` work).
    pub fn section<T: DeserializeOwned + Send + Sync + 'static>(
        mut self,
        key: impl Into<String>,
    ) -> Self {
        self.entries.push(Entry::new::<T>(Some(key.into())));
        self
    }

    /// Makes a `for_root` module non-global: only modules importing it see its types. Features then
    /// cannot find the root's source, so use this only without `for_feature`.
    pub fn local(mut self) -> Self {
        self.global = false;
        self
    }
}

impl Module for ConfigModule {
    fn define(&self, m: &mut ModuleDef) {
        if self.global {
            m.global();
        }
        match &self.loaded {
            Some(loaded) => {
                m.provide_value(RegisterConfigModuleForRootInTheRootModule(loaded.clone()))
                    .export::<RegisterConfigModuleForRootInTheRootModule>();
                for entry in &self.entries {
                    (entry.root)(m, loaded, entry.key.as_deref());
                }
            }
            None => {
                for entry in &self.entries {
                    (entry.feature)(m, entry.key.as_deref());
                }
            }
        }
    }

    fn id(&self) -> ModuleId {
        let discriminator = if self.loaded.is_some() {
            self.root_id
        } else {
            // Identical features de-duplicate; different ones are distinct modules. Sorted, so the
            // order of `load`/`section` calls does not matter.
            let mut hashes: Vec<u64> = self
                .entries
                .iter()
                .map(|entry| {
                    let mut hasher = DefaultHasher::new();
                    (entry.type_id, &entry.key).hash(&mut hasher);
                    hasher.finish()
                })
                .collect();
            hashes.sort_unstable();
            let mut hasher = DefaultHasher::new();
            hashes.hash(&mut hasher);
            hasher.finish() | FEATURE_DISCRIMINATOR
        };
        ModuleId::of::<Self>().with_discriminator(discriminator)
    }
}

fn extract<T: DeserializeOwned>(loaded: &Loaded, key: Option<&str>) -> Result<T, BoxError> {
    let focused = match key {
        Some(key) => loaded.figment.focus(key),
        None => loaded.figment.clone(),
    };
    focused
        .extract::<T>()
        .map_err(|err| describe(err, key, &loaded.optional_files))
}

fn register_root<T: DeserializeOwned + Send + Sync + 'static>(
    m: &mut ModuleDef,
    loaded: &Arc<Loaded>,
    key: Option<&str>,
) {
    let loaded = loaded.clone();
    let key = key.map(str::to_owned);
    m.factory(move || {
        let loaded = loaded.clone();
        let key = key.clone();
        async move { extract::<T>(&loaded, key.as_deref()) }
    })
    .export::<T>();
}

fn register_feature<T: DeserializeOwned + Send + Sync + 'static>(
    m: &mut ModuleDef,
    key: Option<&str>,
) {
    let key = key.map(str::to_owned);
    m.factory(
        move |root: Arc<RegisterConfigModuleForRootInTheRootModule>| {
            let key = key.clone();
            async move { extract::<T>(&root.0, key.as_deref()) }
        },
    )
    .export::<T>();
}

/// `missing field `url` at `database` (TOML file `config.toml`)`, one entry per error.
///
/// figment's own rendering prefixes the profile and, for nested extraction, lists the key path
/// innermost-first; spell the full dotted path out instead.
fn describe(err: figment::Error, section: Option<&str>, optional_files: &[PathBuf]) -> BoxError {
    let problems: Vec<String> = err
        .into_iter()
        .map(|err| {
            let path: Vec<&str> = section
                .into_iter()
                .flat_map(|section| section.split('.'))
                .chain(err.path.iter().map(String::as_str))
                .collect();
            let mut problem = err.kind.to_string();
            if !path.is_empty() {
                problem.push_str(&format!(" at `{}`", path.join(".")));
            }
            if let Some(metadata) = &err.metadata {
                match &metadata.source {
                    Some(source) => problem.push_str(&format!(" ({} `{source}`)", metadata.name)),
                    None => problem.push_str(&format!(" ({})", metadata.name)),
                }
            }
            problem
        })
        .collect();
    let mut message = format!("invalid configuration: {}", problems.join("; "));
    // A wrong working directory only shows up as missing keys; name the files that were skipped.
    for file in optional_files.iter().filter(|file| find(file).is_none()) {
        message.push_str(&format!(
            "; note: `{}` was not found (searched from the working directory upward)",
            file.display()
        ));
    }
    message.into()
}
