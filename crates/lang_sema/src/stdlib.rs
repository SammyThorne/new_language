//! The standard library modules written in the language (`stdlib/` at the repository root),
//! compiled into the compiler so that a program finds them wherever it is built. Like the
//! prelude, their files have the neutral extension `.lang`, so renaming the language's source
//! extension does not rename them.

use lang_names::SOURCE_EXTENSION;

/// Each module's name and source.
pub const MODULES: &[(&str, &str)] = &[
    ("io", include_str!("../../../stdlib/io.lang")),
    ("os", include_str!("../../../stdlib/os.lang")),
    ("time", include_str!("../../../stdlib/time.lang")),
    ("json", include_str!("../../../stdlib/json.lang")),
    ("log", include_str!("../../../stdlib/log.lang")),
    ("http", include_str!("../../../stdlib/http.lang")),
    ("re", include_str!("../../../stdlib/re.lang")),
];

/// The source of a standard library module.
pub fn source(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(n, _)| *n == name).map(|(_, source)| *source)
}

/// The file name a standard library module has in diagnostics.
pub fn display_name(name: &str) -> String {
    format!("<stdlib>/{}.{SOURCE_EXTENSION}", name.replace('.', "/"))
}
