use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use deno_core::{op2, OpDecl, OpState};
use deno_error::JsErrorBox;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::message::{MessageResource, SerializedMessage};

const CHANNEL_CAPACITY: usize = 1024;
const MAX_MESSAGE_PORT_ENDPOINTS: usize = 4096;

#[derive(Clone, Default)]
struct MessagePortHub(Arc<Mutex<MessagePortHubState>>);

#[derive(Default)]
struct MessagePortHubState {
    next_id: u32,
    endpoints: HashMap<u32, Arc<MessagePortEndpoint>>,
}

struct MessagePortEndpoint {
    incoming: tokio::sync::Mutex<mpsc::Receiver<SerializedMessage>>,
    outgoing: mpsc::Sender<SerializedMessage>,
    owner: watch::Sender<u32>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagePortHandle {
    id: u32,
    owner: u32,
}

impl MessagePortHub {
    fn pair(&self) -> Result<[MessagePortHandle; 2], JsErrorBox> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.endpoints.len() > MAX_MESSAGE_PORT_ENDPOINTS.saturating_sub(2) {
            return Err(JsErrorBox::range_error("Too many active MessagePorts"));
        }
        let first_id = Self::next_id(&mut state)?;
        let second_id = Self::next_id(&mut state)?;
        let (to_first, first_incoming) = mpsc::channel(CHANNEL_CAPACITY);
        let (to_second, second_incoming) = mpsc::channel(CHANNEL_CAPACITY);
        state.endpoints.insert(
            first_id,
            Arc::new(MessagePortEndpoint {
                incoming: tokio::sync::Mutex::new(first_incoming),
                outgoing: to_second,
                owner: watch::channel(1).0,
            }),
        );
        state.endpoints.insert(
            second_id,
            Arc::new(MessagePortEndpoint {
                incoming: tokio::sync::Mutex::new(second_incoming),
                outgoing: to_first,
                owner: watch::channel(1).0,
            }),
        );
        Ok([
            MessagePortHandle {
                id: first_id,
                owner: 1,
            },
            MessagePortHandle {
                id: second_id,
                owner: 1,
            },
        ])
    }

    fn next_id(state: &mut MessagePortHubState) -> Result<u32, JsErrorBox> {
        for _ in 0..=MAX_MESSAGE_PORT_ENDPOINTS {
            state.next_id = state.next_id.wrapping_add(1).max(1);
            if !state.endpoints.contains_key(&state.next_id) {
                return Ok(state.next_id);
            }
        }
        Err(JsErrorBox::range_error(
            "No MessagePort identifiers available",
        ))
    }

    fn transfer(
        &self,
        handles: &[MessagePortHandle],
    ) -> Result<Vec<MessagePortHandle>, JsErrorBox> {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let mut seen = HashSet::new();
        let mut endpoints = Vec::with_capacity(handles.len());
        for handle in handles {
            if !seen.insert(handle.id) {
                return Err(JsErrorBox::type_error(
                    "A MessagePort cannot be transferred more than once",
                ));
            }
            let endpoint = state
                .endpoints
                .get(&handle.id)
                .ok_or_else(|| JsErrorBox::type_error("MessagePort is closed"))?;
            if *endpoint.owner.borrow() != handle.owner {
                return Err(JsErrorBox::type_error("MessagePort is already transferred"));
            }
            endpoints.push(endpoint.clone());
        }
        let transferred = handles
            .iter()
            .zip(endpoints)
            .map(|(handle, endpoint)| {
                let owner = handle.owner.wrapping_add(1).max(1);
                endpoint.owner.send_replace(owner);
                MessagePortHandle {
                    id: handle.id,
                    owner,
                }
            })
            .collect();
        Ok(transferred)
    }

    fn post(
        &self,
        handle: MessagePortHandle,
        message: SerializedMessage,
    ) -> Result<(), JsErrorBox> {
        let outgoing = {
            let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            let endpoint = state
                .endpoints
                .get(&handle.id)
                .ok_or_else(|| JsErrorBox::type_error("MessagePort is closed"))?;
            if *endpoint.owner.borrow() != handle.owner {
                return Err(JsErrorBox::type_error("MessagePort is already transferred"));
            }
            endpoint.outgoing.clone()
        };
        match outgoing.try_send(message) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(JsErrorBox::range_error("MessagePort queue is full"))
            }
        }
    }

    async fn receive(
        &self,
        handle: MessagePortHandle,
    ) -> Result<Option<SerializedMessage>, JsErrorBox> {
        let endpoint = {
            let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            state
                .endpoints
                .get(&handle.id)
                .cloned()
                .ok_or_else(|| JsErrorBox::type_error("MessagePort is closed"))?
        };
        let mut owner = endpoint.owner.subscribe();
        if *owner.borrow() != handle.owner {
            return Err(JsErrorBox::type_error("MessagePort is already transferred"));
        }
        let mut incoming = endpoint.incoming.lock().await;
        if *owner.borrow() != handle.owner {
            return Err(JsErrorBox::type_error("MessagePort is already transferred"));
        }
        tokio::select! {
            biased;
            changed = owner.changed() => {
                let _ = changed;
                Err(JsErrorBox::type_error("MessagePort ownership changed"))
            }
            message = incoming.recv() => Ok(message),
        }
    }

    fn close(&self, handle: MessagePortHandle) -> bool {
        let endpoint = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            let Some(endpoint) = state.endpoints.get(&handle.id) else {
                return false;
            };
            if *endpoint.owner.borrow() != handle.owner {
                return false;
            }
            state.endpoints.remove(&handle.id).unwrap()
        };
        endpoint.owner.send_replace(0);
        true
    }
}

fn message_port_hub(state: &OpState) -> Result<MessagePortHub, JsErrorBox> {
    state
        .try_borrow::<MessagePortHub>()
        .cloned()
        .ok_or_else(|| JsErrorBox::type_error("MessagePort transport is unavailable"))
}

fn ensure_message_port_hub(state: &mut OpState) -> MessagePortHub {
    if let Some(hub) = state.try_borrow::<MessagePortHub>().cloned() {
        return hub;
    }
    let hub = MessagePortHub::default();
    state.put(hub.clone());
    hub
}

#[op2]
#[serde]
fn op_message_port_create_pair(state: &mut OpState) -> Result<Vec<MessagePortHandle>, JsErrorBox> {
    ensure_message_port_hub(state).pair().map(Vec::from)
}

#[op2]
#[serde]
fn op_message_port_transfer(
    state: &OpState,
    #[serde] handles: Vec<MessagePortHandle>,
) -> Result<Vec<MessagePortHandle>, JsErrorBox> {
    message_port_hub(state)?.transfer(&handles)
}

#[op2(fast)]
fn op_message_port_post(
    state: &mut OpState,
    id: u32,
    owner: u32,
    packet: u32,
) -> Result<(), JsErrorBox> {
    let resource = state
        .resource_table
        .take::<MessageResource>(packet)
        .map_err(|error| JsErrorBox::type_error(error.to_string()))?;
    let message = resource.take()?;
    message_port_hub(state)?.post(MessagePortHandle { id, owner }, message)
}

#[op2]
async fn op_message_port_receive(
    state: Rc<RefCell<OpState>>,
    id: u32,
    owner: u32,
) -> Result<Option<u32>, JsErrorBox> {
    let hub = message_port_hub(&state.borrow())?;
    let message = hub.receive(MessagePortHandle { id, owner }).await?;
    Ok(message.map(|message| {
        state
            .borrow_mut()
            .resource_table
            .add(MessageResource::new(message))
    }))
}

#[op2(fast)]
fn op_message_port_close(state: &OpState, id: u32, owner: u32) -> bool {
    message_port_hub(state).is_ok_and(|hub| hub.close(MessagePortHandle { id, owner }))
}

pub(crate) fn ops() -> Vec<OpDecl> {
    vec![
        op_message_port_create_pair(),
        op_message_port_transfer(),
        op_message_port_post(),
        op_message_port_receive(),
        op_message_port_close(),
    ]
}
