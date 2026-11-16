use std::sync::OnceLock;

pub(crate) const CHILD_REALMS_SUPPORTED: bool = !cfg!(obscura_runtime_bootstrap);
pub(crate) const CHILD_REALMS_UNAVAILABLE: &str =
    "child iframe realms are unavailable in runtime-bootstrap builds";

pub(crate) fn source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        include_str!("../js/bootstrap.js")
            .replace("/* INDEXED_DB */", include_str!("../js/indexed_db.js"))
    })
}
