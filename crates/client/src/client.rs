#[cfg(any(test, feature = "test-support"))]
pub mod test;

pub mod telemetry;
pub mod user;
pub mod zed_urls;

#[cfg(any(test, feature = "test-support"))]
use anyhow::Context as _;
use anyhow::{Result, anyhow};
use clock::SystemClock;
#[cfg(any(test, feature = "test-support"))]
use futures::StreamExt;
use futures::{FutureExt, Stream, TryFutureExt as _, future::BoxFuture, stream::BoxStream};
use gpui::{App, AsyncApp, Entity, Global, Task, WeakEntity, actions};
use http_client::HttpClientWithUrl;
use parking_lot::{Mutex, RwLock};
use postage::watch;
use rpc::proto::{AnyTypedEnvelope, EnvelopedMessage, PeerId, RequestMessage};
use std::{
    any::TypeId,
    future::Future,
    marker::PhantomData,
    path::PathBuf,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use telemetry::Telemetry;
use thiserror::Error;
use util::{ConnectionResult, ResultExt};

pub use rpc::*;
pub use telemetry_events::Event;
pub use user::*;

pub static ZED_APP_PATH: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var("ZED_APP_PATH").ok().map(PathBuf::from));

pub static ZED_ALWAYS_ACTIVE: LazyLock<bool> =
    LazyLock::new(|| std::env::var("ZED_ALWAYS_ACTIVE").is_ok_and(|e| !e.is_empty()));

actions!(
    client,
    [
        /// Signs in to Zed account.
        SignIn,
        /// Signs out of Zed account.
        SignOut,
        /// Reconnects to the collaboration server.
        Reconnect
    ]
);

pub fn init(_client: &Arc<Client>, _cx: &mut App) {}

struct GlobalClient(Arc<Client>);

impl Global for GlobalClient {}

pub struct Client {
    id: AtomicU64,
    sign_in_enabled: AtomicBool,
    peer: Arc<Peer>,
    http: Arc<HttpClientWithUrl>,
    telemetry: Arc<Telemetry>,
    state: RwLock<ClientState>,
    handler_set: Mutex<ProtoMessageHandlerSet>,

    #[allow(clippy::type_complexity)]
    #[cfg(any(test, feature = "test-support"))]
    authenticate:
        RwLock<Option<Box<dyn 'static + Send + Sync + Fn(&AsyncApp) -> Task<Result<Credentials>>>>>,

    #[allow(clippy::type_complexity)]
    #[cfg(any(test, feature = "test-support"))]
    establish_connection: RwLock<
        Option<
            Box<
                dyn 'static
                    + Send
                    + Sync
                    + Fn(
                        &Credentials,
                        &AsyncApp,
                    ) -> Task<Result<Connection, EstablishConnectionError>>,
            >,
        >,
    >,
}

#[derive(Error, Debug)]
pub enum EstablishConnectionError {
    #[error("upgrade required")]
    UpgradeRequired,
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    Other(#[from] anyhow::Error),
    #[error("{0}")]
    InvalidHeaderValue(#[from] http_client::http::header::InvalidHeaderValue),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl EstablishConnectionError {
    pub fn other(error: impl Into<anyhow::Error> + Send + Sync) -> Self {
        Self::Other(error.into())
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Status {
    SignedOut,
    UpgradeRequired,
    Authenticating,
    Authenticated,
    AuthenticationError,
    Connecting,
    ConnectionError,
    Connected {
        peer_id: PeerId,
        connection_id: ConnectionId,
    },
    ConnectionLost,
    Reauthenticating,
    Reauthenticated,
    Reconnecting,
    ReconnectionError {
        next_reconnection: Instant,
    },
}

impl Status {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub fn was_connected(&self) -> bool {
        matches!(
            self,
            Self::ConnectionLost
                | Self::Reauthenticating
                | Self::Reauthenticated
                | Self::Reconnecting
        )
    }

    /// Returns whether the client is currently connected or was connected at some point.
    pub fn is_or_was_connected(&self) -> bool {
        self.is_connected() || self.was_connected()
    }

    pub fn is_signing_in(&self) -> bool {
        matches!(
            self,
            Self::Authenticating | Self::Reauthenticating | Self::Connecting | Self::Reconnecting
        )
    }

    pub fn is_signed_out(&self) -> bool {
        matches!(self, Self::SignedOut | Self::UpgradeRequired)
    }
}

struct ClientState {
    credentials: Option<Credentials>,
    status: (watch::Sender<Status>, watch::Receiver<Status>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credentials {
    pub user_id: u64,
    pub access_token: String,
}

impl Credentials {
    pub fn authorization_header(&self) -> String {
        format!("{} {}", self.user_id, self.access_token)
    }
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            credentials: None,
            status: watch::channel_with(Status::SignedOut),
        }
    }
}

pub enum Subscription {
    Entity {
        client: Weak<Client>,
        id: (TypeId, u64),
    },
    Message {
        client: Weak<Client>,
        id: TypeId,
    },
}

impl Drop for Subscription {
    fn drop(&mut self) {
        match self {
            Subscription::Entity { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entities_by_type_and_remote_id.remove(id);
                }
            }
            Subscription::Message { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entity_types_by_message_type.remove(id);
                    let _ = state.message_handlers.remove(id);
                }
            }
        }
    }
}

pub struct PendingEntitySubscription<T: 'static> {
    client: Arc<Client>,
    remote_id: u64,
    _entity_type: PhantomData<T>,
    consumed: bool,
}

impl<T: 'static> PendingEntitySubscription<T> {
    pub fn set_entity(mut self, entity: &Entity<T>, cx: &AsyncApp) -> Subscription {
        self.consumed = true;
        let mut handlers = self.client.handler_set.lock();
        let id = (TypeId::of::<T>(), self.remote_id);
        let Some(EntityMessageSubscriber::Pending(messages)) =
            handlers.entities_by_type_and_remote_id.remove(&id)
        else {
            unreachable!()
        };

        handlers.entities_by_type_and_remote_id.insert(
            id,
            EntityMessageSubscriber::Entity {
                handle: entity.downgrade().into(),
            },
        );
        drop(handlers);
        for message in messages {
            let client_id = self.client.id();
            let type_name = message.payload_type_name();
            let sender_id = message.original_sender_id();
            log::debug!(
                "handling queued rpc message. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                sender_id,
                type_name
            );
            self.client.handle_message(message, cx);
        }
        Subscription::Entity {
            client: Arc::downgrade(&self.client),
            id,
        }
    }
}

impl<T: 'static> Drop for PendingEntitySubscription<T> {
    fn drop(&mut self) {
        if !self.consumed {
            let mut state = self.client.handler_set.lock();
            if let Some(EntityMessageSubscriber::Pending(messages)) = state
                .entities_by_type_and_remote_id
                .remove(&(TypeId::of::<T>(), self.remote_id))
            {
                for message in messages {
                    log::info!("unhandled message {}", message.payload_type_name());
                }
            }
        }
    }
}

impl Client {
    pub fn new(
        clock: Arc<dyn SystemClock>,
        http: Arc<HttpClientWithUrl>,
        cx: &mut App,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: AtomicU64::new(0),
            sign_in_enabled: AtomicBool::new(cfg!(any(test, feature = "test-support"))),
            peer: Peer::new(0),
            telemetry: Telemetry::new(clock, http.clone(), cx),
            http,
            state: Default::default(),
            handler_set: Default::default(),

            #[cfg(any(test, feature = "test-support"))]
            authenticate: Default::default(),
            #[cfg(any(test, feature = "test-support"))]
            establish_connection: Default::default(),
        })
    }

    pub fn production(cx: &mut App) -> Arc<Self> {
        let clock = Arc::new(clock::RealSystemClock);
        let http = Arc::new(HttpClientWithUrl::new(cx.http_client(), "https://zed.dev"));
        Self::new(clock, http, cx)
    }

    pub fn id(&self) -> u64 {
        self.id.load(Ordering::SeqCst)
    }

    pub fn http_client(&self) -> Arc<HttpClientWithUrl> {
        self.http.clone()
    }

    pub fn set_id(&self, id: u64) -> &Self {
        self.id.store(id, Ordering::SeqCst);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn teardown(&self) {
        self.handler_set.lock().clear();
        self.peer.teardown();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn override_authenticate<F>(&self, authenticate: F) -> &Self
    where
        F: 'static + Send + Sync + Fn(&AsyncApp) -> Task<Result<Credentials>>,
    {
        *self.authenticate.write() = Some(Box::new(authenticate));
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn override_establish_connection<F>(&self, connect: F) -> &Self
    where
        F: 'static
            + Send
            + Sync
            + Fn(&Credentials, &AsyncApp) -> Task<Result<Connection, EstablishConnectionError>>,
    {
        *self.establish_connection.write() = Some(Box::new(connect));
        self
    }

    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalClient>().0.clone()
    }
    pub fn set_global(client: Arc<Client>, cx: &mut App) {
        cx.set_global(GlobalClient(client))
    }

    pub fn user_id(&self) -> Option<u64> {
        self.state
            .read()
            .credentials
            .as_ref()
            .map(|credentials| credentials.user_id)
    }

    pub fn peer_id(&self) -> Option<PeerId> {
        if let Status::Connected { peer_id, .. } = &*self.status().borrow() {
            Some(*peer_id)
        } else {
            None
        }
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.state.read().status.1.clone()
    }

    /// Watches successful cloud websocket reconnections.
    ///
    /// The value is bumped each time the websocket handshake completes. The
    /// initial `0` means no reconnection yet.
    fn set_status(self: &Arc<Self>, status: Status, _cx: &AsyncApp) {
        *self.state.write().status.0.borrow_mut() = status;
    }

    pub fn subscribe_to_entity<T>(
        self: &Arc<Self>,
        remote_id: u64,
    ) -> Result<PendingEntitySubscription<T>>
    where
        T: 'static,
    {
        let id = (TypeId::of::<T>(), remote_id);

        let mut state = self.handler_set.lock();
        anyhow::ensure!(
            !state.entities_by_type_and_remote_id.contains_key(&id),
            "already subscribed to entity"
        );

        state
            .entities_by_type_and_remote_id
            .insert(id, EntityMessageSubscriber::Pending(Default::default()));

        Ok(PendingEntitySubscription {
            client: self.clone(),
            remote_id,
            consumed: false,
            _entity_type: PhantomData,
        })
    }

    #[track_caller]
    pub fn add_message_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        self.add_message_handler_impl(entity, move |entity, message, _, cx| {
            handler(entity, message, cx)
        })
    }

    fn add_message_handler_impl<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static
            + Sync
            + Fn(Entity<E>, TypedEnvelope<M>, AnyProtoClient, AsyncApp) -> F
            + Send
            + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        let message_type_id = TypeId::of::<M>();
        let mut state = self.handler_set.lock();
        state
            .entities_by_message_type
            .insert(message_type_id, entity.into());

        let prev_handler = state.message_handlers.insert(
            message_type_id,
            Arc::new(move |subscriber, envelope, client, cx| {
                let subscriber = subscriber.downcast::<E>().unwrap();
                let envelope = envelope.into_any().downcast::<TypedEnvelope<M>>().unwrap();
                handler(subscriber, *envelope, client, cx).boxed_local()
            }),
        );
        if prev_handler.is_some() {
            let location = std::panic::Location::caller();
            panic!(
                "{}:{} registered handler for the same message {} twice",
                location.file(),
                location.line(),
                std::any::type_name::<M>()
            );
        }

        Subscription::Message {
            client: Arc::downgrade(self),
            id: message_type_id,
        }
    }

    pub fn add_request_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: RequestMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<M::Response>>,
    {
        self.add_message_handler_impl(entity, move |handle, envelope, this, cx| {
            Self::respond_to_request(envelope.receipt(), handler(handle, envelope, cx), this)
        })
    }

    async fn respond_to_request<T: RequestMessage, F: Future<Output = Result<T::Response>>>(
        receipt: Receipt<T>,
        response: F,
        client: AnyProtoClient,
    ) -> Result<()> {
        match response.await {
            Ok(response) => {
                client.send_response(receipt.message_id, response)?;
                Ok(())
            }
            Err(error) => {
                client.send_response(receipt.message_id, error.to_proto())?;
                Err(error)
            }
        }
    }

    pub async fn has_credentials(&self, _cx: &AsyncApp) -> bool {
        false
    }

    pub fn disable_sign_in(&self) {
        self.sign_in_enabled.store(false, Ordering::Release);
    }

    pub async fn sign_in(
        self: &Arc<Self>,
        _try_provider: bool,
        _cx: &AsyncApp,
    ) -> Result<Credentials> {
        anyhow::bail!(
            "Zed accounts are unavailable in ZedStorm. Sign in through Codex CLI instead."
        )
    }

    pub async fn sign_in_with_optional_connect(
        self: &Arc<Self>,
        _try_provider: bool,
        _cx: &AsyncApp,
    ) -> Result<()> {
        anyhow::bail!(
            "Zed accounts are unavailable in ZedStorm. Sign in through Codex CLI instead."
        )
    }

    pub async fn connect(
        self: &Arc<Self>,
        _try_provider: bool,
        cx: &AsyncApp,
    ) -> ConnectionResult<()> {
        #[cfg(any(test, feature = "test-support"))]
        if self.sign_in_enabled.load(Ordering::Acquire) {
            let authenticate = self
                .authenticate
                .read()
                .as_ref()
                .map(|callback| callback(cx));
            if let Some(authenticate) = authenticate {
                let credentials = match authenticate.await {
                    Ok(credentials) => credentials,
                    Err(error) => return ConnectionResult::Result(Err(error)),
                };
                let connection = self
                    .establish_connection
                    .read()
                    .as_ref()
                    .map(|callback| callback(&credentials, cx));
                if let Some(connection) = connection {
                    return ConnectionResult::Result(match connection.await {
                        Ok(connection) => {
                            self.state.write().credentials = Some(credentials);
                            self.set_connection(connection, cx).await
                        }
                        Err(error) => Err(error.into()),
                    });
                }
            }
        }
        let _cx = cx;
        ConnectionResult::Result(Err(anyhow!(
            "Zed server connections are unavailable in ZedStorm."
        )))
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn set_connection(self: &Arc<Self>, conn: Connection, cx: &AsyncApp) -> Result<()> {
        let executor = cx.background_executor();
        log::debug!("add connection to peer");
        let (connection_id, handle_io, mut incoming) = self.peer.add_connection(conn, {
            let executor = executor.clone();
            move |duration| executor.timer(duration)
        });
        let handle_io = executor.spawn(handle_io);

        let peer_id = async {
            log::debug!("waiting for server hello");
            let message = incoming.next().await.context("no hello message received")?;
            log::debug!("got server hello");
            let hello_message_type_name = message.payload_type_name().to_string();
            let hello = message
                .into_any()
                .downcast::<TypedEnvelope<proto::Hello>>()
                .map_err(|_| {
                    anyhow!(
                        "invalid hello message received: {:?}",
                        hello_message_type_name
                    )
                })?;
            let peer_id = hello.payload.peer_id.context("invalid peer id")?;
            Ok(peer_id)
        };

        let peer_id = match peer_id.await {
            Ok(peer_id) => peer_id,
            Err(error) => {
                self.peer.disconnect(connection_id);
                return Err(error);
            }
        };

        log::debug!(
            "set status to connected (connection id: {:?}, peer id: {:?})",
            connection_id,
            peer_id
        );
        self.set_status(
            Status::Connected {
                peer_id,
                connection_id,
            },
            cx,
        );

        cx.spawn({
            let this = self.clone();
            async move |cx| {
                while let Some(message) = incoming.next().await {
                    this.handle_message(message, cx);
                    // Don't starve the main thread when receiving lots of messages at once.
                    smol::future::yield_now().await;
                }
            }
        })
        .detach();

        cx.spawn({
            let this = self.clone();
            async move |cx| match handle_io.await {
                Ok(()) => {
                    if *this.status().borrow()
                        == (Status::Connected {
                            connection_id,
                            peer_id,
                        })
                    {
                        this.set_status(Status::SignedOut, cx);
                    }
                }
                Err(err) => {
                    log::error!("connection error: {:?}", err);
                    this.set_status(Status::ConnectionLost, cx);
                }
            }
        })
        .detach();

        Ok(())
    }

    pub fn authenticate_with_browser(
        self: &Arc<Self>,
        _cx: &AsyncApp,
    ) -> Task<Result<Credentials>> {
        Task::ready(Err(anyhow!(
            "Zed accounts are unavailable in ZedStorm. Sign in through Codex CLI instead."
        )))
    }

    pub async fn sign_out(self: &Arc<Self>, cx: &AsyncApp) {
        self.state.write().credentials = None;
        self.disconnect(cx);
    }

    pub fn request_sign_out(&self) {}

    pub fn disconnect(self: &Arc<Self>, cx: &AsyncApp) {
        self.peer.teardown();
        self.set_status(Status::SignedOut, cx);
    }

    pub fn reconnect(self: &Arc<Self>, cx: &AsyncApp) {
        self.peer.teardown();
        self.set_status(Status::ConnectionLost, cx);
    }

    fn connection_id(&self) -> Result<ConnectionId> {
        if let Status::Connected { connection_id, .. } = *self.status().borrow() {
            Ok(connection_id)
        } else {
            anyhow::bail!("not connected");
        }
    }

    pub fn send<T: EnvelopedMessage>(&self, message: T) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), T::NAME);
        self.peer.send(self.connection_id()?, message)
    }

    pub fn request<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<T::Response>> + use<T> {
        self.request_envelope(request)
            .map_ok(|envelope| envelope.payload)
    }

    pub fn request_stream<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<T::Response>>>> {
        let client_id = self.id.load(Ordering::SeqCst);
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_stream(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_envelope<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<TypedEnvelope<T::Response>>> + use<T> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_envelope(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_dynamic(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> impl Future<Output = Result<proto::Envelope>> + use<> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            request_type
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_dynamic(conn_id, envelope, request_type));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                request_type
            );
            Ok(response?.0)
        }
    }

    fn handle_message(self: &Arc<Client>, message: Box<dyn AnyTypedEnvelope>, cx: &AsyncApp) {
        let sender_id = message.sender_id();
        let request_id = message.message_id();
        let type_name = message.payload_type_name();
        let original_sender_id = message.original_sender_id();

        if let Some(future) = ProtoMessageHandlerSet::handle_message(
            &self.handler_set,
            message,
            self.clone().into(),
            cx.clone(),
        ) {
            let client_id = self.id();
            log::debug!(
                "rpc message received. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                original_sender_id,
                type_name
            );
            cx.spawn(async move |_| match future.await {
                Ok(()) => {
                    log::debug!("rpc message handled. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}");
                }
                Err(error) => {
                    log::error!("error handling message. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}, error:{error:#}");
                }
            })
            .detach();
        } else {
            log::info!("unhandled message {}", type_name);
            self.peer
                .respond_with_unhandled_message(sender_id.into(), request_id, type_name)
                .log_err();
        }
    }

    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }
}

impl ProtoClient for Client {
    fn request(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<proto::Envelope>> {
        self.request_dynamic(envelope, request_type).boxed()
    }

    fn request_stream(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<proto::Envelope>>>> {
        let client_id = self.id();
        let response = self.connection_id().map(|connection_id| {
            self.peer
                .request_stream_dynamic(connection_id, envelope, request_type)
        });

        async move {
            log::debug!(
                "rpc stream request start. client_id:{}. name:{}",
                client_id,
                request_type
            );
            let response = response?.await;
            log::debug!(
                "rpc stream request opened. client_id:{}. name:{}",
                client_id,
                request_type
            );
            response
        }
        .boxed()
    }

    fn send(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), message_type);
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn send_response(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!(
            "rpc respond. client_id:{}, name:{}",
            self.id(),
            message_type
        );
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn message_handler_set(&self) -> &Mutex<ProtoMessageHandlerSet> {
        &self.handler_set
    }

    fn is_via_collab(&self) -> bool {
        true
    }

    fn has_wsl_interop(&self) -> bool {
        false
    }
}

/// prefix for the zed:// url scheme
pub const ZED_URL_SCHEME: &str = "zed";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::FakeServer;

    use clock::FakeSystemClock;
    use gpui::{AppContext as _, TestAppContext};
    use http_client::FakeHttpClient;
    use proto::TypedEnvelope;
    use settings::SettingsStore;

    #[gpui::test]
    async fn test_disabled_sign_in_rejects_all_authentication_routes(cx: &mut TestAppContext) {
        init_test(cx);
        let client = cx.update(|cx| {
            Client::new(
                Arc::new(FakeSystemClock::new()),
                FakeHttpClient::create(|request| async move {
                    panic!("account actions must not make requests: {}", request.uri());
                }),
                cx,
            )
        });
        client.override_authenticate(|_| panic!("disabled sign-in must not authenticate"));
        client.disable_sign_in();
        let async_cx = cx.to_async();
        for try_provider in [false, true] {
            assert!(client.sign_in(try_provider, &async_cx).await.is_err());
            assert!(
                client
                    .sign_in_with_optional_connect(try_provider, &async_cx)
                    .await
                    .is_err()
            );
            assert!(
                client
                    .connect(try_provider, &async_cx)
                    .await
                    .into_response()
                    .is_err()
            );
        }
        assert!(client.authenticate_with_browser(&async_cx).await.is_err());
        assert!(client.status().borrow().is_signed_out());
    }

    #[gpui::test]
    async fn test_subscribing_to_entity(cx: &mut TestAppContext) {
        init_test(cx);
        let user_id = 5;
        let client = cx.update(|cx| {
            Client::new(
                Arc::new(FakeSystemClock::new()),
                FakeHttpClient::with_404_response(),
                cx,
            )
        });
        let server = FakeServer::for_client(user_id, &client, cx).await;

        let (done_tx1, done_rx1) = async_channel::unbounded();
        let (done_tx2, done_rx2) = async_channel::unbounded();
        AnyProtoClient::from(client.clone()).add_entity_message_handler(
            move |entity: Entity<TestEntity>, _: TypedEnvelope<proto::UpdateProject>, cx| {
                match entity.read_with(&cx, |entity, _| entity.id) {
                    1 => done_tx1.try_send(()).unwrap(),
                    2 => done_tx2.try_send(()).unwrap(),
                    _ => unreachable!(),
                }
                async { Ok(()) }
            },
        );
        let entity1 = cx.new(|_| TestEntity {
            id: 1,
            subscription: None,
        });
        let entity2 = cx.new(|_| TestEntity {
            id: 2,
            subscription: None,
        });
        let entity3 = cx.new(|_| TestEntity {
            id: 3,
            subscription: None,
        });

        let _subscription1 = client
            .subscribe_to_entity(1)
            .unwrap()
            .set_entity(&entity1, &cx.to_async());
        let _subscription2 = client
            .subscribe_to_entity(2)
            .unwrap()
            .set_entity(&entity2, &cx.to_async());
        // Ensure dropping a subscription for the same entity type still allows receiving of
        // messages for other entity IDs of the same type.
        let subscription3 = client
            .subscribe_to_entity(3)
            .unwrap()
            .set_entity(&entity3, &cx.to_async());
        drop(subscription3);

        server.send(proto::UpdateProject {
            project_id: 1,
            worktrees: Vec::new(),
        });
        server.send(proto::UpdateProject {
            project_id: 2,
            worktrees: Vec::new(),
        });
        done_rx1.recv().await.unwrap();
        done_rx2.recv().await.unwrap();
    }

    #[gpui::test]
    async fn test_subscribing_after_dropping_subscription(cx: &mut TestAppContext) {
        init_test(cx);
        let user_id = 5;
        let client = cx.update(|cx| {
            Client::new(
                Arc::new(FakeSystemClock::new()),
                FakeHttpClient::with_404_response(),
                cx,
            )
        });
        let server = FakeServer::for_client(user_id, &client, cx).await;

        let entity = cx.new(|_| TestEntity::default());
        let (done_tx1, _done_rx1) = async_channel::unbounded();
        let (done_tx2, done_rx2) = async_channel::unbounded();
        let subscription1 = client.add_message_handler(
            entity.downgrade(),
            move |_, _: TypedEnvelope<proto::Ping>, _| {
                done_tx1.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        drop(subscription1);
        let _subscription2 = client.add_message_handler(
            entity.downgrade(),
            move |_, _: TypedEnvelope<proto::Ping>, _| {
                done_tx2.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        server.send(proto::Ping {});
        done_rx2.recv().await.unwrap();
    }

    #[gpui::test]
    async fn test_dropping_subscription_in_handler(cx: &mut TestAppContext) {
        init_test(cx);
        let user_id = 5;
        let client = cx.update(|cx| {
            Client::new(
                Arc::new(FakeSystemClock::new()),
                FakeHttpClient::with_404_response(),
                cx,
            )
        });
        let server = FakeServer::for_client(user_id, &client, cx).await;

        let entity = cx.new(|_| TestEntity::default());
        let (done_tx, done_rx) = async_channel::unbounded();
        let subscription = client.add_message_handler(
            entity.clone().downgrade(),
            move |entity: Entity<TestEntity>, _: TypedEnvelope<proto::Ping>, mut cx| {
                entity
                    .update(&mut cx, |entity, _| entity.subscription.take())
                    .unwrap();
                done_tx.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        entity.update(cx, |entity, _| {
            entity.subscription = Some(subscription);
        });
        server.send(proto::Ping {});
        done_rx.recv().await.unwrap();
    }

    #[derive(Default)]
    struct TestEntity {
        id: usize,
        subscription: Option<Subscription>,
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }
}
