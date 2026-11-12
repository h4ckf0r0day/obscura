mod protocol;
#[cfg(feature = "headed")]
mod client;
#[cfg(feature = "headed")]
mod window;
#[cfg(feature = "headed")]
mod events;

pub(crate) use protocol::Packet;
#[cfg(feature = "headed")]
pub(crate) use client::NativeClient;
#[cfg(feature = "headed")]
pub use window::run_window;

#[cfg(not(feature = "headed"))]
pub(crate) struct NativeClient;

pub(crate) async fn start() -> anyhow::Result<NativeClient> {
    #[cfg(feature = "headed")]
    { client::NativeClient::start().await }
    #[cfg(not(feature = "headed"))]
    { anyhow::bail!("--headed requires a build with the headed feature") }
}

pub(crate) fn process_id(client: &NativeClient) -> Option<u32> {
    #[cfg(feature = "headed")]
    { client.process_id() }
    #[cfg(not(feature = "headed"))]
    { let _ = client; None }
}

pub(crate) async fn next_packet(client: &mut Option<NativeClient>) -> Option<Packet> {
    #[cfg(feature = "headed")]
    if let Some(client) = client { return client.receive().await; }
    let _ = client;
    std::future::pending().await
}

pub(crate) async fn apply(
    client: &mut NativeClient,
    packet: Packet,
    state: &mut crate::BrowserState,
) -> anyhow::Result<()> {
    #[cfg(feature = "headed")]
    { client.apply(packet, state).await }
    #[cfg(not(feature = "headed"))]
    { let _ = (client, packet, state); Ok(()) }
}
