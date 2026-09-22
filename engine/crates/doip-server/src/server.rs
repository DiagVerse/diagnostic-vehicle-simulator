//! The sockets: UDP discovery on 13400 and TCP diagnostic connections on the same port.
//!
//! This half owns the network and the clock and makes no protocol decisions — everything it
//! sends comes from `DoIpEntity`, which it drives.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use application::ProtocolHandler;
use doip::header::{c_uHeaderLength, HeaderLimits, ReadHeader};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Notify;

use crate::entity::DoIpEntity;

/// How often the connection clock is advanced.
///
/// The shortest deadline that matters is the 2-second initial inactivity timer, so a quarter
/// second is well inside it while costing nothing.
const c_tickInterval: Duration = Duration::from_millis(250);

/// Largest UDP datagram accepted. Discovery messages are tiny; anything larger is not one.
const c_uMaxDatagram: usize = 1024;

/// A running DoIP server, and how to stop it.
pub struct ServerHandle {
    m_taskUdp: tokio::task::JoinHandle<()>,
    m_taskTcp: tokio::task::JoinHandle<()>,
    m_taskTick: tokio::task::JoinHandle<()>,
    /// The address the TCP listener actually bound, which is what a test needs when it asked
    /// for port 0.
    m_tcpAddress: SocketAddr,
}

impl ServerHandle {
    /// The address diagnostic connections are accepted on.
    pub fn TcpAddress(&self) -> SocketAddr {
        self.m_tcpAddress
    }

    /// Stop serving.
    pub fn Stop(self) {
        self.m_taskUdp.abort();
        self.m_taskTcp.abort();
        self.m_taskTick.abort();
    }
}

/// Builds and runs the entity's sockets.
pub struct DoIpServer;

impl DoIpServer {
    /// Start listening.
    ///
    /// Both sockets are bound before anything is spawned, so a caller that gets a handle back
    /// knows the ports are actually held — starting the tasks first would report success for a
    /// server that then failed to bind.
    pub async fn Start(
        arcEntity: Arc<Mutex<DoIpEntity>>,
        bindAddress: SocketAddr,
        protocol: Arc<dyn ProtocolHandler>,
    ) -> std::io::Result<ServerHandle> {
        let tcpListener = TcpListener::bind(bindAddress).await?;
        let tcpAddress = tcpListener.local_addr()?;

        // The discovery socket takes the port the TCP listener ended up on, so a test that asks
        // for an ephemeral port still finds both halves together.
        let mut udpAddress = bindAddress;
        udpAddress.set_port(tcpAddress.port());
        let udpSocket = UdpSocket::bind(udpAddress).await?;
        udpSocket.set_broadcast(true)?;

        tracing::info!(tcp = %tcpAddress, "DoIP entity listening");

        // Shared with every connection task so an elapsed timer can reach the socket it
        // belongs to. Created before the spawns, which both need it.
        let closeSignals: CloseSignals = Arc::new(Mutex::new(BTreeMap::new()));

        let taskUdp = tokio::spawn(RunUdp(Arc::clone(&arcEntity), udpSocket));
        let taskTcp = tokio::spawn(RunTcp(
            Arc::clone(&arcEntity),
            tcpListener,
            Arc::clone(&protocol),
            Arc::clone(&closeSignals),
        ));
        let taskTick = tokio::spawn(RunTick(Arc::clone(&arcEntity), Arc::clone(&closeSignals)));

        Ok(ServerHandle {
            m_taskUdp: taskUdp,
            m_taskTcp: taskTcp,
            m_taskTick: taskTick,
            m_tcpAddress: tcpAddress,
        })
    }
}

/// Answer discovery messages.
async fn RunUdp(arcEntity: Arc<Mutex<DoIpEntity>>, socket: UdpSocket) {
    let mut arrBuffer = vec![0u8; c_uMaxDatagram];

    loop {
        let (uLength, fromAddress) = match socket.recv_from(&mut arrBuffer).await {
            Ok(received) => received,
            Err(error) => {
                tracing::warn!(%error, "the DoIP discovery socket failed");
                return;
            }
        };

        // A packet whose source is itself a broadcast or multicast address is ignored outright
        // — no answer and no negative acknowledgement (REQ 7.DoIP-031 AL).
        if IsBroadcastOrMulticast(&fromAddress) {
            continue;
        }

        let reaction = {
            let entity = arcEntity.lock().expect("DoIP entity mutex poisoned");
            entity.HandleUdp(&arrBuffer[..uLength])
        };

        for vecReply in reaction.m_vecReplies {
            // The answer goes back to the port the request came from, which is the tester's
            // ephemeral one — not to 13400. Only the unsolicited announcement uses 13400.
            if let Err(error) = socket.send_to(&vecReply, fromAddress).await {
                tracing::warn!(%error, peer = %fromAddress, "could not answer a discovery request");
            }
        }
    }
}

/// Accept diagnostic connections.
async fn RunTcp(
    arcEntity: Arc<Mutex<DoIpEntity>>,
    listener: TcpListener,
    protocol: Arc<dyn ProtocolHandler>,
    closeSignals: CloseSignals,
) {
    // Sockets are numbered rather than keyed by peer address: a tester may open several
    // connections from one machine, and the standard keys a connection on the socket, not the
    // host.
    static s_atomicNextSocketId: AtomicU64 = AtomicU64::new(1);

    loop {
        let (stream, peerAddress) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "the DoIP listener failed");
                return;
            }
        };

        let u64Socket = s_atomicNextSocketId.fetch_add(1, Ordering::Relaxed);
        tracing::info!(socket = u64Socket, peer = %peerAddress, "DoIP connection accepted");

        arcEntity
            .lock()
            .expect("DoIP entity mutex poisoned")
            .OpenConnection(u64Socket);

        tokio::spawn(ServeConnection(
            Arc::clone(&arcEntity),
            stream,
            u64Socket,
            Arc::clone(&protocol),
            Arc::clone(&closeSignals),
        ));
    }
}

/// One diagnostic connection, until it closes.
async fn ServeConnection(
    arcEntity: Arc<Mutex<DoIpEntity>>,
    mut stream: TcpStream,
    u64Socket: u64,
    protocol: Arc<dyn ProtocolHandler>,
    closeSignals: CloseSignals,
) {
    let mut vecPending: Vec<u8> = Vec::new();
    let mut arrChunk = vec![0u8; 4096];

    let notifyClose = Arc::new(Notify::new());
    closeSignals
        .lock()
        .expect("DoIP close signals mutex poisoned")
        .insert(u64Socket, Arc::clone(&notifyClose));

    loop {
        let uRead = tokio::select! {
            // Biased so a socket whose timer has already elapsed closes even when the peer is
            // simultaneously sending: the timer decided this connection is over.
            biased;

            _ = notifyClose.notified() => {
                tracing::debug!(socket = u64Socket, "DoIP connection closed by its inactivity timer");
                break;
            }
            result = stream.read(&mut arrChunk) => match result {
                Ok(0) => break,
                Ok(uRead) => uRead,
                Err(error) => {
                    tracing::debug!(%error, socket = u64Socket, "DoIP connection read failed");
                    break;
                }
            },
        };
        vecPending.extend_from_slice(&arrChunk[..uRead]);

        // TCP is a stream: the generic header's length field is the only framing there is, so
        // messages can arrive split across segments or several at once. Both happen with real
        // testers, and assuming one message per segment is a listed trap.
        // Re-read each pass: the limits are settable while the entity is running.
        let limits = arcEntity
            .lock()
            .expect("DoIP entity mutex poisoned")
            .Limits();

        while let Some(uMessageLength) = NextMessageLength(&vecPending, limits) {
            if vecPending.len() < uMessageLength {
                break;
            }
            let vecMessage: Vec<u8> = vecPending.drain(..uMessageLength).collect();

            let reaction = {
                let mut entity = arcEntity.lock().expect("DoIP entity mutex poisoned");
                entity.HandleTcp(u64Socket, &vecMessage, protocol.as_ref())
            };

            for vecReply in &reaction.m_vecReplies {
                if let Err(error) = stream.write_all(vecReply).await {
                    tracing::debug!(%error, socket = u64Socket, "DoIP write failed");
                    CloseConnection(&arcEntity, u64Socket);
                    return;
                }
            }

            if reaction.m_bCloseSocket {
                tracing::info!(
                    socket = u64Socket,
                    "closing the connection as the standard requires"
                );
                let _ = stream.shutdown().await;
                CloseConnection(&arcEntity, u64Socket);
                return;
            }
        }
    }

    tracing::info!(socket = u64Socket, "DoIP connection closed");
    closeSignals
        .lock()
        .expect("DoIP close signals mutex poisoned")
        .remove(&u64Socket);
    CloseConnection(&arcEntity, u64Socket);
}

/// How many bytes the next complete message occupies, if its header has arrived.
///
/// Takes the entity's *configured* limits rather than the defaults. Framing against a different
/// maximum from the one the entity enforces desynchronises the stream: a message inside the
/// advertised size is framed as though it were oversized, only its header is consumed, and the
/// body is then read as the next header — whose first two bytes are a source address rather
/// than a version pair, so the connection dies with a generic header NACK about a message
/// nobody sent.
fn NextMessageLength(vecPending: &[u8], limits: HeaderLimits) -> Option<usize> {
    if vecPending.len() < c_uHeaderLength {
        return None;
    }
    // A header this entity would reject still has a readable length field, and the message has
    // to be consumed before the next one can be found — so the length is taken from the bytes
    // rather than from a successful parse.
    let u32PayloadLength =
        u32::from_be_bytes([vecPending[4], vecPending[5], vecPending[6], vecPending[7]]);

    // Refuse to wait for a body larger than anything this entity accepts; the header handler
    // will reject it, and the connection is not going to recover.
    if u32PayloadLength > limits.m_u32MaxDataSize && ReadHeader(vecPending, limits).is_err() {
        return Some(c_uHeaderLength.min(vecPending.len()));
    }

    Some(c_uHeaderLength + u32PayloadLength as usize)
}

/// How a socket is told to close from outside the task reading it.
///
/// An inactivity timer elapsing has to close the TCP connection, and the task that could close
/// it is parked in `read()` — which a peer that has gone quiet will never return from. Waiting
/// for the read is exactly the mistake: a silent peer is the case the timer exists for. So the
/// reading task selects on this alongside its read.
pub type CloseSignals = Arc<Mutex<BTreeMap<u64, Arc<Notify>>>>;

/// Advance every connection's inactivity clock, closing the sockets that have run out.
async fn RunTick(arcEntity: Arc<Mutex<DoIpEntity>>, closeSignals: CloseSignals) {
    let mut interval = tokio::time::interval(c_tickInterval);

    loop {
        interval.tick().await;
        let vecExpired = arcEntity
            .lock()
            .expect("DoIP entity mutex poisoned")
            .Tick(c_tickInterval.as_millis() as u64);

        for u64Socket in vecExpired {
            // ISO 13400-2 REQ 3.DoIP-086 and 3.DoIP-082: an elapsed inactivity timer closes the
            // socket and returns it to the listen state. Dropping the entry alone used to leave
            // the TCP connection ESTABLISHED and permanently deaf — every later message found no
            // connection and was answered with silence, so a tester that activated routing a
            // second too late waited out its own timeout with no FIN to tell it to reconnect.
            let optNotify = closeSignals
                .lock()
                .expect("DoIP close signals mutex poisoned")
                .remove(&u64Socket);

            if let Some(notify) = optNotify {
                tracing::info!(socket = u64Socket, "closing an inactive DoIP connection");
                notify.notify_one();
            }
        }
    }
}

fn CloseConnection(arcEntity: &Arc<Mutex<DoIpEntity>>, u64Socket: u64) {
    arcEntity
        .lock()
        .expect("DoIP entity mutex poisoned")
        .CloseConnection(u64Socket);
}

/// True when a packet's source address is one no host legitimately sends from.
fn IsBroadcastOrMulticast(address: &SocketAddr) -> bool {
    match address {
        SocketAddr::V4(v4) => v4.ip().is_broadcast() || v4.ip().is_multicast(),
        SocketAddr::V6(v6) => v6.ip().is_multicast(),
    }
}
