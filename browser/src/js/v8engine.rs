//! V8 behind a page: the same host functions (`natives()`) and prelude as
//! Boa, run by Google's engine (the `v8` crate, Deno's bindings).
//!
//! V8 makes no requests of its own: module sources come from the page's
//! `Loader`, which fetches through net.rs like every other subresource.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{natives, peek, with, Engine, Loader, Native};
use crate::jsval::Val;
use crate::url::Url;

thread_local! {
    /// host functions by index (a function's `data` is its index)
    static NATIVES: RefCell<Vec<Native>> = const { RefCell::new(Vec::new()) };
    /// the page's loader, for the module callbacks (which get no state)
    static LOADER: RefCell<Option<Rc<Loader>>> = const { RefCell::new(None) };
    /// the page's module map: URL → module, and module → URL (by identity hash)
    static MODULES: RefCell<HashMap<String, v8::Global<v8::Module>>> = RefCell::new(HashMap::new());
    static MODULE_URLS: RefCell<HashMap<i32, String>> = RefCell::new(HashMap::new());
    /// promises rejected with no handler yet (removed when one is added)
    static REJECTED: RefCell<Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>> = const { RefCell::new(Vec::new()) };
}

fn init_platform() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // page threads have a 512 MB stack: let scripts recurse deep (V8's
        // default, ~1 MB, overflows where Boa and Chrome's renderer do not).
        // Isolates made on smaller threads get a limit near their own stack.
        v8::V8::set_flags_from_string("--stack-size=65536");
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

pub struct V8Engine {
    // dropped last: the context belongs to the isolate
    context: v8::Global<v8::Context>,
    isolate: v8::OwnedIsolate,
}

impl V8Engine {
    pub fn new(loader: Rc<Loader>, persona: crate::persona::Persona) -> V8Engine {
        // the page's clock in the chosen country's zone: V8 reads TZ, and
        // is told to read it again (an earlier page may have had another)
        let zone_changed = !persona.country.time_zone.is_empty() && std::env::var("TZ").ok().as_deref() != Some(persona.country.time_zone);
        if zone_changed {
            std::env::set_var("TZ", persona.country.time_zone);
        }
        init_platform();
        let table = natives();
        NATIVES.with(|n| *n.borrow_mut() = table.iter().map(|(_, _, f)| *f).collect());
        LOADER.with(|l| *l.borrow_mut() = Some(loader));
        MODULES.with(|m| m.borrow_mut().clear());
        MODULE_URLS.with(|m| m.borrow_mut().clear());
        REJECTED.with(|r| r.borrow_mut().clear());

        let mut params = v8::CreateParams::default();
        // on a page thread (a 512 MB stack) scripts may recurse as deep as
        // in Chrome's renderer and more; elsewhere V8's own default holds
        if std::thread::current().name() != Some("page") {
            let here = 0u8;
            let limit = (&here as *const u8 as usize).saturating_sub(512 * 1024);
            // SAFETY: half a megabyte below here is inside any thread's
            // stack, which outlives the isolate made on it
            params = unsafe { params.set_stack_limit(limit as *mut u32) };
        }
        let mut isolate = v8::Isolate::new(params);
        if zone_changed {
            isolate.date_time_configuration_change_notification(v8::TimeZoneDetection::Redetect);
        }
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(promise_reject);
        isolate.set_host_import_module_dynamically_callback(dynamic_import);
        isolate.set_host_initialize_import_meta_object_callback(import_meta);
        let context = {
            v8::scope!(let scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let global = context.global(scope);
            for (i, (name, len, _)) in table.iter().enumerate() {
                let data = v8::Integer::new(scope, i as i32);
                let f = v8::Function::builder(call_native).data(data.into()).length(*len as i32).build(scope).expect("host function");
                let key = v8::String::new(scope, name).unwrap();
                global.set(scope, key.into(), f.into());
            }
            let f = v8::Function::builder(script_eval).length(2).build(scope).expect("host function");
            let key = v8::String::new(scope, "__script_eval").unwrap();
            global.set(scope, key.into(), f.into());
            v8::Global::new(scope, context)
        };
        V8Engine { context, isolate }
    }
}

impl Engine for V8Engine {
    fn eval(&mut self, code: &str, label: &str) -> Result<Val, String> {
        v8::scope_with_context!(let scope, &mut self.isolate, &self.context);
        run_script(scope, code, label).map(|v| to_val(scope, v, 0))
    }

    fn run_jobs(&mut self) {
        v8::scope_with_context!(let scope, &mut self.isolate, &self.context);
        scope.perform_microtask_checkpoint();
    }

    fn run_module(&mut self, code: &str, path: &str) -> Result<(), String> {
        v8::scope_with_context!(let scope, &mut self.isolate, &self.context);
        let inline = path.contains("#inline-module-");
        let known = if inline { None } else { MODULES.with(|m| m.borrow().get(path).cloned()) };
        let module = match known {
            Some(module) => v8::Local::new(scope, &module),
            None => {
                v8::tc_scope!(let tc, scope);
                let Some(module) = compile_module(tc, code, path, !inline) else { return Err(caught(tc)) };
                module
            }
        };
        let promise = {
            v8::tc_scope!(let tc, scope);
            if module.get_status() == v8::ModuleStatus::Uninstantiated && module.instantiate_module(tc, resolve).is_none() {
                return Err(caught(tc));
            }
            match module.evaluate(tc) {
                Some(p) => p,
                None => return Err(caught(tc)),
            }
        };
        scope.perform_microtask_checkpoint();
        if let Ok(promise) = v8::Local::<v8::Promise>::try_from(promise) {
            if promise.state() == v8::PromiseState::Rejected {
                let reason = promise.result(scope);
                promise.mark_as_handled();
                return Err(describe(scope, reason));
            }
        }
        Ok(())
    }

    fn report_rejections(&mut self) {
        let pending = REJECTED.with(|r| std::mem::take(&mut *r.borrow_mut()));
        if pending.is_empty() {
            return;
        }
        v8::scope_with_context!(let scope, &mut self.isolate, &self.context);
        let context = scope.get_current_context();
        let global = context.global(scope);
        let key = v8::String::new(scope, "__sepia_unhandled").unwrap();
        let Some(report) = global.get(scope, key.into()).and_then(|f| v8::Local::<v8::Function>::try_from(f).ok()) else { return };
        for (promise, reason) in pending {
            let promise = v8::Local::new(scope, &promise);
            let reason = v8::Local::new(scope, &reason);
            v8::tc_scope!(let tc, scope);
            let undefined = v8::undefined(tc).into();
            let _ = report.call(tc, undefined, &[promise.into(), reason]);
        }
        scope.perform_microtask_checkpoint();
    }
}

// ---------------------------------------------------------------------------
// values
// ---------------------------------------------------------------------------

fn to_val(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, depth: u8) -> Val {
    if v.is_undefined() {
        Val::Undef
    } else if v.is_null() {
        Val::Null
    } else if v.is_boolean() {
        Val::Bool(v.boolean_value(scope))
    } else if v.is_number() {
        Val::Num(v.number_value(scope).unwrap_or(f64::NAN))
    } else if v.is_string() {
        Val::Str(v.to_rust_string_lossy(scope))
    } else if depth < 4 && v.is_array() {
        let array = v8::Local::<v8::Array>::try_from(v).unwrap();
        let mut items = Vec::with_capacity(array.length().min(1 << 16) as usize);
        for i in 0..array.length() {
            let item = array.get_index(scope, i).unwrap_or_else(|| v8::undefined(scope).into());
            items.push(to_val(scope, item, depth + 1));
        }
        Val::List(items)
    } else {
        // objects as their string (URL objects, String wrappers…), as Boa does
        v8::tc_scope!(let tc, scope);
        match v.to_string(tc) {
            Some(s) => Val::Str(s.to_rust_string_lossy(tc)),
            None => Val::Str(String::new()),
        }
    }
}

fn from_val<'s>(scope: &mut v8::PinScope<'s, '_>, v: Val) -> v8::Local<'s, v8::Value> {
    match v {
        Val::Undef => v8::undefined(scope).into(),
        Val::Null => v8::null(scope).into(),
        Val::Bool(b) => v8::Boolean::new(scope, b).into(),
        Val::Num(n) => v8::Number::new(scope, n).into(),
        Val::Str(s) => v8::String::new(scope, &s).map(Into::into).unwrap_or_else(|| v8::String::empty(scope).into()),
        Val::List(items) => {
            let items: Vec<v8::Local<v8::Value>> = items.into_iter().map(|v| from_val(scope, v)).collect();
            v8::Array::new_with_elements(scope, &items).into()
        }
    }
}

/// A host function's error as the JS error its text names (TypeError by default).
fn throw(scope: &mut v8::PinScope, message: String) {
    let (kind, rest) = match message.split_once(": ") {
        Some((k, r)) if k.ends_with("Error") && !k.contains(' ') => (k.to_string(), r.to_string()),
        _ => ("TypeError".to_string(), message.clone()),
    };
    let text = v8::String::new(scope, &rest).unwrap_or_else(|| v8::String::empty(scope));
    let error = match kind.as_str() {
        "SyntaxError" => v8::Exception::syntax_error(scope, text),
        "RangeError" => v8::Exception::range_error(scope, text),
        "ReferenceError" => v8::Exception::reference_error(scope, text),
        _ => v8::Exception::type_error(scope, text),
    };
    scope.throw_exception(error);
}

/// What was thrown, as `Kind: message` (and the stack with SEPIA_VERBOSE).
fn describe(scope: &mut v8::PinScope, exception: v8::Local<v8::Value>) -> String {
    let text = exception.to_rust_string_lossy(scope);
    if std::env::var_os("SEPIA_VERBOSE").is_none() {
        return text;
    }
    let stack = v8::Local::<v8::Object>::try_from(exception).ok().and_then(|o| {
        let key = v8::String::new(scope, "stack")?;
        o.get(scope, key.into())
    });
    match stack.filter(|s| s.is_string()) {
        Some(s) => format!("{text}\n{}", s.to_rust_string_lossy(scope)),
        None => text,
    }
}

fn caught(tc: &mut v8::PinnedRef<'_, v8::TryCatch<v8::HandleScope>>) -> String {
    match tc.exception() {
        Some(e) => describe(tc, e),
        None => "terminated".into(),
    }
}

// ---------------------------------------------------------------------------
// scripts and host functions
// ---------------------------------------------------------------------------

fn origin<'s>(scope: &mut v8::PinScope<'s, '_>, label: &str, module: bool) -> v8::ScriptOrigin<'s> {
    let name = v8::String::new(scope, if label.is_empty() { "sepia:glue" } else { label }).unwrap();
    v8::ScriptOrigin::new(scope, name.into(), 0, 0, false, 0, None, false, false, module, None)
}

fn run_script<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, label: &str) -> Result<v8::Local<'s, v8::Value>, String> {
    v8::tc_scope!(let tc, scope);
    let Some(source) = v8::String::new(tc, code) else { return Err("RangeError: script too long".into()) };
    let origin = origin(tc, label, false);
    let Some(script) = v8::Script::compile(tc, source, Some(&origin)) else { return Err(caught(tc)) };
    match script.run(tc) {
        Some(v) => Ok(v),
        None => Err(caught(tc)),
    }
}

fn call_native(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue<v8::Value>) {
    let index = args.data().int32_value(scope).unwrap_or(-1);
    let Some(f) = NATIVES.with(|n| n.borrow().get(index as usize).copied()) else { return };
    let vals: Vec<Val> = (0..args.length()).map(|i| to_val(scope, args.get(i), 0)).collect();
    match f(&vals) {
        Ok(v) => {
            let v = from_val(scope, v);
            rv.set(v);
        }
        Err(m) => throw(scope, m),
    }
}

/// `__script_eval(code, label)`: run a classic script the page inserted; false if it threw.
fn script_eval(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue<v8::Value>) {
    let code = args.get(0).to_rust_string_lossy(scope);
    let label = args.get(1).to_rust_string_lossy(scope);
    peek(&code, &label);
    let ok = match run_script(scope, &code, &label) {
        Ok(_) => true,
        Err(e) => {
            let line = format!("{label}: {e}");
            crate::report::js_error(&line);
            with(|st| st.log.push(line));
            false
        }
    };
    rv.set_bool(ok);
}

extern "C" fn promise_reject(message: v8::PromiseRejectMessage) {
    v8::callback_scope!(unsafe scope, &message);
    let promise = message.get_promise();
    match message.get_event() {
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            let reason = message.get_value().unwrap_or_else(|| v8::undefined(scope).into());
            let entry = (v8::Global::new(scope, promise), v8::Global::new(scope, reason));
            REJECTED.with(|r| r.borrow_mut().push(entry));
        }
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            let handled = v8::Global::new(scope, promise);
            REJECTED.with(|r| r.borrow_mut().retain(|(p, _)| *p != handled));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// modules
// ---------------------------------------------------------------------------

/// Compile a module and enter it in the page's module map (by URL) unless
/// it is an inline one.
fn compile_module<'s>(scope: &mut v8::PinScope<'s, '_>, code: &str, url: &str, remember: bool) -> Option<v8::Local<'s, v8::Module>> {
    let source = v8::String::new(scope, code)?;
    let origin = origin(scope, url, true);
    let mut source = v8::script_compiler::Source::new(source, Some(&origin));
    let module = v8::script_compiler::compile_module(scope, &mut source)?;
    MODULE_URLS.with(|m| m.borrow_mut().insert(module.get_identity_hash().get(), url.to_string()));
    if remember {
        let global = v8::Global::new(scope, module);
        MODULES.with(|m| m.borrow_mut().insert(url.to_string(), global));
    }
    Some(module)
}

/// The base URL of a module (its own URL), else the page's.
fn module_base(referrer: Option<&str>) -> Option<Url> {
    referrer.and_then(Url::parse).or_else(|| with(|st| st.url.clone()))
}

/// A module by specifier from `base`: from the map, else fetched and compiled.
fn load_module<'s>(scope: &mut v8::PinScope<'s, '_>, specifier: &str, base: Option<Url>) -> Result<v8::Local<'s, v8::Module>, String> {
    let base = base.ok_or("no base URL")?;
    let loader = LOADER.with(|l| l.borrow().clone()).ok_or("no loader")?;
    // known already (also by its resolved URL)
    if let Ok(url) = loader.locate(specifier, &base) {
        if let Some(known) = MODULES.with(|m| m.borrow().get(&url.to_string()).cloned()) {
            return Ok(v8::Local::new(scope, &known));
        }
    }
    let (key, code) = loader.source_for(specifier, &base)?;
    if let Some(known) = MODULES.with(|m| m.borrow().get(&key).cloned()) {
        return Ok(v8::Local::new(scope, &known));
    }
    v8::tc_scope!(let tc, scope);
    match compile_module(tc, &code, &key, true) {
        Some(m) => Ok(m),
        None => Err(caught(tc)),
    }
}

fn resolve<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let specifier = specifier.to_rust_string_lossy(scope);
    let from = MODULE_URLS.with(|m| m.borrow().get(&referrer.get_identity_hash().get()).cloned());
    match load_module(scope, &specifier, module_base(from.as_deref())) {
        Ok(m) => Some(m),
        Err(e) => {
            throw(scope, format!("TypeError: Failed to resolve module specifier \"{specifier}\": {e}"));
            None
        }
    }
}

/// `import()`: load, link and run the module now (sources come from the
/// prefetched graph or the network), and settle the promise with it.
fn dynamic_import<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    resource: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let specifier = specifier.to_rust_string_lossy(scope);
    let from = resource.is_string().then(|| resource.to_rust_string_lossy(scope)).filter(|r| r.contains("://"));
    let outcome: Result<v8::Local<v8::Value>, v8::Local<v8::Value>> = {
        v8::tc_scope!(let tc, scope);
        match load_module(tc, &specifier, module_base(from.as_deref())) {
            Err(e) => {
                let text = v8::String::new(tc, &format!("Failed to fetch dynamically imported module: {specifier}: {e}")).unwrap();
                Err(v8::Exception::type_error(tc, text))
            }
            Ok(module) => {
                let linked = module.get_status() != v8::ModuleStatus::Uninstantiated || module.instantiate_module(tc, resolve).is_some();
                let evaluated = if linked { module.evaluate(tc) } else { None };
                match evaluated {
                    None => Err(tc.exception().unwrap_or_else(|| v8::undefined(tc).into())),
                    Some(done) => {
                        let namespace = module.get_module_namespace();
                        match v8::Local::<v8::Promise>::try_from(done) {
                            Ok(p) if p.state() == v8::PromiseState::Rejected => Err(p.result(tc)),
                            _ => Ok(namespace),
                        }
                    }
                }
            }
        }
    };
    match outcome {
        Ok(namespace) => resolver.resolve(scope, namespace),
        Err(error) => resolver.reject(scope, error),
    };
    Some(promise)
}

extern "C" fn import_meta(context: v8::Local<v8::Context>, module: v8::Local<v8::Module>, meta: v8::Local<v8::Object>) {
    v8::callback_scope!(unsafe scope, context);
    v8::scope!(let scope, scope);
    let url = MODULE_URLS.with(|m| m.borrow().get(&module.get_identity_hash().get()).cloned()).unwrap_or_default();
    let key = v8::String::new(scope, "url").unwrap();
    let value = v8::String::new(scope, &url).unwrap();
    meta.create_data_property(scope, key.into(), value.into());
    // import.meta.resolve(specifier): through the page's import map
    let data = v8::String::new(scope, &url).unwrap();
    let resolve_fn = v8::Function::builder(|scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue<v8::Value>| {
        let base = args.data().to_rust_string_lossy(scope);
        let specifier = args.get(0).to_rust_string_lossy(scope);
        let loader = LOADER.with(|l| l.borrow().clone());
        let located = match (loader, Url::parse(&base)) {
            (Some(l), Some(b)) => l.locate(&specifier, &b),
            _ => Err("module has no URL".into()),
        };
        match located {
            Ok(u) => {
                let s = v8::String::new(scope, &u.to_string()).unwrap();
                rv.set(s.into());
            }
            Err(e) => throw(scope, e),
        }
    })
    .data(data.into())
    .build(scope);
    if let Some(f) = resolve_fn {
        let key = v8::String::new(scope, "resolve").unwrap();
        meta.create_data_property(scope, key.into(), f.into());
    }
}
