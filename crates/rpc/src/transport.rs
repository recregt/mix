use std::future::Future;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::{Stream, StreamExt};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tonic::transport::server::{Connected, UdsConnectInfo};
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tonic::{Request, Response, Status};

use crate::convert::{self, Malformed};
use crate::proto;
use crate::proto::worker_client::WorkerClient;
use crate::proto::worker_server::WorkerServer;
use crate::types::{BootstrapRequest, Caller, Event, Outcome, RepairRequest};

const WORKER_URI: &str = "http://worker";

pub type Events = mpsc::UnboundedSender<Event>;

pub trait Worker: Send + Sync + 'static {
    fn bootstrap(
        &self,
        caller: Caller,
        request: BootstrapRequest,
        events: Events,
    ) -> impl Future<Output = Outcome> + Send;

    fn repair(
        &self,
        caller: Caller,
        request: RepairRequest,
        events: Events,
    ) -> impl Future<Output = Outcome> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not start the privileged worker: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("could not reach the privileged worker: {0}")]
    Connect(String),

    #[error("the privileged worker refused the request: {0}")]
    Refused(String),

    #[error("the privileged worker stopped before it finished")]
    Ended,

    #[error(transparent)]
    Malformed(#[from] Malformed),

    #[error("stdin is not the connection to a client: {0}")]
    NotAConnection(#[source] std::io::Error),
}

type WireEvents = Pin<Box<dyn Stream<Item = Result<proto::Event, Status>> + Send>>;

struct Service<W>(Arc<W>);

fn caller<T>(request: &Request<T>) -> Result<Caller, Status> {
    request
        .extensions()
        .get::<UdsConnectInfo>()
        .and_then(|info| info.peer_cred)
        .map(|cred| Caller {
            uid: cred.uid(),
            gid: cred.gid(),
        })
        .ok_or_else(|| Status::unauthenticated("the caller could not be identified"))
}

fn stream(events: mpsc::UnboundedReceiver<Event>) -> WireEvents {
    Box::pin(
        futures_util::stream::unfold(events, |mut events| async move {
            events.recv().await.map(|event| (event, events))
        })
        .map(|event| Ok(convert::event_to_wire(event))),
    )
}

fn malformed(error: Malformed) -> Status {
    Status::invalid_argument(error.to_string())
}

#[tonic::async_trait]
impl<W: Worker> proto::worker_server::Worker for Service<W> {
    type BootstrapStream = WireEvents;
    type RepairStream = WireEvents;

    async fn bootstrap(
        &self,
        request: Request<proto::BootstrapRequest>,
    ) -> Result<Response<WireEvents>, Status> {
        let caller = caller(&request)?;
        let request =
            convert::bootstrap_request_from_wire(request.into_inner()).map_err(malformed)?;
        let (events, received) = mpsc::unbounded_channel();
        let worker = Arc::clone(&self.0);
        tokio::spawn(async move {
            let outcome = worker.bootstrap(caller, request, events.clone()).await;
            let _ = events.send(Event::Finished(outcome));
        });
        Ok(Response::new(stream(received)))
    }

    async fn repair(
        &self,
        request: Request<proto::RepairRequest>,
    ) -> Result<Response<WireEvents>, Status> {
        let caller = caller(&request)?;
        let request = convert::repair_request_from_wire(request.into_inner()).map_err(malformed)?;
        let (events, received) = mpsc::unbounded_channel();
        let worker = Arc::clone(&self.0);
        tokio::spawn(async move {
            let outcome = worker.repair(caller, request, events.clone()).await;
            let _ = events.send(Event::Finished(outcome));
        });
        Ok(Response::new(stream(received)))
    }
}

struct OneConnection {
    inner: UnixStream,
    closed: Option<oneshot::Sender<()>>,
}

impl Drop for OneConnection {
    fn drop(&mut self) {
        if let Some(closed) = self.closed.take() {
            let _ = closed.send(());
        }
    }
}

impl Connected for OneConnection {
    type ConnectInfo = UdsConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.inner.connect_info()
    }
}

impl AsyncRead for OneConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for OneConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub async fn serve_connection<W: Worker>(worker: W, connection: UnixStream) -> Result<(), Error> {
    let (closed, on_close) = oneshot::channel();
    let connection = OneConnection {
        inner: connection,
        closed: Some(closed),
    };
    let incoming = futures_util::stream::once(async move { Ok::<_, std::io::Error>(connection) })
        .chain(futures_util::stream::pending());
    Server::builder()
        .add_service(WorkerServer::new(Service(Arc::new(worker))))
        .serve_with_incoming_shutdown(incoming, async {
            let _ = on_close.await;
        })
        .await
        .map_err(|error| Error::Connect(error.to_string()))
}

pub async fn serve_stdin<W: Worker>(worker: W) -> Result<(), Error> {
    let stdin = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map_err(Error::NotAConnection)?;
    serve_connection(worker, unix_stream(stdin).map_err(Error::NotAConnection)?).await
}

fn unix_stream(fd: OwnedFd) -> std::io::Result<UnixStream> {
    let socket = std::os::unix::net::UnixStream::from(fd);
    socket.set_nonblocking(true)?;
    socket.peer_addr()?;
    UnixStream::from_std(socket)
}

pub struct Client {
    inner: WorkerClient<Channel>,
    worker: Option<std::process::Child>,
}

impl Client {
    pub async fn connect(connection: UnixStream) -> Result<Self, Error> {
        let slot = Arc::new(Mutex::new(Some(connection)));
        let channel = Endpoint::from_static(WORKER_URI)
            .connect_with_connector(tower::service_fn(move |_: Uri| {
                let connection = slot.lock().ok().and_then(|mut slot| slot.take());
                async move {
                    connection.map(TokioIo::new).ok_or_else(|| {
                        std::io::Error::other("the worker connection was already used")
                    })
                }
            }))
            .await
            .map_err(|error| Error::Connect(error.to_string()))?;
        Ok(Self {
            inner: WorkerClient::new(channel),
            worker: None,
        })
    }

    pub async fn start(program: &Path, args: &[&str], via: Option<&str>) -> Result<Self, Error> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(Error::Spawn)?;
        let mut command = match via {
            Some(launcher) => {
                let mut command = std::process::Command::new(launcher);
                command.arg(program);
                command
            }
            None => std::process::Command::new(program),
        };
        let worker = command
            .args(args)
            .stdin(std::process::Stdio::from(OwnedFd::from(theirs)))
            .spawn()
            .map_err(Error::Spawn)?;
        ours.set_nonblocking(true).map_err(Error::Spawn)?;
        let mut client = Self::connect(UnixStream::from_std(ours).map_err(Error::Spawn)?).await?;
        client.worker = Some(worker);
        Ok(client)
    }

    pub async fn bootstrap(
        &mut self,
        request: &BootstrapRequest,
    ) -> Result<impl Stream<Item = Result<Event, Error>> + use<>, Error> {
        let events = self
            .inner
            .bootstrap(convert::bootstrap_request_to_wire(request))
            .await
            .map_err(refused)?
            .into_inner();
        Ok(decode(events))
    }

    pub async fn repair(
        &mut self,
        request: &RepairRequest,
    ) -> Result<impl Stream<Item = Result<Event, Error>> + use<>, Error> {
        let events = self
            .inner
            .repair(convert::repair_request_to_wire(request))
            .await
            .map_err(refused)?
            .into_inner();
        Ok(decode(events))
    }

    pub fn wait(mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        drop(self.inner);
        self.worker
            .take()
            .map(|mut worker| worker.wait())
            .transpose()
    }
}

fn refused(status: Status) -> Error {
    Error::Refused(status.message().to_string())
}

fn decode(events: tonic::Streaming<proto::Event>) -> impl Stream<Item = Result<Event, Error>> {
    events.map(|event| match event {
        Ok(event) => Ok(convert::event_from_wire(event)?),
        Err(status) => Err(refused(status)),
    })
}
