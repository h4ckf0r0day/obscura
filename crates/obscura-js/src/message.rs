use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use deno_core::v8::{ValueDeserializerHelper as _, ValueSerializerHelper as _};
use deno_core::{op2, v8, OpDecl, OpState, Resource};
use deno_error::JsErrorBox;

const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SHARED_OBJECTS: usize = 1024;
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;
// A minimum charge also caps the number of tiny pending message resources.
const MIN_MESSAGE_CHARGE: usize = 64 * 1024;

#[derive(Clone, Default)]
struct MessageBudget(Arc<AtomicUsize>);

struct MessageReservation {
    budget: MessageBudget,
    bytes: usize,
}

impl MessageBudget {
    fn reserve(&self, bytes: usize) -> Result<MessageReservation, JsErrorBox> {
        let bytes = bytes.max(MIN_MESSAGE_CHARGE);
        let mut used = self.0.load(Ordering::Relaxed);
        loop {
            let Some(total) = used
                .checked_add(bytes)
                .filter(|total| *total <= MAX_PENDING_BYTES)
            else {
                return Err(JsErrorBox::range_error(
                    "Pending messages exceed the size limit",
                ));
            };
            match self
                .0
                .compare_exchange_weak(used, total, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => used = actual,
            }
        }
        Ok(MessageReservation {
            budget: self.clone(),
            bytes,
        })
    }
}

impl Drop for MessageReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct SharedObjects {
    buffers: Vec<v8::SharedRef<v8::BackingStore>>,
    modules: Vec<v8::CompiledWasmModule>,
}

// No V8 local/global handles travel between threads. Backing stores and
// compiled modules are the V8-supported, independently owned shared objects.
pub struct SerializedMessage {
    bytes: Vec<u8>,
    shared: SharedObjects,
    _reservation: MessageReservation,
}

impl SerializedMessage {
    pub(crate) fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

pub(crate) struct MessageResource(RefCell<Option<SerializedMessage>>);

impl MessageResource {
    pub(crate) fn new(message: SerializedMessage) -> Self {
        Self(RefCell::new(Some(message)))
    }

    pub(crate) fn take(&self) -> Result<SerializedMessage, JsErrorBox> {
        self.0
            .borrow_mut()
            .take()
            .ok_or_else(|| JsErrorBox::type_error("Message was already consumed"))
    }
}

impl Resource for MessageResource {
    fn name(&self) -> Cow<'_, str> {
        "serializedMessage".into()
    }
}

struct MessageWriter<'a> {
    shared: Rc<RefCell<SharedObjects>>,
    error_callback: Option<v8::Local<'a, v8::Function>>,
}

impl v8::ValueSerializerImpl for MessageWriter<'_> {
    fn throw_data_clone_error<'s, 'i>(
        &self,
        scope: &mut v8::PinScope<'s, 'i>,
        message: v8::Local<'s, v8::String>,
    ) {
        if let Some(callback) = self.error_callback {
            v8::tc_scope!(let scope, scope);
            let undefined = v8::undefined(scope).into();
            callback.call(scope, undefined, &[message.into()]);
            if scope.has_caught() || scope.has_terminated() {
                scope.rethrow();
                return;
            }
        }
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
    }

    fn get_shared_array_buffer_id<'s, 'i>(
        &self,
        _scope: &mut v8::PinScope<'s, 'i>,
        buffer: v8::Local<'s, v8::SharedArrayBuffer>,
    ) -> Option<u32> {
        let mut shared = self.shared.borrow_mut();
        if shared.buffers.len() >= MAX_SHARED_OBJECTS {
            return None;
        }
        let id = shared.buffers.len() as u32;
        shared.buffers.push(buffer.get_backing_store());
        Some(id)
    }

    fn get_wasm_module_transfer_id<'s, 'i>(
        &self,
        _scope: &mut v8::PinScope<'s, 'i>,
        module: v8::Local<v8::WasmModuleObject>,
    ) -> Option<u32> {
        let mut shared = self.shared.borrow_mut();
        if shared.modules.len() >= MAX_SHARED_OBJECTS {
            return None;
        }
        let id = shared.modules.len() as u32;
        shared.modules.push(module.get_compiled_module());
        Some(id)
    }
}

struct MessageReader(SharedObjects);

impl v8::ValueDeserializerImpl for MessageReader {
    fn get_shared_array_buffer_from_id<'s, 'i>(
        &self,
        scope: &mut v8::PinScope<'s, 'i>,
        id: u32,
    ) -> Option<v8::Local<'s, v8::SharedArrayBuffer>> {
        Some(v8::SharedArrayBuffer::with_backing_store(
            scope,
            self.0.buffers.get(id as usize)?,
        ))
    }

    fn get_wasm_module_from_id<'s, 'i>(
        &self,
        scope: &mut v8::PinScope<'s, 'i>,
        id: u32,
    ) -> Option<v8::Local<'s, v8::WasmModuleObject>> {
        v8::WasmModuleObject::from_compiled_module(scope, self.0.modules.get(id as usize)?)
    }
}

#[op2(reentrant)]
fn op_message_serialize<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    state: Rc<RefCell<OpState>>,
    value: v8::Local<'s, v8::Value>,
    error_callback: Option<v8::Local<'s, v8::Function>>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
    let shared = Rc::new(RefCell::new(SharedObjects::default()));
    let writer = v8::ValueSerializer::new(
        scope,
        Box::new(MessageWriter {
            shared: shared.clone(),
            error_callback,
        }),
    );
    writer.write_header();
    v8::tc_scope!(let scope, scope);
    let written = writer.write_value(scope.get_current_context(), value);
    if scope.has_caught() || scope.has_terminated() {
        scope.rethrow();
        return Ok(v8::undefined(scope).into());
    }
    if written != Some(true) {
        return Err(JsErrorBox::type_error("Value could not be cloned"));
    }
    let bytes = writer.release();
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(JsErrorBox::range_error("Message exceeds the size limit"));
    }
    let budget = {
        let mut state = state.borrow_mut();
        if state.try_borrow::<MessageBudget>().is_none() {
            state.put(MessageBudget::default());
        }
        state.borrow::<MessageBudget>().clone()
    };
    let reservation = budget.reserve(bytes.len())?;
    let message = SerializedMessage {
        bytes,
        shared: std::mem::take(&mut *shared.borrow_mut()),
        _reservation: reservation,
    };
    let rid = state
        .borrow_mut()
        .resource_table
        .add(MessageResource::new(message));
    Ok(v8::Integer::new_from_unsigned(scope, rid).into())
}

#[op2(reentrant)]
fn op_message_deserialize<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    state: Rc<RefCell<OpState>>,
    rid: u32,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
    let resource = state
        .borrow_mut()
        .resource_table
        .take::<MessageResource>(rid)
        .map_err(|error| JsErrorBox::type_error(error.to_string()))?;
    let SerializedMessage {
        bytes,
        shared,
        _reservation,
    } = resource.take()?;
    let reader = v8::ValueDeserializer::new(scope, Box::new(MessageReader(shared)), &bytes);
    v8::tc_scope!(let scope, scope);
    let value = if reader.read_header(scope.get_current_context()) == Some(true) {
        reader.read_value(scope.get_current_context())
    } else {
        None
    };
    if scope.has_caught() || scope.has_terminated() {
        scope.rethrow();
        return Ok(v8::undefined(scope).into());
    }
    value.ok_or_else(|| JsErrorBox::type_error("Message could not be deserialized"))
}

pub(crate) fn ops() -> Vec<OpDecl> {
    vec![op_message_serialize(), op_message_deserialize()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_message_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<SerializedMessage>();
    }
}
