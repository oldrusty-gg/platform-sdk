//! Transport behind a small trait so it can change (WebSocket, QUIC, a
//! relay) without touching the match logic. Two implementations: framed
//! JSON over TCP (u32 big-endian length + serde_json body, loopback only
//! for now) and an in-memory pair for tests.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, ErrorKind, Read, Write};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Largest accepted frame body (server messages: views and snapshots).
pub const MAX_FRAME: usize = 1 << 20;
/// Largest frame a server accepts from a client: a handshake with a deck,
/// an intention or a streamed press is a few hundred bytes to a few KiB.
pub const MAX_CLIENT_FRAME: usize = 16 * 1024;

#[derive(Debug, PartialEq)]
pub enum Received<T> {
    Message(T),
    /// Nothing waiting right now.
    Empty,
    /// The peer went away (or the connection was closed for a bad frame).
    Closed,
    /// A frame that is not a valid message (the frame was consumed).
    Malformed(String),
}

/// One end of a message connection: sends `Out`, receives `In`.
pub trait Connection<Out, In> {
    fn send(&mut self, message: &Out) -> io::Result<()>;
    /// Send a message already serialised as its JSON body (one body for
    /// many recipients: the lobby's player list). Connections that cannot
    /// say `Unsupported`; the caller then uses [`Connection::send`].
    fn send_serialized(&mut self, body: &[u8]) -> io::Result<()> {
        let _ = body;
        Err(io::Error::new(ErrorKind::Unsupported, "no pre-serialised sends"))
    }
    /// Non-blocking receive.
    fn poll(&mut self) -> Received<In>;
    fn close(&mut self);
}

/// A boxed connection is a connection (clients pick TLS or plain at run time).
impl<Out, In, T: Connection<Out, In> + ?Sized> Connection<Out, In> for Box<T> {
    fn send(&mut self, message: &Out) -> io::Result<()> {
        (**self).send(message)
    }
    fn send_serialized(&mut self, body: &[u8]) -> io::Result<()> {
        (**self).send_serialized(body)
    }
    fn poll(&mut self) -> Received<In> {
        (**self).poll()
    }
    fn close(&mut self) {
        (**self).close()
    }
}

/// Wait up to `timeout` for a message (tests and simple clients).
pub fn recv_timeout<Out, In>(
    conn: &mut impl Connection<Out, In>,
    timeout: Duration,
) -> Received<In> {
    let start = Instant::now();
    loop {
        match conn.poll() {
            Received::Empty if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(1))
            }
            other => return other,
        }
    }
}

/// A byte stream a [`FramedTcp`] can run over: a plain TCP stream
/// (loopback only) or TLS over TCP ([`crate::tls`]). Reads and writes are
/// non-blocking (`WouldBlock` when nothing can move).
pub trait Stream: Read + Write + Send {
    /// Close the connection (TLS: send close_notify first).
    fn shutdown(&mut self);
}

impl Stream for TcpStream {
    fn shutdown(&mut self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
}

/// Framed JSON over a byte stream (TCP by default, or TLS).
pub struct FramedTcp<Out, In, S = TcpStream> {
    stream: Option<S>,
    buffer: Vec<u8>,
    max_frame: usize,
    /// Queued sending ([`FramedTcp::with_send_queue`]): bytes not yet
    /// written, at most this many (`None`: sends wait until written).
    queue_cap: Option<usize>,
    outgoing: VecDeque<u8>,
    /// TLS records written into the session but not yet onto the socket.
    flush_due: bool,
    _types: PhantomData<fn(Out) -> In>,
}

impl<Out: Serialize, In: DeserializeOwned> FramedTcp<Out, In, TcpStream> {
    pub fn new(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self::over(stream))
    }

    /// Connect to a loopback address without encryption (any other host
    /// needs TLS: [`crate::tls::connect`]).
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "an unencrypted connection is only allowed to this computer (loopback); use TLS for other hosts",
            ));
        }
        Self::new(TcpStream::connect(addr)?)
    }
}

impl<Out: Serialize, In: DeserializeOwned, S: Stream> FramedTcp<Out, In, S> {
    /// Frame messages over `stream` (already non-blocking).
    pub fn over(stream: S) -> Self {
        Self {
            stream: Some(stream),
            buffer: Vec::new(),
            max_frame: MAX_FRAME,
            queue_cap: None,
            outgoing: VecDeque::new(),
            flush_due: false,
            _types: PhantomData,
        }
    }

    /// Never wait for a slow peer (a server with many connections on one
    /// thread): a send queues the frame and writes what the socket takes
    /// now; the rest goes out on later sends and polls. A peer that lets
    /// more than `cap` bytes pile up (one frame always fits an empty
    /// queue) is cut off: that send fails and the connection closes.
    pub fn with_send_queue(mut self, cap: usize) -> Self {
        self.queue_cap = Some(cap);
        self
    }

    /// Bytes queued and not yet written.
    pub fn queued(&self) -> usize {
        self.outgoing.len()
    }

    /// Write what the socket takes now, without waiting.
    fn write_queued(&mut self) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Err(io::Error::new(ErrorKind::NotConnected, "closed"));
        };
        while !self.outgoing.is_empty() {
            let (front, _) = self.outgoing.as_slices();
            match stream.write(front) {
                // TLS: the session's own buffer is full for now.
                Ok(0) => break,
                Ok(n) => {
                    self.outgoing.drain(..n);
                    self.flush_due = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        if self.flush_due {
            match stream.flush() {
                Ok(()) => self.flush_due = false,
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Accept frames of at most `max` bytes (a larger one closes the
    /// connection; nothing beyond one frame is buffered).
    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max;
        self
    }

    /// Write one raw frame body (tests send malformed bodies with it).
    pub fn send_raw(&mut self, body: &[u8]) -> io::Result<()> {
        if self.stream.is_none() {
            return Err(io::Error::new(ErrorKind::NotConnected, "closed"));
        }
        let len = u32::try_from(body.len())
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "frame too large"))?;
        let Some(cap) = self.queue_cap else {
            let mut frame = len.to_be_bytes().to_vec();
            frame.extend_from_slice(body);
            let stream = self.stream.as_mut().expect("checked");
            return write_all_nonblocking(stream, &frame);
        };
        if !self.outgoing.is_empty() && self.outgoing.len() + 4 + body.len() > cap {
            self.close();
            return Err(io::Error::new(ErrorKind::TimedOut, "peer not reading (send queue full)"));
        }
        self.outgoing.extend(len.to_be_bytes());
        self.outgoing.extend(body);
        if let Err(e) = self.write_queued() {
            self.outgoing.clear();
            self.close();
            return Err(e);
        }
        Ok(())
    }

    fn fill(&mut self) -> bool {
        let Some(stream) = self.stream.as_mut() else {
            return false;
        };
        let mut chunk = [0u8; 16 * 1024];
        // Never buffer more than one maximal frame (and its header): a
        // peer cannot make the reader hold more than that.
        while self.buffer.len() <= self.max_frame + 4 {
            match stream.read(&mut chunk) {
                Ok(0) => return false,
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        true
    }
}

fn write_all_nonblocking(stream: &mut impl Write, mut data: &[u8]) -> io::Result<()> {
    let start = Instant::now();
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return Err(io::Error::new(ErrorKind::WriteZero, "peer closed")),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if start.elapsed() > Duration::from_secs(10) {
                    return Err(io::Error::new(ErrorKind::TimedOut, "peer not reading"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    // TLS keeps records it could not write yet; push them out now.
    loop {
        match stream.flush() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if start.elapsed() > Duration::from_secs(10) {
                    return Err(io::Error::new(ErrorKind::TimedOut, "peer not reading"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

impl<Out: Serialize, In: DeserializeOwned, S: Stream> Connection<Out, In> for FramedTcp<Out, In, S> {
    fn send(&mut self, message: &Out) -> io::Result<()> {
        let body = serde_json::to_vec(message).map_err(io::Error::other)?;
        self.send_raw(&body)
    }

    fn send_serialized(&mut self, body: &[u8]) -> io::Result<()> {
        self.send_raw(body)
    }

    fn poll(&mut self) -> Received<In> {
        if (!self.outgoing.is_empty() || self.flush_due) && self.write_queued().is_err() {
            self.stream = None;
            self.outgoing.clear();
        }
        let open = self.fill();
        if self.buffer.len() >= 4 {
            let len = u32::from_be_bytes(self.buffer[..4].try_into().expect("4 bytes")) as usize;
            if len > self.max_frame {
                self.close();
                self.buffer.clear();
                return Received::Malformed(format!("frame of {len} bytes exceeds {}", self.max_frame));
            }
            if self.buffer.len() >= 4 + len {
                let body: Vec<u8> = self.buffer.drain(..4 + len).skip(4).collect();
                return match serde_json::from_slice(&body) {
                    Ok(m) => Received::Message(m),
                    Err(e) => Received::Malformed(e.to_string()),
                };
            }
        }
        if open && self.stream.is_some() {
            Received::Empty
        } else {
            self.stream = None;
            Received::Closed
        }
    }

    fn close(&mut self) {
        // Whatever the socket takes now still goes out (a refusal, the
        // last message before a server closes); the rest is dropped.
        if !self.outgoing.is_empty() || self.flush_due {
            let _ = self.write_queued();
        }
        self.outgoing.clear();
        if let Some(mut s) = self.stream.take() {
            s.shutdown();
        }
    }
}

/// A listening socket bound to a loopback address.
pub struct LoopbackListener {
    listener: TcpListener,
    max_frame: usize,
}

impl LoopbackListener {
    /// Bind `addr`, which must be a loopback address (port 0 picks one).
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "the server binds loopback addresses only in this milestone",
            ));
        }
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            max_frame: MAX_CLIENT_FRAME,
        })
    }

    /// The frame limit of accepted connections ([`MAX_CLIENT_FRAME`] by
    /// default: the listener is a server's).
    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max;
        self
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// A waiting TCP stream and its peer (the caller frames it).
    pub fn accept_stream(&self) -> io::Result<Option<(TcpStream, SocketAddr)>> {
        match self.listener.accept() {
            Ok(pair) => Ok(Some(pair)),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The configured client frame limit.
    pub fn max_frame(&self) -> usize {
        self.max_frame
    }

    /// A new connection, if one is waiting.
    pub fn accept<Out: Serialize, In: DeserializeOwned>(
        &self,
    ) -> io::Result<Option<FramedTcp<Out, In>>> {
        match self.listener.accept() {
            Ok((stream, _)) => Ok(Some(FramedTcp::new(stream)?.with_max_frame(self.max_frame))),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// In-memory connection (messages still go through serde_json, so tests
/// exercise the wire encoding).
pub struct Memory<Out, In> {
    tx: Option<mpsc::Sender<Vec<u8>>>,
    rx: mpsc::Receiver<Vec<u8>>,
    max_frame: usize,
    _types: PhantomData<fn(Out) -> In>,
}

impl<Out, In> Memory<Out, In> {
    /// The same end with other message types (the wire is JSON, so a
    /// server end can carry an envelope whose variants serialise as the
    /// plain messages, e.g. a game lobby's `ServerFrame`).
    pub fn retype<O2, I2>(self) -> Memory<O2, I2> {
        Memory { tx: self.tx, rx: self.rx, max_frame: self.max_frame, _types: PhantomData }
    }

    /// Refuse (as malformed, closing) received bodies above `max` bytes.
    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max;
        self
    }
}

/// Two connected ends.
pub fn memory_pair<A, B>() -> (Memory<A, B>, Memory<B, A>) {
    let (ta, rb) = mpsc::channel();
    let (tb, ra) = mpsc::channel();
    (
        Memory {
            tx: Some(ta),
            rx: ra,
            max_frame: MAX_FRAME,
            _types: PhantomData,
        },
        Memory {
            tx: Some(tb),
            rx: rb,
            max_frame: MAX_FRAME,
            _types: PhantomData,
        },
    )
}

impl<Out: Serialize, In: DeserializeOwned> Connection<Out, In> for Memory<Out, In> {
    fn send(&mut self, message: &Out) -> io::Result<()> {
        let body = serde_json::to_vec(message).map_err(io::Error::other)?;
        self.tx
            .as_ref()
            .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "closed"))?
            .send(body)
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "peer gone"))
    }

    fn send_serialized(&mut self, body: &[u8]) -> io::Result<()> {
        self.tx
            .as_ref()
            .ok_or_else(|| io::Error::new(ErrorKind::NotConnected, "closed"))?
            .send(body.to_vec())
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "peer gone"))
    }

    fn poll(&mut self) -> Received<In> {
        match self.rx.try_recv() {
            Ok(body) if body.len() > self.max_frame => {
                self.tx = None;
                Received::Malformed(format!("frame of {} bytes exceeds {}", body.len(), self.max_frame))
            }
            Ok(body) => match serde_json::from_slice(&body) {
                Ok(m) => Received::Message(m),
                Err(e) => Received::Malformed(e.to_string()),
            },
            Err(mpsc::TryRecvError::Empty) if self.tx.is_some() => Received::Empty,
            Err(_) => Received::Closed,
        }
    }

    fn close(&mut self) {
        self.tx = None;
    }
}
