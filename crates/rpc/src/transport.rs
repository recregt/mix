use std::future::Future;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use futures_util::{Stream, StreamExt};
use hyper_util::rt::TokioIo;
use mix_events::Normalize;
use mix_events::v1::{Command, Envelope};
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::task::TaskTracker;
use tonic::transport::server::{Connected, UdsConnectInfo};
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tonic::{Request, Response, Status, Streaming};

use crate::proto;
use crate::proto::run_request::Call;
use crate::proto::run_response;
use crate::proto::worker_service_client::WorkerServiceClient;
use crate::proto::worker_service_server::WorkerServiceServer;
use crate::types::{Caller, Control, Malformed, Reply};

const WORKER_URI: &str = "http://worker";

pub const PROTOCOL: u32 = 1;

pub type Events = mpsc::UnboundedSender<Reply>;

pub type Controls = mpsc::UnboundedReceiver<Control>;

pub trait Worker: Send + Sync + 'static {
    const VERSION: &'static str;

    fn admits(&self, caller: Caller) -> bool;

    fn run(
        &self,
        caller: Caller,
        command: Command,
        controls: Controls,
        events: Events,
    ) -> impl Future<Output = ()> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not start the privileged worker: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("could not start the privileged worker")]
    Launch(#[source] mix_exec::Error),

    #[error("could not reach the privileged worker: {0}")]
    Connect(String),

    #[error("the privileged worker refused the request: {0}")]
    Refused(String),

    #[error("the privileged worker stopped before it finished")]
    Ended,

    #[error("the mix daemon does not serve this user: {0}")]
    Denied(String),

    #[error("the privileged worker is mix {theirs}, but this is mix {ours}")]
    VersionMismatch { ours: String, theirs: String },

    #[error(transparent)]
    Malformed(#[from] Malformed),

    #[error("stdin is not the connection to a client: {0}")]
    NotAConnection(#[source] std::io::Error),
}

type Responses = Pin<Box<dyn Stream<Item = Result<proto::RunResponse, Status>> + Send>>;

struct Service<W> {
    worker: Arc<W>,
    running: TaskTracker,
    one_at_a_time: Arc<Semaphore>,
    admitted: OnceLock<(Caller, bool)>,
}

impl<W: Worker> Service<W> {
    fn admits(&self, caller: Caller) -> bool {
        match *self
            .admitted
            .get_or_init(|| (caller, self.worker.admits(caller)))
        {
            (checked, admitted) if checked == caller => admitted,
            _ => self.worker.admits(caller),
        }
    }

    fn admit<T>(&self, request: &Request<T>) -> Result<Caller, Status> {
        let caller = caller(request)?;
        if self.admits(caller) {
            Ok(caller)
        } else {
            Err(Status::permission_denied(format!(
                "uid {} is not root and not a member of mix-users",
                caller.uid
            )))
        }
    }
}

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

async fn command(calls: &mut Streaming<proto::RunRequest>) -> Result<Command, Status> {
    match calls.message().await? {
        Some(proto::RunRequest {
            call: Some(Call::Command(bytes)),
        }) => Command::decode(bytes.as_slice())
            .map_err(|error| Status::invalid_argument(format!("the command: {error}"))),
        Some(_) => Err(Status::invalid_argument(
            "the first message of a request must be its command",
        )),
        None => Err(Status::invalid_argument(
            "the request ended before its command",
        )),
    }
}

fn control_from_wire(control: i32) -> Option<Control> {
    match proto::Control::try_from(control).ok()? {
        proto::Control::Interrupt => Some(Control::Interrupt),
        proto::Control::Terminate => Some(Control::Terminate),
        proto::Control::Pause => Some(Control::Pause),
        proto::Control::Resume => Some(Control::Resume),
        proto::Control::Unspecified => None,
    }
}

fn control_to_wire(control: Control) -> proto::Control {
    match control {
        Control::Interrupt => proto::Control::Interrupt,
        Control::Terminate => proto::Control::Terminate,
        Control::Pause => proto::Control::Pause,
        Control::Resume => proto::Control::Resume,
    }
}

async fn forward_controls(
    mut calls: Streaming<proto::RunRequest>,
    controls: mpsc::UnboundedSender<Control>,
) {
    while let Ok(Some(call)) = calls.message().await {
        if let Some(Call::Control(control)) = call.call
            && let Some(control) = control_from_wire(control)
            && controls.send(control).is_err()
        {
            return;
        }
    }
}

fn reply_to_wire(reply: Reply) -> proto::RunResponse {
    proto::RunResponse {
        reply: Some(match reply {
            Reply::Envelope(envelope) => run_response::Reply::Envelope(envelope.encode_to_vec()),
            Reply::Applied(control) => {
                run_response::Reply::Applied(control_to_wire(control) as i32)
            }
        }),
    }
}

fn reply_from_wire(response: proto::RunResponse) -> Result<Reply, Error> {
    match response.reply {
        Some(run_response::Reply::Envelope(bytes)) => envelope(&bytes).map(Reply::Envelope),
        Some(run_response::Reply::Applied(control)) => control_from_wire(control)
            .map(Reply::Applied)
            .ok_or_else(|| Malformed(format!("an applied control: {control}")).into()),
        None => Err(Malformed("a reply with nothing in it".to_string()).into()),
    }
}

pub struct Controller(mpsc::UnboundedSender<Control>);

impl Controller {
    pub fn send(&self, control: Control) {
        let _ = self.0.send(control);
    }
}

#[tonic::async_trait]
impl<W: Worker> proto::worker_service_server::WorkerService for Service<W> {
    type RunStream = Responses;

    async fn hello(
        &self,
        request: Request<proto::HelloRequest>,
    ) -> Result<Response<proto::HelloResponse>, Status> {
        self.admit(&request)?;
        Ok(Response::new(proto::HelloResponse {
            version: W::VERSION.to_string(),
            protocol: PROTOCOL,
        }))
    }

    async fn run(
        &self,
        request: Request<Streaming<proto::RunRequest>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        let caller = self.admit(&request)?;
        let mut calls = request.into_inner();
        let command = command(&mut calls).await?;
        let permit = Arc::clone(&self.one_at_a_time)
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("the worker is already running a request"))?;
        let (events, received) = mpsc::unbounded_channel();
        let (controls, steering) = mpsc::unbounded_channel();
        tokio::spawn(forward_controls(calls, controls));
        let worker = Arc::clone(&self.worker);
        self.running.spawn(async move {
            worker.run(caller, command, steering, events).await;
            drop(permit);
        });
        let responses = futures_util::stream::unfold(received, |mut received| async move {
            received.recv().await.map(|reply| (reply, received))
        })
        .map(|reply| Ok(reply_to_wire(reply)));
        Ok(Response::new(Box::pin(responses)))
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
    let running = TaskTracker::new();
    let served = Server::builder()
        .add_service(WorkerServiceServer::new(Service {
            worker: Arc::new(worker),
            running: running.clone(),
            one_at_a_time: Arc::new(Semaphore::new(1)),
            admitted: OnceLock::new(),
        }))
        .serve_with_incoming_shutdown(incoming, async {
            let _ = on_close.await;
        })
        .await
        .map_err(|error| Error::Connect(error.to_string()));
    running.close();
    running.wait().await;
    served
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
    inner: WorkerServiceClient<Channel>,
    worker: Option<mix_exec::Foreground>,
}

impl Client {
    pub async fn connect(connection: UnixStream, version: &str) -> Result<Self, Error> {
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
        let mut inner = WorkerServiceClient::new(channel);
        let answer = inner
            .hello(proto::HelloRequest {
                version: version.to_string(),
                protocol: PROTOCOL,
            })
            .await
            .map_err(refused)?
            .into_inner();
        if answer.version != version || answer.protocol != PROTOCOL {
            return Err(Error::VersionMismatch {
                ours: version.to_string(),
                theirs: answer.version,
            });
        }
        Ok(Self {
            inner,
            worker: None,
        })
    }

    pub async fn start(
        program: &Path,
        args: &[&str],
        via: Option<&str>,
        version: &str,
    ) -> Result<Self, Error> {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(Error::Spawn)?;
        let command = match via {
            Some(launcher) => mix_exec::Command::new(launcher).arg(program),
            None => mix_exec::Command::new(program),
        };
        let worker = command
            .args(args)
            .spawn_foreground(OwnedFd::from(theirs))
            .map_err(Error::Launch)?;
        ours.set_nonblocking(true).map_err(Error::Spawn)?;
        let connection = UnixStream::from_std(ours).map_err(Error::Spawn)?;
        let mut client = Self::connect(connection, version).await?;
        client.worker = Some(worker);
        Ok(client)
    }

    pub async fn run(
        &mut self,
        command: &Command,
    ) -> Result<(Controller, impl Stream<Item = Result<Reply, Error>> + use<>), Error> {
        let first = proto::RunRequest {
            call: Some(Call::Command(command.encode_to_vec())),
        };
        let (controls, steering) = mpsc::unbounded_channel();
        let calls = futures_util::stream::once(async move { first }).chain(
            futures_util::stream::unfold(steering, |mut steering| async move {
                steering.recv().await.map(|control: Control| {
                    let call = proto::RunRequest {
                        call: Some(Call::Control(control_to_wire(control) as i32)),
                    };
                    (call, steering)
                })
            }),
        );
        let responses = self.inner.run(calls).await.map_err(refused)?.into_inner();
        Ok((
            Controller(controls),
            responses.map(|response| match response {
                Ok(response) => reply_from_wire(response),
                Err(_) => Err(Error::Ended),
            }),
        ))
    }

    pub async fn wait(mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        drop(self.inner);
        match self.worker.take() {
            Some(mut worker) => worker.wait().await.map(Some),
            None => Ok(None),
        }
    }
}

fn refused(status: Status) -> Error {
    match status.code() {
        tonic::Code::PermissionDenied => Error::Denied(status.message().to_string()),
        _ => Error::Refused(status.message().to_string()),
    }
}

fn envelope(bytes: &[u8]) -> Result<Envelope, Error> {
    let mut envelope = Envelope::decode(bytes)
        .map_err(|error| Malformed(format!("an event envelope: {error}")))?;
    envelope.normalize();
    Ok(envelope)
}
