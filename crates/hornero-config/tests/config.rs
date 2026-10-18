#![allow(
    clippy::result_large_err,
    reason = "figment::Jail closures return figment::Error"
)]

use figment::Jail;
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, PartialEq)]
struct AppConfig {
    name: String,
    port: u16,
    database: DbConfig,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Clone)]
struct DbConfig {
    url: String,
    pool: u32,
}

/// Lives in a module that does not import `ConfigModule`: the module is global.
#[derive(Injectable)]
struct Consumer {
    db: Arc<DbConfig>,
}

#[module(providers = [Consumer], exports = [Consumer])]
struct ConsumerModule;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn loads_files_sections_and_environment_overrides() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "config.toml",
            r#"
                name = "todo"
                port = 3000

                [database]
                url = "sqlite::memory:"
                pool = 5
            "#,
        )?;
        jail.set_env("APP_PORT", "8080");
        jail.set_env("APP_DATABASE__POOL", "20");

        struct App;
        impl Module for App {
            fn define(&self, m: &mut ModuleDef) {
                m.import(
                    ConfigModule::for_root(Source::new().toml("config.toml").env("APP_"))
                        .load::<AppConfig>()
                        .section::<DbConfig>("database"),
                )
                .import(ConsumerModule);
            }
        }

        let app = block_on(Hornero::new(App).build()).unwrap();
        let config = app.get::<AppConfig>().unwrap();
        assert_eq!(config.name, "todo");
        assert_eq!(config.port, 8080, "environment overrides the file");
        assert_eq!(config.database.pool, 20, "`__` separates nested keys");
        assert_eq!(app.get::<Consumer>().unwrap().db.url, "sqlite::memory:");
        Ok(())
    });
}

#[test]
fn defaults_and_inline_toml() {
    let source = Source::new()
        .defaults(DbConfig {
            url: "postgres://localhost".into(),
            pool: 1,
        })
        .toml_str("pool = 9");
    struct App(Source);
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(ConfigModule::for_root(self.0.clone()).load::<DbConfig>());
        }
    }
    let app = block_on(Hornero::new(App(source)).build()).unwrap();
    assert_eq!(
        *app.get::<DbConfig>().unwrap(),
        DbConfig {
            url: "postgres://localhost".into(),
            pool: 9
        }
    );
}

#[test]
fn invalid_configuration_is_a_bootstrap_error() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(
                ConfigModule::for_root(Source::new().toml_str("name = \"x\"")).load::<AppConfig>(),
            );
        }
    }
    let err = block_on(Hornero::new(App).build())
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("AppConfig (in ConfigModule) failed to construct"),
        "{err}"
    );
    assert!(err.contains("missing field `port`"), "{err}");
}

#[test]
fn local_config_needs_an_import() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(
                ConfigModule::for_root(Source::new().toml_str("[database]\nurl = \"x\"\npool = 1"))
                    .section::<DbConfig>("database")
                    .local(),
            )
            .import(ConsumerModule);
        }
    }
    let err = Hornero::new(App).validate().unwrap_err().to_string();
    assert!(
        err.contains(
            "`DbConfig` is exported by ConfigModule; add it to the imports of ConsumerModule"
        ),
        "{err}"
    );
}

#[test]
fn errors_spell_out_the_full_key_path() {
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Pool {
        size: u32,
    }

    let source = Source::new().toml_str("[database.pool]\nsize = \"big\"");
    let err = block_on(
        Hornero::new(ConfigModule::for_root(source).section::<Pool>("database.pool")).build(),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        err.contains(
            "Pool (in ConfigModule) failed to construct: invalid configuration: invalid type: found string \"big\", expected u32 at `database.pool.size` (TOML source string)"
        ),
        "{err}"
    );

    let err = block_on(
        Hornero::new(ConfigModule::for_root(Source::new()).section::<Pool>("database.pool"))
            .build(),
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        err.contains("missing field `size` at `database.pool`"),
        "{err}"
    );
}

fn boot_error(source: Source) -> String {
    struct App(Source);
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(ConfigModule::for_root(self.0.clone()).load::<DbConfig>());
        }
    }
    block_on(Hornero::new(App(source)).build())
        .err()
        .unwrap()
        .to_string()
}

#[test]
fn a_missing_required_file_is_a_bootstrap_error_naming_it() {
    Jail::expect_with(|_| {
        let err = boot_error(
            Source::new()
                .toml_str("url = \"x\"\npool = 1")
                .toml_required("config.toml"),
        );
        assert!(
            err.contains("required configuration file `config.toml` not found"),
            "{err}"
        );
        Ok(())
    });
}

#[test]
fn a_present_required_file_is_found_from_a_subdirectory() {
    Jail::expect_with(|jail| {
        jail.create_file("config.toml", "url = \"x\"\npool = 1")?;
        jail.create_dir("sub")?;
        jail.change_dir("sub")?;
        struct App;
        impl Module for App {
            fn define(&self, m: &mut ModuleDef) {
                m.import(
                    ConfigModule::for_root(Source::new().toml_required("config.toml"))
                        .load::<DbConfig>(),
                );
            }
        }
        let app = block_on(Hornero::new(App).build()).unwrap();
        assert_eq!(app.get::<DbConfig>().unwrap().pool, 1);
        Ok(())
    });
}

#[test]
fn a_failure_notes_optional_files_that_were_not_found() {
    Jail::expect_with(|jail| {
        let err = boot_error(Source::new().toml("config.toml"));
        assert!(err.contains("missing field"), "{err}");
        assert!(err.contains("`config.toml` was not found"), "{err}");

        // Found files are not blamed.
        jail.create_file("config.toml", "url = \"x\"")?;
        let err = boot_error(Source::new().toml("config.toml"));
        assert!(err.contains("missing field `pool`"), "{err}");
        assert!(!err.contains("was not found"), "{err}");
        Ok(())
    });
}

#[derive(Deserialize)]
struct AConf {
    a: u32,
}

#[derive(Deserialize)]
struct BConf {
    b: u32,
}

#[derive(Injectable)]
struct AConsumer {
    conf: Arc<AConf>,
}

#[derive(Injectable)]
struct BConsumer {
    conf: Arc<BConf>,
}

#[module(
    imports = [ConfigModule::for_feature().load::<AConf>()],
    providers = [AConsumer],
    exports = [AConsumer],
)]
struct FeatureA;

#[module(
    imports = [ConfigModule::for_feature().load::<BConf>()],
    providers = [BConsumer],
    exports = [BConsumer],
)]
struct FeatureB;

#[test]
fn feature_modules_read_their_own_types_from_the_root_source() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(ConfigModule::for_root(
                Source::new().toml_str("a = 1\nb = 2"),
            ))
            .import(FeatureA)
            .import(FeatureB);
        }
    }
    let app = block_on(Hornero::new(App).build()).unwrap();
    assert_eq!(app.get::<AConsumer>().unwrap().conf.a, 1);
    assert_eq!(app.get::<BConsumer>().unwrap().conf.b, 2);
}

#[test]
fn a_feature_without_a_root_says_to_register_for_root() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(FeatureA);
        }
    }
    let err = Hornero::new(App).validate().unwrap_err().to_string();
    assert!(
        err.contains("RegisterConfigModuleForRootInTheRootModule"),
        "{err}"
    );
}

#[test]
fn two_roots_are_ambiguous_rather_than_collapsed() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(ConfigModule::for_root(Source::new().toml_str("a = 1")))
                .import(ConfigModule::for_root(Source::new().toml_str("a = 2")))
                .import(FeatureA);
        }
    }
    let err = Hornero::new(App).validate().unwrap_err().to_string();
    assert!(err.contains("ambiguous"), "{err}");
}
