use std::sync::OnceLock;

use deno_core::v8;

pub(crate) fn source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        include_str!("../js/bootstrap.js")
            .replace("/* INDEXED_DB */", include_str!("../js/indexed_db.js"))
    })
}

pub(crate) fn install_in_context<'i>(
    scope: &mut v8::PinScope<'_, 'i>,
    context: v8::Local<'i, v8::Context>,
) -> bool {
    let scope = &mut v8::ContextScope::new(scope, context);
    let global = context.global(scope);
    let ops = v8::Object::new(scope);
    let core = v8::Object::new(scope);
    let deno = v8::Object::new(scope);
    let Some(ops_key) = v8::String::new(scope, "ops") else {
        return false;
    };
    let Some(core_key) = v8::String::new(scope, "core") else {
        return false;
    };
    let Some(deno_key) = v8::String::new(scope, "Deno") else {
        return false;
    };
    if !core.set(scope, ops_key.into(), ops.into()).unwrap_or(false)
        || !deno.set(scope, core_key.into(), core.into()).unwrap_or(false)
        || !global.set(scope, deno_key.into(), deno.into()).unwrap_or(false)
    {
        return false;
    }

    let Some(source_code) = v8::String::new(scope, source()) else {
        return false;
    };
    let Some(script) = v8::Script::compile(scope, source_code, None) else {
        return false;
    };
    script.run(scope).is_some()
}
