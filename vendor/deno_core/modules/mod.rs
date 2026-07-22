// Copyright 2018-2025 the Deno authors. MIT license.

use crate::FastStaticString;
use crate::error::CoreError;
use crate::error::exception_to_err_result;
use crate::fast_string::FastString;
use crate::module_specifier::ModuleSpecifier;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use url::Url;

mod loaders;
mod map;
mod module_map_data;
mod recursive_load;

#[cfg(all(test, not(miri)))]
mod tests;

pub use loaders::ExtCodeCache;
pub(crate) use loaders::ExtModuleLoader;
pub use loaders::FsModuleLoader;
pub(crate) use loaders::LazyEsmModuleLoader;
pub use loaders::ModuleLoadResponse;
pub use loaders::ModuleLoader;
pub use loaders::ModuleLoaderError;
pub use loaders::NoopModuleLoader;
pub use loaders::StaticModuleLoader;
pub(crate) use map::ModuleMap;
pub(crate) use map::script_origin;
pub(crate) use map::synthetic_module_evaluation_steps;
pub(crate) use module_map_data::ModuleMapSnapshotData;

pub type ModuleId = usize;
pub(crate) type ModuleLoadId = i32;

/// The actual source code returned from the loader. Most embedders should
/// try to return bytes and let deno_core interpret if the module should be
/// converted to a string or not.
#[derive(Debug, Hash, PartialEq, Eq)]
pub enum ModuleSourceCode {
  String(ModuleCodeString),
  Bytes(ModuleCodeBytes),
}

impl ModuleSourceCode {
  pub fn as_bytes(&self) -> &[u8] {
    match self {
      Self::String(s) => s.as_bytes(),
      Self::Bytes(b) => b.as_bytes(),
    }
  }
}

pub type ModuleCodeString = FastString;
pub type ModuleName = FastString;

/// Converts various string-like things into `ModuleName`.
pub trait IntoModuleName {
  fn into_module_name(self) -> ModuleName;
}

impl IntoModuleName for ModuleName {
  fn into_module_name(self) -> ModuleName {
    self
  }
}

impl IntoModuleName for &'static str {
  fn into_module_name(self) -> ModuleName {
    ModuleName::from_static(self)
  }
}

impl IntoModuleName for String {
  fn into_module_name(self) -> ModuleName {
    ModuleName::from(self)
  }
}

impl IntoModuleName for Url {
  fn into_module_name(self) -> ModuleName {
    ModuleName::from(self)
  }
}

impl IntoModuleName for FastStaticString {
  fn into_module_name(self) -> ModuleName {
    ModuleName::from(self)
  }
}

/// Converts various string-like things into `ModuleCodeString`.
pub trait IntoModuleCodeString {
  fn into_module_code(self) -> ModuleCodeString;
}

impl IntoModuleCodeString for ModuleCodeString {
  fn into_module_code(self) -> ModuleCodeString {
    self
  }
}

impl IntoModuleCodeString for &'static str {
  fn into_module_code(self) -> ModuleCodeString {
    ModuleCodeString::from_static(self)
  }
}

impl IntoModuleCodeString for String {
  fn into_module_code(self) -> ModuleCodeString {
    ModuleCodeString::from(self)
  }
}

impl IntoModuleCodeString for FastStaticString {
  fn into_module_code(self) -> ModuleCodeString {
    ModuleCodeString::from(self)
  }
}

impl IntoModuleCodeString for Arc<str> {
  fn into_module_code(self) -> ModuleCodeString {
    ModuleCodeString::from(self)
  }
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub enum ModuleCodeBytes {
  /// Created from static data.
  Static(&'static [u8]),

  /// An owned chunk of data. Note that we use `Box` rather than `Vec` to avoid
  /// the storage overhead.
  Boxed(Box<[u8]>),

  /// Code loaded from the `deno_graph` infrastructure.
  Arc(Arc<[u8]>),
}

impl ModuleCodeBytes {
  pub fn as_bytes(&self) -> &[u8] {
    match self {
      ModuleCodeBytes::Static(s) => s,
      ModuleCodeBytes::Boxed(s) => s,
      ModuleCodeBytes::Arc(s) => s,
    }
  }

  pub fn to_vec(&self) -> Vec<u8> {
    match self {
      ModuleCodeBytes::Static(s) => s.to_vec(),
      ModuleCodeBytes::Boxed(s) => s.to_vec(),
      ModuleCodeBytes::Arc(s) => s.to_vec(),
    }
  }
}

impl From<Arc<[u8]>> for ModuleCodeBytes {
  fn from(value: Arc<[u8]>) -> Self {
    Self::Arc(value)
  }
}

impl From<Box<[u8]>> for ModuleCodeBytes {
  fn from(value: Box<[u8]>) -> Self {
    Self::Boxed(value)
  }
}

impl From<&'static [u8]> for ModuleCodeBytes {
  fn from(value: &'static [u8]) -> Self {
    Self::Static(value)
  }
}

/// Callback to customize value of `import.meta.resolve("./foo.ts")`.
pub type ImportMetaResolveCallback = Box<
  dyn Fn(
    &dyn ModuleLoader,
    String,
    String,
  ) -> Result<ModuleSpecifier, ModuleLoaderError>,
>;

pub(crate) fn default_import_meta_resolve_cb(
  loader: &dyn ModuleLoader,
  specifier: String,
  referrer: String,
) -> Result<ModuleSpecifier, ModuleLoaderError> {
  if specifier.starts_with("npm:") {
    return Err(ModuleLoaderError::NpmUnsupportedMetaResolve);
  }

  loader.resolve(&specifier, &referrer, ResolutionKind::DynamicImport)
}

/// Callback to validate import attributes. If the validation fails and exception
/// should be thrown using `scope.throw_exception()`.
pub type ValidateImportAttributesCb =
  Box<dyn Fn(&mut v8::HandleScope, &HashMap<String, String>)>;

/// Callback to validate import attributes. If the validation fails and exception
/// should be thrown using `scope.throw_exception()`.
pub type CustomModuleEvaluationCb = Box<
  dyn Fn(
    &mut v8::HandleScope,
    Cow<'_, str>,
    &FastString,
    ModuleSourceCode,
  ) -> Result<CustomModuleEvaluationKind, deno_error::JsErrorBox>,
>;

/// A callback to get the code cache for a script.
/// (specifier, code) -> ...
pub type EvalContextGetCodeCacheCb = Box<
  dyn Fn(
    &Url,
    &v8::String,
  ) -> Result<SourceCodeCacheInfo, deno_error::JsErrorBox>,
>;

/// Callback when the code cache is ready.
/// (specifier, hash, data) -> ()
pub type EvalContextCodeCacheReadyCb = Box<dyn Fn(Url, u64, &[u8])>;

pub enum CustomModuleEvaluationKind {
  /// This evaluation results in a single, "synthetic" module.
  Synthetic(v8::Global<v8::Value>),

  /// This evaluation results in creation of two modules:
  ///  - a "computed" module - some JavaScript that most likely is rendered and
  ///    uses the "synthetic" module - this module's ID is returned from
  ///    [`new_module`] call.
  ///  - a "synthetic" module - a kind of a helper module that abstracts
  ///    the source of JS objects - this module is set up first.
  ComputedAndSynthetic(
    // Source code of computed module,
    FastString,
    // Synthetic module value
    v8::Global<v8::Value>,
    // Synthetic module type
    ModuleType,
  ),
}

#[derive(Debug)]
pub(crate) enum ImportAttributesKind {
  StaticImport,
  DynamicImport,
}

pub(crate) fn parse_import_attributes(
  scope: &mut v8::HandleScope,
  attributes: v8::Local<v8::FixedArray>,
  kind: ImportAttributesKind,
) -> HashMap<String, String> {
  let mut assertions: HashMap<String, String> = HashMap::default();

  let assertions_per_line = match kind {
    // For static imports, assertions are triples of (keyword, value and source offset)
    // Also used in `module_resolve_callback`.
    ImportAttributesKind::StaticImport => 3,
    // For dynamic imports, assertions are tuples of (keyword, value)
    ImportAttributesKind::DynamicImport => 2,
  };
  assert_eq!(attributes.length() % assertions_per_line, 0);
  let no_of_assertions = attributes.length() / assertions_per_line;

  for i in 0..no_of_assertions {
    let assert_key = attributes.get(scope, assertions_per_line * i).unwrap();
    let assert_key_val = v8::Local::<v8::Value>::try_from(assert_key).unwrap();
    let assert_value = attributes
      .get(scope, (assertions_per_line * i) + 1)
      .unwrap();
    let assert_value_val =
      v8::Local::<v8::Value>::try_from(assert_value).unwrap();
    assertions.insert(
      assert_key_val.to_rust_string_lossy(scope),
      assert_value_val.to_rust_string_lossy(scope),
    );
  }

  assertions
}

pub(crate) fn get_requested_module_type_from_attributes(
  attributes: &HashMap<String, String>,
) -> RequestedModuleType {
  let Some(ty) = attributes.get("type") else {
    return RequestedModuleType::None;
  };

  if ty == "json" {
    RequestedModuleType::Json
  } else {
    RequestedModuleType::Other(Cow::Owned(ty.to_string()))
  }
}

/// A type of module to be executed.
///
/// `deno_core` supports loading and executing JavaScript, Wasm and JSON modules,
/// by default, but embedders can customize it further by providing
/// [`CustomModuleEvaluationCb`].
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ModuleType {
  JavaScript,
  Wasm,
  Json,
  Other(Cow<'static, str>),
}

impl std::fmt::Display for ModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::JavaScript => write!(f, "JavaScript"),
      Self::Wasm => write!(f, "Wasm"),
      Self::Json => write!(f, "JSON"),
      Self::Other(ty) => write!(f, "{}", ty),
    }
  }
}

impl ModuleType {
  pub fn to_v8<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
  ) -> v8::Local<'s, v8::Value> {
    match self {
      ModuleType::JavaScript => v8::Integer::new(scope, 0).into(),
      ModuleType::Wasm => v8::Integer::new(scope, 1).into(),
      ModuleType::Json => v8::Integer::new(scope, 2).into(),
      ModuleType::Other(ty) => v8::String::new(scope, ty).unwrap().into(),
    }
  }

  pub fn try_from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Option<Self> {
    Some(if let Some(int) = value.to_integer(scope) {
      match int.int32_value(scope).unwrap_or_default() {
        0 => ModuleType::JavaScript,
        1 => ModuleType::Wasm,
        2 => ModuleType::Json,
        _ => return None,
      }
    } else if let Ok(str) = v8::Local::<v8::String>::try_from(value) {
      ModuleType::Other(Cow::Owned(str.to_rust_string_lossy(scope)))
    } else {
      return None;
    })
  }
}

#[derive(Debug)]
pub struct SourceCodeCacheInfo {
  pub hash: u64,
  pub data: Option<Cow<'static, [u8]>>,
}

/// EsModule source code that will be loaded into V8.
///
/// Users can implement `Into<ModuleInfo>` for different file types that
/// can be transpiled to valid EsModule.
///
/// Found module URL might be different from specified URL
/// used for loading due to redirections (like HTTP 303).
/// Eg. Both "`https://example.com/a.ts`" and
/// "`https://example.com/b.ts`" may point to "`https://example.com/c.ts`"
/// By keeping track of specified and found URL we can alias modules and avoid
/// recompiling the same code 3 times.
// TODO(bartlomieju): I have a strong opinion we should store all redirects
// that happened; not only first and final target. It would simplify a lot
// of things throughout the codebase otherwise we may end up requesting
// intermediate redirects from file loader.
// NOTE: This should _not_ be made #[derive(Clone)] unless we take some precautions to avoid excessive string copying.
#[derive(Debug)]
pub struct ModuleSource {
  pub code: ModuleSourceCode,
  pub module_type: ModuleType,
  pub code_cache: Option<SourceCodeCacheInfo>,
  module_url_specified: ModuleName,
  /// If the module was found somewhere other than the specified address, this will be [`Some`].
  module_url_found: Option<ModuleName>,
}

impl ModuleSource {
  /// Create a [`ModuleSource`] without a redirect.
  pub fn new(
    module_type: impl Into<ModuleType>,
    code: ModuleSourceCode,
    specifier: &ModuleSpecifier,
    code_cache: Option<SourceCodeCacheInfo>,
  ) -> Self {
    let module_url_specified = specifier.as_ref().to_owned().into();
    Self {
      code,
      module_type: module_type.into(),
      code_cache,
      module_url_specified,
      module_url_found: None,
    }
  }

  /// Create a [`ModuleSource`] with a potential redirect. If the `specifier_found` parameter is the same as the
  /// specifier, the code behaves the same was as `ModuleSource::new`.
  pub fn new_with_redirect(
    module_type: impl Into<ModuleType>,
    code: ModuleSourceCode,
    specifier: &ModuleSpecifier,
    specifier_found: &ModuleSpecifier,
    code_cache: Option<SourceCodeCacheInfo>,
  ) -> Self {
    let module_url_found = if specifier == specifier_found {
      None
    } else {
      Some(specifier_found.as_ref().to_owned().into())
    };
    let module_url_specified = specifier.as_ref().to_owned().into();
    Self {
      code,
      module_type: module_type.into(),
      code_cache,
      module_url_specified,
      module_url_found,
    }
  }

  #[cfg(test)]
  pub fn for_test(code: &'static str, file: impl AsRef<str>) -> Self {
    Self {
      code: ModuleSourceCode::String(code.into_module_code()),
      module_type: ModuleType::JavaScript,
      code_cache: None,
      module_url_specified: file.as_ref().to_owned().into(),
      module_url_found: None,
    }
  }

  /// If the `found` parameter is the same as the `specified` parameter, the code behaves the same was as `ModuleSource::for_test`.
  #[cfg(test)]
  pub fn for_test_with_redirect(
    code: &'static str,
    specified: impl AsRef<str>,
    found: impl AsRef<str>,
    code_cache: Option<SourceCodeCacheInfo>,
  ) -> Self {
    let specified = specified.as_ref().to_string();
    let found = found.as_ref().to_string();
    let found = if found == specified {
      None
    } else {
      Some(found.into())
    };
    Self {
      code: ModuleSourceCode::String(code.into_module_code()),
      module_type: ModuleType::JavaScript,
      code_cache,
      module_url_specified: specified.into(),
      module_url_found: found,
    }
  }

  pub fn get_string_source(code: ModuleSourceCode) -> ModuleCodeString {
    match code {
      ModuleSourceCode::String(code) => code,
      ModuleSourceCode::Bytes(bytes) => {
        match String::from_utf8_lossy(bytes.as_bytes()) {
          Cow::Borrowed(s) => ModuleCodeString::from(s.to_owned()),
          Cow::Owned(s) => ModuleCodeString::from(s),
        }
      }
    }
  }
}

pub type ModuleSourceFuture =
  dyn Future<Output = Result<ModuleSource, ModuleLoaderError>>;

#[derive(Debug, PartialEq, Eq)]
pub enum ResolutionKind {
  /// This kind is used in only one situation: when a module is loaded via
  /// `JsRuntime::load_main_module` and is the top-level module, ie. the one
  /// passed as an argument to `JsRuntime::load_main_module`.
  MainModule,
  /// This kind is returned for all other modules during module load, that are
  /// static imports.
  Import,
  /// This kind is returned for all modules that are loaded as a result of a
  /// call to `import()` API (ie. top-level module as well as all its
  /// dependencies, and any other `import()` calls from that load).
  DynamicImport,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum RequestedModuleType {
  /// There was no attribute specified in the import statement.
  ///
  /// Example:
  /// ```ignore
  /// import foo from "./foo.js";
  ///
  /// const bar = await import("bar");
  /// ```
  None,

  /// The `type` attribute had value `json`. This is the only known module type
  /// in `deno_core`.
  ///
  /// Embedders should use `Other` variant for custom module
  /// types like `wasm`, `bytes` or `text`.
  ///
  /// Example:
  /// ```ignore
  /// import jsonData from "./data.json" with { type: "json" };
  ///
  /// const jsonData2 = await import"./data2.json", { with { type: "json" } });
  /// ```
  Json,

  /// An arbitrary module type. It is up to the embedder to handle (or deny) it.
  /// If [`CustomModuleEvaluationCb`] was not passed when creating a runtime,
  /// then all "other" module types cause an error to be returned.
  ///
  /// Example:
  /// ```ignore
  /// import text from "./log.txt" with { type: "text" };
  ///
  /// const imgData = await import(`./images/${name}.png`, { with: { type: "bytes" }});
  /// ```
  Other(Cow<'static, str>),
}

impl RequestedModuleType {
  pub fn to_v8<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
  ) -> v8::Local<'s, v8::Value> {
    match self {
      RequestedModuleType::None => v8::Integer::new(scope, 0).into(),
      RequestedModuleType::Json => v8::Integer::new(scope, 1).into(),
      RequestedModuleType::Other(ty) => {
        v8::String::new(scope, ty).unwrap().into()
      }
    }
  }

  pub fn try_from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Option<Self> {
    Some(if let Some(int) = value.to_integer(scope) {
      match int.int32_value(scope).unwrap_or_default() {
        0 => RequestedModuleType::None,
        1 => RequestedModuleType::Json,
        _ => return None,
      }
    } else if let Ok(str) = v8::Local::<v8::String>::try_from(value) {
      RequestedModuleType::Other(Cow::Owned(str.to_rust_string_lossy(scope)))
    } else {
      return None;
    })
  }
}

impl AsRef<RequestedModuleType> for RequestedModuleType {
  fn as_ref(&self) -> &RequestedModuleType {
    self
  }
}

// TODO(bartlomieju): this is questionable. I think we should remove it.
impl PartialEq<ModuleType> for RequestedModuleType {
  fn eq(&self, other: &ModuleType) -> bool {
    match other {
      ModuleType::JavaScript => self == &RequestedModuleType::None,
      ModuleType::Wasm => self == &RequestedModuleType::None,
      ModuleType::Json => self == &RequestedModuleType::Json,
      ModuleType::Other(ty) => self == &RequestedModuleType::Other(ty.clone()),
    }
  }
}

impl From<ModuleType> for RequestedModuleType {
  fn from(module_type: ModuleType) -> RequestedModuleType {
    match module_type {
      ModuleType::JavaScript => RequestedModuleType::None,
      ModuleType::Wasm => RequestedModuleType::None,
      ModuleType::Json => RequestedModuleType::Json,
      ModuleType::Other(ty) => RequestedModuleType::Other(ty.clone()),
    }
  }
}

impl std::fmt::Display for RequestedModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::None => write!(f, "None"),
      Self::Json => write!(f, "JSON"),
      Self::Other(ty) => write!(f, "Other({ty})"),
    }
  }
}

/// Describes a request for a module as parsed from the source code.
/// Usually executable (`JavaScriptOrWasm`) is used, except when an
/// import assertions explicitly constrains an import to JSON, in
/// which case this will have a `RequestedModuleType::Json`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleRequest {
  pub specifier: ModuleSpecifier,
  pub requested_module_type: RequestedModuleType,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleInfo {
  #[allow(unused)]
  pub id: ModuleId,
  pub main: bool,
  pub name: ModuleName,
  pub requests: Vec<ModuleRequest>,
  pub module_type: ModuleType,
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
pub enum ModuleConcreteError {
  #[error(
    "Trying to create \"main\" module ({new_module:?}), when one already exists ({main_module:?})"
  )]
  MainModuleAlreadyExists {
    main_module: String,
    new_module: String,
  },
  #[error("Unable to get code cache from unbound module script")]
  UnboundModuleScriptCodeCache,
  #[class(inherit)]
  #[error("{0}")]
  WasmParse(wasm_dep_analyzer::ParseError),
  #[error("Source code for Wasm module must be provided as bytes")]
  WasmNotBytes,
  #[error("Failed to compile Wasm module '{0}'")]
  WasmCompile(String),
  #[error("Importing '{0}' modules is not supported")]
  UnsupportedKind(String),
}

#[derive(Debug)]
pub enum ModuleError {
  Exception(v8::Global<v8::Value>),
  Concrete(ModuleConcreteError),
  Core(CoreError),
}

impl ModuleError {
  pub fn into_error(
    self,
    scope: &mut v8::HandleScope,
    in_promise: bool,
    clear_error: bool,
  ) -> CoreError {
    match self {
      ModuleError::Exception(exception) => {
        let exception = v8::Local::new(scope, exception);
        exception_to_err_result::<()>(scope, exception, in_promise, clear_error)
          .unwrap_err()
      }
      ModuleError::Core(error) => error,
      ModuleError::Concrete(error) => CoreError::Module(error),
    }
  }
}

impl From<ModuleConcreteError> for ModuleError {
  fn from(value: ModuleConcreteError) -> Self {
    ModuleError::Concrete(value)
  }
}
