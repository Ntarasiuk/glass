//! Remote access ("anywhere" links): the session over WebRTC instead of WebTransport, for a
//! computer that isn't on this Mac's network. glassdesk.dev introduces the two sides
//! (one offer, one signed answer); then it's peer to peer. The host bridges the channels
//! onto its own WebTransport session (see `crate::remote`):
//! - "d" (unordered, no retransmits) ⇄ datagrams
//! - one channel per stream, "bi" or "uni"; a text message "end" finishes a stream.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::Message;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState, RTCIceServer,
    RTCPeerConnectionState, RTCSessionDescription, Registry, register_default_interceptors,
};

const STUN: &str = "stun:stun.cloudflare.com:3478";
const CHUNK: usize = 16 * 1024;
const GATHER: Duration = Duration::from_secs(3);
const CONNECT: Duration = Duration::from_secs(20);

struct Handler {
    gathered: mpsc::Sender<()>,
    channels: mpsc::UnboundedSender<Arc<dyn DataChannel>>,
    state: tokio::sync::watch::Sender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.state.send(state);
    }
    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.channels.send(channel);
    }
}

/// The introducer's next message.
async fn next_json<S, E>(ws: &mut S, secs: u64) -> Result<Value>
where
    S: futures_util::Stream<Item = Result<Message, E>> + Unpin,
{
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return serde_json::from_str::<Value>(&t).context("bad reply from glassdesk.dev"),
                Some(Ok(_)) => continue,
                _ => bail!("glassdesk.dev closed the connection"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(secs), fut).await.context("timed out")?
}

/// Where a channel's events go: its messages (`None`: it ended), and when it opened.
fn pump(channel: Arc<dyn DataChannel>) -> (mpsc::UnboundedReceiver<Option<Bytes>>, tokio::sync::watch::Receiver<bool>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (open_tx, open_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    let _ = open_tx.send(true);
                }
                DataChannelEvent::OnMessage(m) if m.is_string => {
                    if &m.data[..] == b"end" {
                        let _ = tx.send(None);
                    }
                }
                DataChannelEvent::OnMessage(m) => {
                    let _ = tx.send(Some(Bytes::copy_from_slice(&m.data)));
                }
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
        let _ = tx.send(None);
    });
    (rx, open_rx)
}

/// A stream's sending side: a reliable, ordered channel.
pub struct RtcTx {
    channel: Arc<dyn DataChannel>,
    open: tokio::sync::watch::Receiver<bool>,
}

impl RtcTx {
    pub async fn write_all(&mut self, mut bytes: &[u8]) -> Result<()> {
        let _ = self.open.wait_for(|o| *o).await;
        while !bytes.is_empty() {
            let n = bytes.len().min(CHUNK);
            // Backpressure: wait while the other side is behind.
            let _ = self.channel.writable().await;
            self.channel.send(BytesMut::from(&bytes[..n])).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            bytes = &bytes[n..];
        }
        Ok(())
    }

    pub async fn finish(&mut self) -> Result<()> {
        let _ = self.open.wait_for(|o| *o).await;
        self.channel.send_text("end").await.map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// A stream's receiving side.
pub struct RtcRx {
    rx: mpsc::UnboundedReceiver<Option<Bytes>>,
    buf: VecDeque<u8>,
    ended: bool,
}

impl RtcRx {
    /// Some bytes, or `None` at the end of the stream.
    pub async fn read(&mut self, out: &mut [u8]) -> Result<Option<usize>> {
        while self.buf.is_empty() {
            if self.ended {
                return Ok(None);
            }
            match self.rx.recv().await.flatten() {
                Some(b) => self.buf.extend(b.iter()),
                None => self.ended = true,
            }
        }
        let n = out.len().min(self.buf.len());
        for (o, b) in out.iter_mut().zip(self.buf.drain(..n)) {
            *o = b;
        }
        Ok(Some(n))
    }
}

pub struct RtcConn {
    pc: Arc<dyn PeerConnection>,
    /// Datagrams to send (from any thread, the decoder's included).
    out: mpsc::UnboundedSender<Bytes>,
    datagrams: Mutex<mpsc::UnboundedReceiver<Option<Bytes>>>,
    incoming: Mutex<mpsc::UnboundedReceiver<Arc<dyn DataChannel>>>,
    state: tokio::sync::watch::Receiver<RTCPeerConnectionState>,
}

impl RtcConn {
    /// Connects to the computer whose remote-access key this is, through glassdesk.dev.
    pub async fn connect(signal_base: &str, host_key: &str) -> Result<Self> {
        // wss:// to glassdesk.dev (the app process may not have chosen a TLS provider yet).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let key = crate::remote::b64url_decode(host_key).context("bad link")?;
        let url = format!("{}/signal/connect/{}", signal_base.replacen("http", "ws", 1), crate::remote::id_for(&key));
        let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&url)).await.context("couldn't reach glassdesk.dev")??;
        let (mut ws_tx, mut ws_rx) = ws.split();
        let hello = next_json(&mut ws_rx, 10).await.context("glassdesk.dev didn't answer")?;
        if hello["t"] != "hello" {
            bail!("unexpected reply from glassdesk.dev");
        }
        if hello["online"] != true {
            bail!("This computer is offline, or remote access is turned off on it.");
        }
        let mut servers = vec![RTCIceServer { urls: vec![STUN.into()], ..Default::default() }];
        for s in hello["iceServers"].as_array().into_iter().flatten() {
            let urls = match &s["urls"] {
                Value::String(u) => vec![u.clone()],
                Value::Array(a) => a.iter().filter_map(|u| u.as_str().map(str::to_string)).collect(),
                _ => continue,
            };
            servers.push(RTCIceServer {
                urls,
                username: s["username"].as_str().unwrap_or_default().into(),
                credential: s["credential"].as_str().unwrap_or_default().into(),
            });
        }

        let mut media = MediaEngine::default();
        let registry = register_default_interceptors(Registry::new(), &mut media)?;
        let (gathered_tx, mut gathered) = mpsc::channel(1);
        let (channels_tx, channels) = mpsc::unbounded_channel();
        let (state_tx, state) = tokio::sync::watch::channel(RTCPeerConnectionState::New);
        let handler = Arc::new(Handler { gathered: gathered_tx, channels: channels_tx, state: state_tx });
        let runtime = webrtc::runtime::default_runtime().context("no async runtime")?;
        let pc = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(servers).build())
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(handler)
            .with_runtime(runtime)
            .with_udp_addrs(vec!["0.0.0.0:0".to_string(), "[::]:0".to_string()])
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        // Video over the datagram channel, like any datagram: the native decoder takes it as is.
        let dgram = pc.create_data_channel("d", Some(RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() })).await?;
        let (datagrams, mut dgram_open) = pump(dgram.clone());

        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer).await?;
        let _ = tokio::time::timeout(GATHER, gathered.recv()).await;
        let offer_sdp = pc.local_description().await.context("no offer")?.sdp;
        ws_tx.send(Message::text(serde_json::json!({ "t": "offer", "sdp": offer_sdp }).to_string())).await?;
        let reply = next_json(&mut ws_rx, 15).await.context("the computer didn't answer")?;
        if reply["t"] == "error" {
            bail!("{}", reply["msg"].as_str().unwrap_or("glassdesk.dev refused"));
        }
        let answer_sdp = reply["sdp"].as_str().context("no answer")?.to_string();
        let sig = reply["sig"].as_str().and_then(crate::remote::b64url_decode).unwrap_or_default();
        // Only the computer holding the link's key can sign the answer.
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &key)
            .verify(&crate::remote::signed_payload(&offer_sdp, &answer_sdp), &sig)
            .map_err(|_| anyhow::anyhow!("Couldn't verify this computer's identity; not connecting."))?;
        pc.set_remote_description(RTCSessionDescription::answer(answer_sdp)?).await?;
        let _ = ws_tx.send(Message::Close(None)).await;
        tokio::time::timeout(CONNECT, dgram_open.wait_for(|o| *o)).await.context("Couldn't make a direct connection to this computer.")??;
        tracing::info!("viewer: remote connection up");
        let (out, mut outgoing) = mpsc::unbounded_channel::<Bytes>();
        tokio::spawn(async move {
            while let Some(b) = outgoing.recv().await {
                // Dropped rather than queued when the channel is behind, as the network would.
                if dgram.outstanding_bytes().await.unwrap_or(0) < 256 * 1024 {
                    let _ = dgram.try_send(BytesMut::from(&b[..])).await;
                }
            }
        });
        Ok(Self { pc, out, datagrams: Mutex::new(datagrams), incoming: Mutex::new(channels), state })
    }

    pub fn send_datagram(&self, bytes: &[u8]) {
        let _ = self.out.send(Bytes::copy_from_slice(bytes));
    }

    pub async fn receive_datagram(&self) -> Result<Bytes> {
        let mut state = self.state.clone();
        let mut rx = self.datagrams.lock().await;
        tokio::select! {
            d = rx.recv() => d.flatten().context("connection closed"),
            _ = state.wait_for(|s| matches!(s, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed | RTCPeerConnectionState::Disconnected)) => bail!("connection lost"),
        }
    }

    async fn open(&self, label: &str) -> Result<(RtcTx, RtcRx)> {
        let channel = self.pc.create_data_channel(label, Some(RTCDataChannelInit { ordered: true, ..Default::default() })).await?;
        let (rx, open) = pump(channel.clone());
        Ok((RtcTx { channel, open }, RtcRx { rx, buf: VecDeque::new(), ended: false }))
    }

    pub async fn open_bi(&self) -> Result<(RtcTx, RtcRx)> {
        self.open("bi").await
    }

    pub async fn open_uni(&self) -> Result<RtcTx> {
        Ok(self.open("uni").await?.0)
    }

    pub async fn accept_uni(&self) -> Result<RtcRx> {
        let channel = self.incoming.lock().await.recv().await.context("connection closed")?;
        let (rx, _) = pump(channel);
        Ok(RtcRx { rx, buf: VecDeque::new(), ended: false })
    }

    pub async fn close(&self) {
        let _ = self.pc.close().await;
    }
}
