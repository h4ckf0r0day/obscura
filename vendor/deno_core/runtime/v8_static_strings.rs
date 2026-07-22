// Copyright 2018-2025 the Deno authors. MIT license.

#[macro_export]
macro_rules! v8_static_strings {
  ($($ident:ident = $str:literal),* $(,)?) => {
    $(
      pub static $ident: $crate::FastStaticString = $crate::ascii_str!($str);
    )*
  };
}

pub use v8_static_strings;

v8_static_strings!(
  BUILD_CUSTOM_ERROR = "buildCustomError",
  CALL_CONSOLE = "callConsole",
  CALL_SITE_EVALS = "deno_core::call_site_evals",
  CAUSE = "cause",
  CODE = "code",
  CONSOLE = "console",
  CONSTRUCTOR = "constructor",
  CORE = "core",
  DENO = "Deno",
  DEFAULT = "default",
  DIRNAME = "dirname",
  ERR_MODULE_NOT_FOUND = "ERR_MODULE_NOT_FOUND",
  ERRORS = "errors",
  EVENT_LOOP_TICK = "eventLoopTick",
  FILENAME = "filename",
  INSTANCE = "Instance",
  MAIN = "main",
  MESSAGE = "message",
  NAME = "name",
  OPS = "ops",
  RESOLVE = "resolve",
  SET_UP_ASYNC_STUB = "setUpAsyncStub",
  STACK = "stack",
  URL = "url",
  WASM_INSTANCE = "WasmInstance",
  WEBASSEMBLY = "WebAssembly",
  ESMODULE = "__esModule",
);
