// Significant part of this code is licensed by the MIT License by tokio-tungstenite authors.
// https://github.com/snapview/tokio-tungstenite

// Copyright (c) 2017 Daniel Abramov
// Copyright (c) 2017 Alexey Galakhov

// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:

// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.

// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

use crate::BoxError;
use futures::{Sink, SinkExt, Stream, StreamExt, TryStreamExt};
use std::marker::PhantomData;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite;
use tungstenite::protocol::frame::coding::CloseCode as TungsteniteCloseCode;

#[derive(Debug)]
struct WebSocketSinkActor<M, S>
where
    M: From<RawMessage>,
    S: Sink<M, Error = BoxError> + Unpin,
{
    receiver: mpsc::UnboundedReceiver<RawMessage>,
    sink: S,
    phantom: PhantomData<M>,
}

impl<M, S> WebSocketSinkActor<M, S>
where
    M: From<RawMessage>,
    S: Sink<M, Error = BoxError> + Unpin,
{
    async fn run(&mut self) -> Result<(), BoxError> {
        while let Some(message) = self.receiver.recv().await {
            self.sink.send(M::from(message)).await?
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WebSocketSink {
    sender: mpsc::UnboundedSender<RawMessage>,
}

impl WebSocketSink {
    pub fn new<M, S>(sink: S) -> Self
    where
        M: From<RawMessage> + Send + 'static,
        S: Sink<M, Error = BoxError> + Unpin + Send + 'static,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut actor = WebSocketSinkActor {
            receiver,
            sink,
            phantom: Default::default(),
        };
        tokio::spawn(async move { actor.run().await.unwrap() });
        Self { sender }
    }

    pub async fn send(&self, message: RawMessage) {
        self.sender.send(message).unwrap();
    }
}

#[derive(Debug)]
struct WebSocketStreamActor<M, S>
where
    M: Into<RawMessage>,
    S: Stream<Item = Result<M, BoxError>> + Unpin,
{
    receiver: mpsc::UnboundedReceiver<oneshot::Sender<Option<RawMessage>>>,
    stream: S,
}

impl<M, S> WebSocketStreamActor<M, S>
where
    M: Into<RawMessage> + std::fmt::Debug,
    S: Stream<Item = Result<M, BoxError>> + Unpin,
{
    async fn run(&mut self) -> Result<(), BoxError> {
        while let Some(respond_to) = self.receiver.recv().await {
            let message = self.stream.next().await.transpose()?;
            if !respond_to.is_closed() {
                respond_to.send(message.map(M::into)).unwrap()
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WebSocketStream {
    sender: mpsc::UnboundedSender<oneshot::Sender<Option<RawMessage>>>,
}

impl WebSocketStream {
    pub fn new<M, S>(stream: S) -> Self
    where
        M: Into<RawMessage> + std::fmt::Debug + Send + 'static,
        S: Stream<Item = Result<M, BoxError>> + Unpin + Send + 'static,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut actor = WebSocketStreamActor { receiver, stream };
        tokio::spawn(async move { actor.run().await.unwrap() });
        Self { sender }
    }

    pub async fn recv(&self) -> Option<RawMessage> {
        let (sender, receiver) = oneshot::channel();
        self.sender.send(sender).unwrap();
        receiver.await.unwrap()
    }
}

#[derive(Debug)]
pub struct WebSocket {
    sink: WebSocketSink,
    stream: WebSocketStream,
}

impl WebSocket {
    pub fn new<M, E: std::error::Error, S>(socket: S) -> Self
    where
        M: Into<RawMessage> + From<RawMessage> + std::fmt::Debug + Send + 'static,
        E: Into<BoxError>,
        S: Sink<M, Error = E> + Unpin + Stream<Item = Result<M, E>> + Unpin + Send + 'static,
    {
        let (sink, stream) = socket.sink_err_into().err_into().split();
        let (sink, stream) = (WebSocketSink::new(sink), WebSocketStream::new(stream));
        Self { sink: sink, stream }
    }

    pub async fn send(&self, message: RawMessage) {
        self.sink.send(message).await;
    }

    pub async fn recv(&self) -> Option<RawMessage> {
        self.stream.recv().await
    }
}

#[derive(Debug, Clone)]
pub struct CloseFrame {
    pub code: CloseCode,
    pub reason: String,
}

impl<'t> From<tungstenite::protocol::CloseFrame<'t>> for CloseFrame {
    fn from(frame: tungstenite::protocol::CloseFrame) -> Self {
        Self {
            code: frame.code.into(),
            reason: frame.reason.into(),
        }
    }
}

impl<'t> From<CloseFrame> for tungstenite::protocol::CloseFrame<'t> {
    fn from(frame: CloseFrame) -> Self {
        Self {
            code: frame.code.into(),
            reason: frame.reason.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CloseCode {
    /// Indicates a normal closure, meaning that the purpose for
    /// which the connection was established has been fulfilled.
    Normal,
    /// Indicates that an endpoint is "going away", such as a server
    /// going down or a browser having navigated away from a page.
    Away,
    /// Indicates that an endpoint is terminating the connection due
    /// to a protocol error.
    Protocol,
    /// Indicates that an endpoint is terminating the connection
    /// because it has received a type of data it cannot accept (e.g., an
    /// endpoint that understands only text data MAY send this if it
    /// receives a binary message).
    Unsupported,
    /// Indicates that no status code was included in a closing frame. This
    /// close code makes it possible to use a single method, `on_close` to
    /// handle even cases where no close code was provided.
    Status,
    /// Indicates an abnormal closure. If the abnormal closure was due to an
    /// error, this close code will not be used. Instead, the `on_error` method
    /// of the handler will be called with the error. However, if the connection
    /// is simply dropped, without an error, this close code will be sent to the
    /// handler.
    Abnormal,
    /// Indicates that an endpoint is terminating the connection
    /// because it has received data within a message that was not
    /// consistent with the type of the message (e.g., non-UTF-8 \[RFC3629\]
    /// data within a text message).
    Invalid,
    /// Indicates that an endpoint is terminating the connection
    /// because it has received a message that violates its policy.  This
    /// is a generic status code that can be returned when there is no
    /// other more suitable status code (e.g., Unsupported or Size) or if there
    /// is a need to hide specific details about the policy.
    Policy,
    /// Indicates that an endpoint is terminating the connection
    /// because it has received a message that is too big for it to
    /// process.
    Size,
    /// Indicates that an endpoint (client) is terminating the
    /// connection because it has expected the server to negotiate one or
    /// more extension, but the server didn't return them in the response
    /// message of the WebSocket handshake.  The list of extensions that
    /// are needed should be given as the reason for closing.
    /// Note that this status code is not used by the server, because it
    /// can fail the WebSocket handshake instead.
    Extension,
    /// Indicates that a server is terminating the connection because
    /// it encountered an unexpected condition that prevented it from
    /// fulfilling the request.
    Error,
    /// Indicates that the server is restarting. A client may choose to reconnect,
    /// and if it does, it should use a randomized delay of 5-30 seconds between attempts.
    Restart,
    /// Indicates that the server is overloaded and the client should either connect
    /// to a different IP (when multiple targets exist), or reconnect to the same IP
    /// when a user has performed an action.
    Again,
    #[doc(hidden)]
    Tls,
    #[doc(hidden)]
    Reserved(u16),
    #[doc(hidden)]
    Iana(u16),
    #[doc(hidden)]
    Library(u16),
    #[doc(hidden)]
    Bad(u16),
}

impl From<CloseCode> for TungsteniteCloseCode {
    fn from(code: CloseCode) -> Self {
        match code {
            CloseCode::Normal => Self::Normal,
            CloseCode::Away => Self::Away,
            CloseCode::Protocol => Self::Protocol,
            CloseCode::Unsupported => Self::Unsupported,
            CloseCode::Status => Self::Status,
            CloseCode::Abnormal => Self::Abnormal,
            CloseCode::Invalid => Self::Invalid,
            CloseCode::Policy => Self::Policy,
            CloseCode::Size => Self::Size,
            CloseCode::Extension => Self::Extension,
            CloseCode::Error => Self::Error,
            CloseCode::Restart => Self::Restart,
            CloseCode::Again => Self::Again,
            CloseCode::Tls => Self::Tls,
            CloseCode::Reserved(v) => Self::Reserved(v),
            CloseCode::Iana(v) => Self::Iana(v),
            CloseCode::Library(v) => Self::Library(v),
            CloseCode::Bad(v) => Self::Bad(v),
        }
    }
}

impl From<TungsteniteCloseCode> for CloseCode {
    fn from(code: TungsteniteCloseCode) -> Self {
        match code {
            TungsteniteCloseCode::Normal => Self::Normal,
            TungsteniteCloseCode::Away => Self::Away,
            TungsteniteCloseCode::Protocol => Self::Protocol,
            TungsteniteCloseCode::Unsupported => Self::Unsupported,
            TungsteniteCloseCode::Status => Self::Status,
            TungsteniteCloseCode::Abnormal => Self::Abnormal,
            TungsteniteCloseCode::Invalid => Self::Invalid,
            TungsteniteCloseCode::Policy => Self::Policy,
            TungsteniteCloseCode::Size => Self::Size,
            TungsteniteCloseCode::Extension => Self::Extension,
            TungsteniteCloseCode::Error => Self::Error,
            TungsteniteCloseCode::Restart => Self::Restart,
            TungsteniteCloseCode::Again => Self::Again,
            TungsteniteCloseCode::Tls => Self::Tls,
            TungsteniteCloseCode::Reserved(v) => Self::Reserved(v),
            TungsteniteCloseCode::Iana(v) => Self::Iana(v),
            TungsteniteCloseCode::Library(v) => Self::Library(v),
            TungsteniteCloseCode::Bad(v) => Self::Bad(v),
        }
    }
}

#[derive(Debug)]
pub enum RawMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(Option<CloseFrame>),
}

impl From<Message> for RawMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::Text(text) => Self::Text(text),
            Message::Binary(bytes) => Self::Binary(bytes),
            Message::Close(frame) => Self::Close(frame.map(CloseFrame::from)),
        }
    }
}

impl From<tungstenite::Message> for RawMessage {
    fn from(message: tungstenite::Message) -> Self {
        match message {
            tungstenite::Message::Text(text) => Self::Text(text),
            tungstenite::Message::Binary(bytes) => Self::Binary(bytes),
            tungstenite::Message::Ping(bytes) => Self::Ping(bytes),
            tungstenite::Message::Pong(bytes) => Self::Pong(bytes),
            tungstenite::Message::Close(frame) => Self::Close(frame.map(CloseFrame::from)),
            tungstenite::Message::Frame(_) => unreachable!(),
        }
    }
}

impl From<RawMessage> for tungstenite::Message {
    fn from(message: RawMessage) -> Self {
        match message {
            RawMessage::Text(text) => Self::Text(text),
            RawMessage::Binary(bytes) => Self::Binary(bytes),
            RawMessage::Ping(bytes) => Self::Ping(bytes),
            RawMessage::Pong(bytes) => Self::Pong(bytes),
            RawMessage::Close(frame) => Self::Close(frame.map(CloseFrame::into)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<CloseFrame>),
}

impl From<Message> for tungstenite::Message {
    fn from(message: Message) -> Self {
        match message {
            Message::Text(text) => tungstenite::Message::Text(text),
            Message::Binary(bytes) => tungstenite::Message::Binary(bytes),
            Message::Close(frame) => tungstenite::Message::Close(frame.map(CloseFrame::into)),
        }
    }
}
