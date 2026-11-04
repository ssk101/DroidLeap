//! Linux capture: the desktop's InputCapture portal watches the edge through
//! pointer barriers, and libei delivers the held input.
//!
//! The portal session runs in a local task: its signal streams borrow the
//! portal proxy, so they can't live in a struct next to it.

use std::{
    fs,
    io::Write,
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    path::Path,
    pin::pin,
    time::Duration,
};

use ashpd::desktop::{
    PersistMode, Session,
    input_capture::{
        ActivatedBarrier, Barrier, BarrierID, BarrierPosition, Capabilities, CreateSessionOptions,
        InputCapture, ReleaseOptions, StartOptions,
    },
};
use futures_util::StreamExt;
use reis::{
    ei::{self, button::ButtonState, keyboard::KeyState},
    event::{DeviceCapability, EiEvent},
};
use tokio::{sync::mpsc, task::JoinHandle};

use super::{CaptureError, Input};
use crate::{
    config::Paths,
    geometry::{EdgeBarrier, Rect, Side, outer_barriers},
    log,
};

const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

enum Request {
    SetEdge(Side),
    SetActive(bool),
    Release((f64, f64)),
    Close,
}

type Inputs = mpsc::UnboundedSender<Result<Input, CaptureError>>;

pub struct Capture {
    inputs: mpsc::UnboundedReceiver<Result<Input, CaptureError>>,
    requests: mpsc::UnboundedSender<Request>,
    task: JoinHandle<()>,
}

impl Capture {
    /// Needs a [`tokio::task::LocalSet`].
    pub fn open(paths: &Paths) -> Capture {
        let (input_sender, inputs) = mpsc::unbounded_channel();
        let (requests, request_receiver) = mpsc::unbounded_channel();
        let token_file = paths.restore_token_file.clone();
        let task = tokio::task::spawn_local(async move {
            if let Err(e) = run(&token_file, request_receiver, &input_sender).await {
                let _ = input_sender.send(Err(e)); // nobody left to tell if the engine has gone
            }
        });
        Capture {
            inputs,
            requests,
            task,
        }
    }

    pub fn set_edge(&self, side: Side) {
        self.request(Request::SetEdge(side));
    }

    pub fn set_active(&self, active: bool) {
        self.request(Request::SetActive(active));
    }

    pub fn release(&self, to: (f64, f64)) {
        self.request(Request::Release(to));
    }

    pub async fn next(&mut self) -> Result<Input, CaptureError> {
        match self.inputs.recv().await {
            Some(input) => input,
            None => Err(CaptureError::Failed("input capture stopped".into())),
        }
    }

    pub async fn close(mut self) {
        self.request(Request::Close);
        if tokio::time::timeout(CLOSE_TIMEOUT, &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }

    fn request(&self, request: Request) {
        // Fails only once the session has ended; `next` reports why.
        let _ = self.requests.send(request);
    }
}

async fn run(
    token_file: &Path,
    requests: mpsc::UnboundedReceiver<Request>,
    inputs: &Inputs,
) -> Result<(), CaptureError> {
    let bus = ashpd::zbus::Connection::session()
        .await
        .map_err(|e| format!("D-Bus: {e}"))?;
    let portal = InputCapture::with_connection(bus)
        .await
        .map_err(|e| format!("InputCapture portal unavailable: {e}"))?;
    let session = open_session(&portal, token_file).await?;
    let result = serve(&portal, &session, requests, inputs).await;
    if let Err(e) = session.close().await {
        log(format!("could not close the input capture session: {e}"));
    }
    result
}

/// Portal state for one session.
struct Barriers<'p> {
    portal: &'p InputCapture,
    session: &'p Session<InputCapture>,
    side: Option<Side>,
    barriers: Vec<EdgeBarrier>,
    active: bool,
    enabled: bool,
}

impl Barriers<'_> {
    /// Puts barriers on the watched edge of the current monitors. Returns how many.
    async fn update(&mut self) -> Result<usize, CaptureError> {
        let Some(side) = self.side else {
            return Ok(0);
        };
        if self.enabled {
            self.portal
                .disable(self.session, Default::default())
                .await
                .map_err(|e| format!("disable: {e}"))?;
            self.enabled = false;
        }
        let zones = self
            .portal
            .zones(self.session, Default::default())
            .await
            .and_then(|r| r.response())
            .map_err(|e| format!("zones: {e}"))?;
        let monitors: Vec<Rect> = zones
            .regions()
            .iter()
            .map(|r| Rect {
                x: r.x_offset(),
                y: r.y_offset(),
                width: r.width() as i32,
                height: r.height() as i32,
            })
            .collect();
        let mut barriers = outer_barriers(&monitors, side);
        let portal_barriers: Vec<Barrier> = barriers
            .iter()
            .map(|b| {
                let (x1, y1, x2, y2) = b.line;
                Barrier::new(
                    BarrierID::new(b.id).expect("ids start at 1"),
                    BarrierPosition::new(x1, y1, x2, y2),
                )
            })
            .collect();
        let response = self
            .portal
            .set_pointer_barriers(
                self.session,
                &portal_barriers,
                zones.zone_set(),
                Default::default(),
            )
            .await
            .and_then(|r| r.response())
            .map_err(|e| format!("set_pointer_barriers: {e}"))?;
        let failed: Vec<u32> = response
            .failed_barriers()
            .iter()
            .map(|id| id.get())
            .collect();
        if !failed.is_empty() {
            log(format!(
                "the desktop rejected {} of {} barriers",
                failed.len(),
                barriers.len()
            ));
        }
        barriers.retain(|b| !failed.contains(&b.id));
        self.barriers = barriers;
        self.sync().await?;
        Ok(self.barriers.len())
    }

    async fn set_active(&mut self, active: bool) -> Result<(), CaptureError> {
        self.active = active;
        self.sync().await
    }

    async fn sync(&mut self) -> Result<(), CaptureError> {
        let wanted = self.active && !self.barriers.is_empty();
        if wanted == self.enabled {
            return Ok(());
        }
        if wanted {
            self.portal
                .enable(self.session, Default::default())
                .await
                .map_err(|e| format!("enable: {e}"))?;
        } else {
            self.portal
                .disable(self.session, Default::default())
                .await
                .map_err(|e| format!("disable: {e}"))?;
        }
        self.enabled = wanted;
        Ok(())
    }

    /// The monitor behind the barrier the pointer hit; any watched one if the portal didn't say.
    fn monitor_of(&self, barrier: Option<ActivatedBarrier>) -> Option<Rect> {
        let hit = match barrier {
            Some(ActivatedBarrier::Barrier(id)) => self.barriers.iter().find(|b| b.id == id.get()),
            _ => None,
        };
        hit.or(self.barriers.first()).map(|b| b.monitor)
    }
}

async fn serve(
    portal: &InputCapture,
    session: &Session<InputCapture>,
    mut requests: mpsc::UnboundedReceiver<Request>,
    inputs: &Inputs,
) -> Result<(), CaptureError> {
    let eis = portal
        .connect_to_eis(session, Default::default())
        .await
        .map_err(|e| format!("connect_to_eis: {e}"))?;
    let stream = UnixStream::from(eis);
    stream
        .set_nonblocking(true)
        .map_err(|e| format!("libei socket: {e}"))?;
    let context = ei::Context::new(stream).map_err(|e| format!("libei: {e}"))?;
    context.flush().map_err(|e| format!("libei: {e}"))?;
    let (_connection, events) = context
        .handshake_tokio("tablet-edge", ei::handshake::ContextType::Receiver)
        .await
        .map_err(|e| format!("libei handshake: {e}"))?;

    let signal_error = |e: ashpd::Error| format!("portal signals: {e}");
    let mut activated = pin!(portal.receive_activated().await.map_err(signal_error)?);
    let mut deactivated = pin!(portal.receive_deactivated().await.map_err(signal_error)?);
    let mut disabled = pin!(portal.receive_disabled().await.map_err(signal_error)?);
    let mut zones_changed = pin!(portal.receive_zones_changed().await.map_err(signal_error)?);
    let mut closed = pin!(session.receive_closed().await.map_err(signal_error)?);
    let mut events = pin!(events);

    let mut barriers = Barriers {
        portal,
        session,
        side: None,
        barriers: Vec::new(),
        active: false,
        enabled: false,
    };
    let mut activation: Option<u32> = None;
    let send = |input: Input| {
        inputs
            .send(Ok(input))
            .map_err(|_| CaptureError::Failed("engine stopped".into()))
    };

    loop {
        tokio::select! {
            request = requests.recv() => match request {
                None | Some(Request::Close) => return Ok(()),
                Some(Request::SetEdge(side)) => {
                    barriers.side = Some(side);
                    send(Input::Watching(barriers.update().await?))?;
                }
                Some(Request::SetActive(active)) => barriers.set_active(active).await?,
                Some(Request::Release(to)) => {
                    if let Some(activation_id) = activation.take() {
                        let options = ReleaseOptions::default().set_activation_id(activation_id).set_cursor_position(to);
                        if let Err(e) = portal.release(session, options).await {
                            log(format!("release: {e}"));
                        }
                    }
                }
            },
            Some(a) = activated.next() => match (a.activation_id(), a.cursor_position(), barriers.monitor_of(a.barrier_id())) {
                (Some(id), Some((x, y)), Some(monitor)) => {
                    activation = Some(id);
                    send(Input::EdgeTouched { position: (x as f64, y as f64), monitor })?;
                }
                _ => log(format!("activation without id, position or barrier, ignored: {a:?}")),
            },
            Some(_) = deactivated.next() => {
                if activation.take().is_some() {
                    send(Input::Released)?;
                }
            }
            Some(_) = disabled.next() => return Err("the desktop disabled input capture".to_string().into()),
            Some(_) = closed.next() => return Err("the desktop closed the input capture session".to_string().into()),
            Some(_) = zones_changed.next() => {
                send(Input::MonitorsChanged)?;
                send(Input::Watching(barriers.update().await?))?;
            }
            event = events.next() => match event {
                Some(Ok(event)) => {
                    if let Some(input) = translate(event, &context)? {
                        send(input)?;
                    }
                }
                Some(Err(e)) => return Err(format!("libei: {e}").into()),
                None => return Err("libei connection closed".to_string().into()),
            },
        }
    }
}

fn translate(event: EiEvent, context: &ei::Context) -> Result<Option<Input>, CaptureError> {
    let input = match event {
        EiEvent::SeatAdded(seat) => {
            seat.seat.bind_capabilities(
                DeviceCapability::Pointer
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll
                    | DeviceCapability::Keyboard,
            );
            context.flush().map_err(|e| format!("libei: {e}"))?;
            return Ok(None);
        }
        EiEvent::PointerMotion(m) => Input::Motion {
            dx: m.dx as f64,
            dy: m.dy as f64,
        },
        EiEvent::Button(b) => Input::Button {
            code: b.button,
            pressed: b.state == ButtonState::Press,
        },
        EiEvent::ScrollDelta(s) => Input::ScrollPixels {
            dx: s.dx as f64,
            dy: s.dy as f64,
        },
        EiEvent::ScrollDiscrete(s) => Input::ScrollSteps {
            dx: s.discrete_dx,
            dy: s.discrete_dy,
        },
        EiEvent::KeyboardKey(k) => Input::Key {
            code: k.key,
            pressed: k.state == KeyState::Press,
        },
        EiEvent::Frame(_) => Input::Frame,
        _ => return Ok(None),
    };
    Ok(Some(input))
}

/// Creates a portal session, reusing a saved permission when the desktop supports that.
async fn open_session(
    portal: &InputCapture,
    token_file: &Path,
) -> Result<Session<InputCapture>, CaptureError> {
    let capabilities = Capabilities::Pointer | Capabilities::Keyboard;
    let failed = |e: ashpd::Error| match e {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => CaptureError::Denied,
        e => CaptureError::Failed(format!("input capture session: {e}")),
    };
    let token = fs::read_to_string(token_file)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if token.is_none() {
        log("asking for permission to capture input; the dialog may open behind other windows");
    }

    let session = match portal.create_session2(Default::default()).await {
        Ok(session) => session,
        Err(ashpd::Error::RequiresVersion(..)) => {
            // Version 1 portals can't remember the permission.
            let options = CreateSessionOptions::default().set_capabilities(capabilities);
            let (session, _) = portal.create_session(None, options).await.map_err(failed)?;
            return Ok(session);
        }
        Err(e) => return Err(failed(e)),
    };
    let options = StartOptions::default()
        .set_capabilities(capabilities)
        .set_restore_token(token)
        .set_persist_mode(PersistMode::ExplicitlyRevoked);
    let response = portal
        .start(&session, None, options)
        .await
        .and_then(|r| r.response())
        .map_err(failed)?;
    match response.restore_token() {
        Some(token) => save_token(token_file, token),
        None => {
            let _ = fs::remove_file(token_file); // fine if there was none
        }
    }
    Ok(session)
}

fn save_token(path: &Path, token: &str) {
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(token.as_bytes()));
    if let Err(e) = written {
        log(format!(
            "could not save the input capture permission token: {e}"
        ));
    }
}
