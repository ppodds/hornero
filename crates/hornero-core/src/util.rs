use std::future::Future;
use std::pin::Pin;

/// A boxed, `Send` future. Used wherever hornero needs to store async work behind `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The error type produced by provider factories and lifecycle hooks.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Strips module paths from a `std::any::type_name` string, keeping generics intact.
///
/// `alloc::sync::Arc<dyn my_app::users::UserRepo>` becomes `Arc<dyn UserRepo>`.
pub(crate) fn short_type_name(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut segment = String::new();
    let mut chars = full.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            segment.clear();
        } else if c.is_alphanumeric() || matches!(c, '_' | '{' | '}') {
            // `{{closure}}` is a path segment of items declared inside functions.
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

#[cfg(test)]
mod tests {
    use super::short_type_name;

    #[test]
    fn shortens_paths_but_keeps_generics() {
        assert_eq!(short_type_name("my_app::users::UserService"), "UserService");
        assert_eq!(
            short_type_name("alloc::sync::Arc<dyn my_app::users::UserRepo>"),
            "Arc<dyn UserRepo>"
        );
        assert_eq!(
            short_type_name("hornero_sea_orm::Db<hornero_sea_orm::Primary>"),
            "Db<Primary>"
        );
        assert_eq!(
            short_type_name("(u8, alloc::string::String)"),
            "(u8, String)"
        );
        assert_eq!(
            short_type_name("tests::it_works::{{closure}}::Local"),
            "Local"
        );
    }

    #[test]
    fn keeps_references_arrays_function_types_and_bounds_readable() {
        assert_eq!(short_type_name("&str"), "&str");
        assert_eq!(short_type_name("&mut alloc::vec::Vec<u8>"), "&mut Vec<u8>");
        assert_eq!(short_type_name("[alloc::string::String; 2]"), "[String; 2]");
        assert_eq!(
            short_type_name("fn(&str) -> core::option::Option<u8>"),
            "fn(&str) -> Option<u8>"
        );
        assert_eq!(
            short_type_name("dyn core::error::Error + core::marker::Send + core::marker::Sync"),
            "dyn Error + Send + Sync"
        );
        assert_eq!(
            short_type_name(
                "std::collections::hash::map::HashMap<alloc::string::String, alloc::vec::Vec<my_app::Todo>>"
            ),
            "HashMap<String, Vec<Todo>>"
        );
        assert_eq!(short_type_name("u8"), "u8");
        assert_eq!(short_type_name(""), "");
    }
}
