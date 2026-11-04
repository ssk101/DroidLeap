//! Decides when input belongs to the tablet and moves it there, on any OS.
//!
//! The per-OS [`Capture`] reports edge touches and held input; the engine
//! applies the push threshold, forwards input to the tablet over USB, and
//! hands it back on Pause or when the tablet app reports its pointer at the
//! return edge. It recovers on its own: a lost tablet disarms the edge until
//! it's back, and a failed capture session is retried.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use mdns_sd::ServiceDaemon;
use tokio::sync::{mpsc, watch};

use crate::{
    capture::{Capture, CaptureError, Input},
    config::{Config, Paths},
    geometry::{Rect, Side, return_point},
    hid::{KEYBOARD_ID, Keyboard, MOUSE_ID, Mouse},
    keymap,
    link::{self, HoverState, Link, LinkEvent, TabletMessage},
    log,
    tablet::{Change, UsbTablet, Wake},
};

/// evdev code of the key that hands input back. Everything else, Esc included, goes to the tablet.
const KEY_PAUSE: u32 = 119;
/// Moving this far back from the edge while pushing hands the pointer back.
const AWAY_PX: f64 = 3.0;
const SESSION_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Requests from the user interface.
#[derive(Debug)]
pub enum Command {
    SetEnabled(bool),
    SetSide(Side),
    ReloadConfig,
    Quit,
}

/// What the user interface shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub side: Side,
    pub tablet: Option<String>,
    pub link: Option<String>,
    pub on_tablet: bool,
    pub problem: Option<String>,
}

impl Status {
    pub fn new(config: &Config) -> Status {
        Status {
            enabled: config.enabled,
            side: config.side,
            tablet: None,
            link: None,
            on_tablet: false,
            problem: None,
        }
    }

    /// Human-readable status, one line per fact.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            match &self.tablet {
                Some(name) => format!("Device: {name}"),
                None => "Device: not connected over USB".into(),
            },
            match &self.link {
                Some(name) => format!("Android app: connected ({name})"),
                None => "Android app: not found on the network".into(),
            },
        ];
        if self.on_tablet {
            lines.push("Input is on the device (Pause returns)".into());
        }
        if let Some(problem) = &self.problem {
            lines.push(problem.clone());
        }
        lines
    }
}

enum End {
    Quit,
    Disabled,
}

/// Sliding-window sum of pushes, like GNOME's PressureBarrier.
struct Pressure {
    window: Duration,
    samples: VecDeque<(Instant, f64)>,
    sum: f64,
}

impl Pressure {
    fn new(window: Duration) -> Self {
        Pressure {
            window,
            samples: VecDeque::new(),
            sum: 0.0,
        }
    }

    fn add(&mut self, now: Instant, amount: f64) -> f64 {
        while let Some(&(t, d)) = self.samples.front() {
            if now.duration_since(t) <= self.window {
                break;
            }
            self.sum -= d;
            self.samples.pop_front();
        }
        self.samples.push_back((now, amount));
        self.sum += amount;
        self.sum
    }

    fn last_push(&self) -> Option<Instant> {
        self.samples.back().map(|&(t, _)| t)
    }
}

enum State {
    Idle,
    /// The pointer touched the edge; input is held while we decide whether it's a push.
    Pressing {
        origin: (f64, f64),
        monitor: Rect,
        touched: Instant,
        pressure: Pressure,
        along: f64,
        away: f64,
    },
    /// Input goes to the tablet.
    Captured {
        origin: (f64, f64),
        /// Monitor the pointer left from, and comes back to.
        monitor: Rect,
        /// Push through the tablet's return edge, measured like the takeover push.
        back: Pressure,
    },
}

/// Per-session state.
struct Session {
    state: State,
    link: Link,
    /// Where the tablet's pointer sits on its return edge (0 to 1), if it's there.
    tablet_at_edge: Option<f64>,
    armed: bool,
}

pub struct Engine {
    config: Config,
    paths: Paths,
    commands: mpsc::UnboundedReceiver<Command>,
    status: watch::Sender<Status>,
    usb: UsbTablet,
    mdns: Option<ServiceDaemon>,
    mouse: Mouse,
    keyboard: Keyboard,
}

impl Engine {
    pub fn new(
        config: Config,
        paths: Paths,
        commands: mpsc::UnboundedReceiver<Command>,
        status: watch::Sender<Status>,
    ) -> Engine {
        let mdns = ServiceDaemon::new()
            .inspect_err(|e| {
                log(format!(
                    "mDNS unavailable, the Android device can only hand back with Pause: {e}"
                ))
            })
            .ok();
        Engine {
            usb: UsbTablet::new(config.serial().map(str::to_string)),
            config,
            paths,
            commands,
            status,
            mdns,
            mouse: Mouse::default(),
            keyboard: Keyboard::default(),
        }
    }

    pub async fn run(mut self) {
        loop {
            if !self.config.enabled {
                log("capture is off");
                if !self.idle(None).await {
                    break;
                }
                continue;
            }
            match self.session().await {
                Ok(End::Quit) => break,
                Ok(End::Disabled) => {}
                Err(CaptureError::Denied) => {
                    log("input capture was not allowed");
                    self.config.enabled = false;
                    self.set_status(|s| {
                        s.enabled = false;
                        s.problem =
                            Some("Input capture was not allowed. Enable it to ask again.".into());
                    });
                }
                Err(CaptureError::Failed(e)) => {
                    log(format!(
                        "input capture stopped: {e}; retrying in {} s",
                        SESSION_RETRY_DELAY.as_secs()
                    ));
                    self.set_status(|s| s.problem = Some(e));
                    if !self.idle(Some(Instant::now() + SESSION_RETRY_DELAY)).await {
                        break;
                    }
                }
            }
            self.set_status(|s| s.on_tablet = false);
        }
        self.usb.close().await;
        if let Some(mdns) = self.mdns.take()
            && let Err(e) = mdns.shutdown()
        {
            log(format!("could not stop mDNS: {e}"));
        }
        log("stopped");
    }

    /// Waits without a capture session, still following commands and the tablet.
    /// Returns false on quit, true once `deadline` passes or capture is switched on.
    async fn idle(&mut self, deadline: Option<Instant>) -> bool {
        loop {
            let sleep = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv() => match command {
                    None | Some(Command::Quit) => return false,
                    Some(Command::SetEnabled(on)) => {
                        self.set_enabled(on);
                        if on {
                            return true;
                        }
                    }
                    Some(Command::SetSide(side)) => {
                        self.set_side(side);
                    }
                    Some(Command::ReloadConfig) => {
                        if self.reload_config().await.is_some() && self.config.enabled {
                            return true;
                        }
                    }
                },
                wake = self.usb.wait() => {
                    self.on_usb(wake).await;
                }
                () = sleep => return true,
            }
        }
    }

    async fn session(&mut self) -> Result<End, CaptureError> {
        let mut capture = Capture::open(&self.paths);
        let result = self.run_session(&mut capture).await;
        capture.close().await;
        result
    }

    async fn run_session(&mut self, capture: &mut Capture) -> Result<End, CaptureError> {
        let mut session = Session {
            state: State::Idle,
            link: self.spawn_link(),
            tablet_at_edge: None,
            armed: false,
        };
        self.set_status(|s| {
            s.enabled = true;
            s.problem = None;
        });
        capture.set_edge(self.config.side);
        self.sync_armed(capture, &mut session).await;

        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    if let Some(end) = self.on_command(capture, &mut session, command).await {
                        return Ok(end);
                    }
                }
                wake = self.usb.wait() => {
                    if self.on_usb(wake).await {
                        self.sync_armed(capture, &mut session).await;
                    }
                }
                input = capture.next() => {
                    if let Some(end) = self.on_input(capture, &mut session, input?).await {
                        return Ok(end);
                    }
                }
                Some(event) = session.link.events.recv() => self.on_link(&mut session, event),
                _ = tick.tick() => self.on_tick(capture, &mut session).await,
            }
        }
    }

    async fn on_command(
        &mut self,
        capture: &Capture,
        session: &mut Session,
        command: Option<Command>,
    ) -> Option<End> {
        match command {
            None | Some(Command::Quit) => {
                self.hand_back_now(capture, session).await;
                Some(End::Quit)
            }
            Some(Command::SetEnabled(true)) => None,
            Some(Command::SetEnabled(false)) => {
                self.hand_back_now(capture, session).await;
                self.set_enabled(false);
                Some(End::Disabled)
            }
            Some(Command::SetSide(side)) => {
                if self.set_side(side) {
                    self.apply_side(capture, session).await;
                }
                None
            }
            Some(Command::ReloadConfig) => {
                let old = self.reload_config().await?;
                if !self.config.enabled {
                    self.hand_back_now(capture, session).await;
                    return Some(End::Disabled);
                }
                if old.side != self.config.side {
                    self.apply_side(capture, session).await;
                }
                self.sync_armed(capture, session).await;
                None
            }
        }
    }

    async fn on_input(
        &mut self,
        capture: &Capture,
        session: &mut Session,
        input: Input,
    ) -> Option<End> {
        let captured = matches!(session.state, State::Captured { .. });
        match input {
            Input::Watching(count) => {
                let side = self.config.side;
                log(format!("watching the {side:?} edge of {count} monitor(s)"));
                let problem =
                    (count == 0).then(|| format!("No monitor has an outer {side:?} edge"));
                self.set_status(|s| s.problem = problem);
            }
            Input::EdgeTouched { position, monitor } => {
                self.on_edge_touched(capture, session, position, monitor)
                    .await
            }
            Input::Motion { dx, dy } => self.on_motion(capture, session, dx, dy).await,
            Input::Button { code, pressed } if captured => {
                let known = self.mouse.set_button(code, pressed);
                if !known {
                    log(format!(
                        "mouse button {code:#x} has no HID equivalent, ignored"
                    ));
                }
            }
            Input::ScrollPixels { dx, dy } if captured => self.mouse.add_scroll_px(dx, dy),
            Input::ScrollSteps { dx, dy } if captured => self.mouse.add_scroll_discrete(dx, dy),
            Input::Frame if captured => {
                for report in self.mouse.frame() {
                    self.send(capture, session, MOUSE_ID, &report).await;
                }
            }
            Input::Key { code, pressed } => self.on_key(capture, session, code, pressed).await,
            Input::Released => {
                if captured {
                    self.lift_all().await;
                    log("the desktop took input back");
                }
                session.state = State::Idle;
                self.set_status(|s| s.on_tablet = false);
            }
            Input::MonitorsChanged => {
                log("monitors changed");
                self.hand_back_now(capture, session).await;
            }
            Input::Quit => {
                self.hand_back_now(capture, session).await;
                return Some(End::Quit);
            }
            Input::Button { .. }
            | Input::ScrollPixels { .. }
            | Input::ScrollSteps { .. }
            | Input::Frame => {}
        }
        None
    }

    async fn on_edge_touched(
        &mut self,
        capture: &Capture,
        session: &mut Session,
        origin: (f64, f64),
        monitor: Rect,
    ) {
        if self.usb.get().is_none() {
            // Armed just as the tablet went away: don't hold input with nowhere to send it.
            capture.release(self.config.side.moved(origin, 1.0, 0.0));
            return;
        }
        session.state = if self.config.threshold_px == 0.0 {
            self.took_over();
            State::Captured {
                origin,
                monitor,
                back: Pressure::new(self.config.timeout()),
            }
        } else {
            State::Pressing {
                origin,
                monitor,
                touched: Instant::now(),
                pressure: Pressure::new(self.config.timeout()),
                along: 0.0,
                away: 0.0,
            }
        };
    }

    fn took_over(&self) {
        log("input on the Android device");
        self.set_status(|s| s.on_tablet = true);
    }

    async fn on_motion(&mut self, capture: &Capture, session: &mut Session, dx: f64, dy: f64) {
        let side = self.config.side;
        let threshold = self.config.threshold_px;
        let now = Instant::now();
        match &mut session.state {
            State::Idle => {}
            State::Pressing {
                origin,
                monitor,
                pressure,
                along,
                away,
                ..
            } => {
                let (push, slide) = side.split(dx, dy);
                *along += slide;
                if push > 0.0 && pressure.add(now, push) >= threshold {
                    session.state = State::Captured {
                        origin: *origin,
                        monitor: *monitor,
                        back: Pressure::new(self.config.timeout()),
                    };
                    self.took_over();
                } else if push < 0.0 {
                    *away -= push;
                    if *away >= AWAY_PX {
                        let to = side.moved(*origin, *away, *along);
                        self.release(capture, session, to).await;
                    }
                }
            }
            State::Captured { monitor, back, .. } => {
                // Heading back to the desktop is the opposite of the takeover push.
                let push_back = -side.split(dx, dy).0;
                let returning = session
                    .tablet_at_edge
                    .filter(|_| push_back > 0.0 && back.add(now, push_back) >= threshold);
                match returning {
                    Some(along) => {
                        let to = return_point(side, monitor, along);
                        self.release(capture, session, to).await;
                        log("input back on the desktop (device edge)");
                    }
                    None => self.mouse.add_motion(dx, dy),
                }
            }
        }
    }

    async fn on_key(&mut self, capture: &Capture, session: &mut Session, code: u32, pressed: bool) {
        let side = self.config.side;
        match session.state {
            State::Pressing { origin, along, .. } if pressed => {
                // Typing while resting on the edge: give input back (that key is lost).
                self.release(capture, session, side.moved(origin, 1.0, along))
                    .await;
            }
            State::Captured { origin, .. } if code == KEY_PAUSE && pressed => {
                self.release(capture, session, side.moved(origin, 1.0, 0.0))
                    .await;
                log("input back on the desktop (Pause)");
            }
            State::Captured { .. } => {
                let Some(usage) = keymap::hid_usage(code) else {
                    log(format!("key {code} has no HID equivalent, ignored"));
                    return;
                };
                if let Some(report) = self.keyboard.key(usage, pressed) {
                    self.send(capture, session, KEYBOARD_ID, &report).await;
                }
            }
            _ => {}
        }
    }

    fn on_link(&mut self, session: &mut Session, event: LinkEvent) {
        let tablet_edge = self.config.side.opposite();
        match event {
            LinkEvent::Connected(name) => {
                log(format!("Android app connected: {name}"));
                self.set_status(|s| s.link = Some(name));
            }
            LinkEvent::Disconnected(reason) => {
                if self.status.borrow().link.is_some() {
                    log(format!("Android app disconnected: {reason}"));
                }
                session.tablet_at_edge = None;
                self.set_status(|s| s.link = None);
            }
            LinkEvent::Message(TabletMessage::Hello {
                version,
                app,
                width,
                height,
                rotation,
            }) => {
                log(format!(
                    "Android app {app}, screen {width}x{height} rotated {rotation}°"
                ));
                if version != link::PROTOCOL_VERSION {
                    log(format!(
                        "Android app speaks protocol {version}, this app {}",
                        link::PROTOCOL_VERSION
                    ));
                }
            }
            LinkEvent::Message(TabletMessage::Display { .. }) => {}
            LinkEvent::Message(TabletMessage::Edge { edge, state, along }) => {
                if edge != tablet_edge {
                    return;
                }
                session.tablet_at_edge = match state {
                    HoverState::Enter | HoverState::Move => Some(along.clamp(0.0, 1.0)),
                    HoverState::Exit => None,
                };
            }
        }
    }

    async fn on_tick(&mut self, capture: &Capture, session: &mut Session) {
        if let State::Pressing {
            origin,
            touched,
            pressure,
            along,
            ..
        } = &session.state
        {
            let last_push = pressure.last_push().unwrap_or(*touched);
            if last_push.elapsed() > self.config.timeout() {
                let to = self.config.side.moved(*origin, 1.0, *along);
                self.release(capture, session, to).await;
            }
        }
    }

    /// Handles a USB event. Returns whether the tablet came or went.
    async fn on_usb(&mut self, wake: Wake) -> bool {
        match self.usb.handle(wake).await {
            Some(change) => {
                self.on_usb_change(change);
                true
            }
            None => false,
        }
    }

    fn on_usb_change(&mut self, change: Change) {
        match change {
            Change::Connected(name) => {
                log(format!("Android device connected: {name}"));
                self.set_status(|s| s.tablet = Some(name));
            }
            Change::Lost(reason) => {
                log(format!("Android device lost: {reason}"));
                self.mouse.release_all();
                self.keyboard.release_all();
                self.set_status(|s| s.tablet = None);
            }
        }
    }

    /// Sends a HID report. If the tablet stopped answering, forgets it and gives input back.
    async fn send(&mut self, capture: &Capture, session: &mut Session, id: u16, report: &[u8]) {
        let Some(tablet) = self.usb.get() else {
            return;
        };
        if let Err(e) = tablet.send(id, report).await {
            let change = self.usb.lost(e);
            self.on_usb_change(change);
            self.sync_armed(capture, session).await;
        }
    }

    /// Lifts every key and button on the tablet, so nothing stays held after handing back.
    async fn lift_all(&mut self) {
        let keyboard = self.keyboard.release_all();
        let mouse = self.mouse.release_all();
        if let Some(tablet) = self.usb.get() {
            let lifted = async {
                tablet.send(KEYBOARD_ID, &keyboard).await?;
                tablet.send(MOUSE_ID, &mouse).await
            };
            if let Err(e) = lifted.await {
                log(format!("could not release keys on the Android device: {e}"));
            }
        }
    }

    /// Gives input back to the desktop with the pointer at `to`.
    async fn release(&mut self, capture: &Capture, session: &mut Session, to: (f64, f64)) {
        if matches!(session.state, State::Idle) {
            return;
        }
        self.lift_all().await;
        session.state = State::Idle;
        self.set_status(|s| s.on_tablet = false);
        capture.release(to);
    }

    /// Gives input back where it left the desktop, if it's away.
    async fn hand_back_now(&mut self, capture: &Capture, session: &mut Session) {
        let origin = match session.state {
            State::Idle => return,
            State::Pressing { origin, .. } | State::Captured { origin, .. } => origin,
        };
        self.release(capture, session, self.config.side.moved(origin, 1.0, 0.0))
            .await;
    }

    /// The edge is armed only with a tablet to send to.
    async fn sync_armed(&mut self, capture: &Capture, session: &mut Session) {
        let armed = self.usb.get().is_some();
        if armed == session.armed {
            return;
        }
        if !armed {
            self.hand_back_now(capture, session).await;
        }
        capture.set_active(armed);
        session.armed = armed;
        if armed {
            let how = if self.config.threshold_px == 0.0 {
                "touch"
            } else {
                "push through"
            };
            log(format!(
                "ready: {how} the {:?} edge to move to the Android device",
                self.config.side
            ));
        } else {
            log("waiting for the Android device");
        }
    }

    async fn apply_side(&mut self, capture: &Capture, session: &mut Session) {
        self.hand_back_now(capture, session).await;
        session.tablet_at_edge = None;
        session.link = self.spawn_link();
        capture.set_edge(self.config.side);
    }

    fn spawn_link(&self) -> Link {
        match &self.mdns {
            Some(mdns) => link::spawn(mdns.clone(), self.config.side.opposite()),
            None => link::disabled(),
        }
    }

    fn set_enabled(&mut self, on: bool) {
        if self.config.enabled == on {
            return;
        }
        self.config.enabled = on;
        self.save_config();
        self.set_status(|s| {
            s.enabled = on;
            s.problem = None;
        });
    }

    /// Returns whether the side changed.
    fn set_side(&mut self, side: Side) -> bool {
        if self.config.side == side {
            return false;
        }
        self.config.side = side;
        self.save_config();
        self.set_status(|s| s.side = side);
        true
    }

    /// Re-reads the settings file. Returns the previous settings if it changed anything.
    async fn reload_config(&mut self) -> Option<Config> {
        let config = match Config::load(&self.paths.config_file) {
            Ok(config) => config,
            Err(e) => {
                log(format!("settings not reloaded: {e}"));
                self.set_status(|s| s.problem = Some(e));
                return None;
            }
        };
        if config == self.config {
            return None;
        }
        log("settings reloaded");
        if let Some(change) = self
            .usb
            .set_serial(config.serial().map(str::to_string))
            .await
        {
            self.on_usb_change(change);
        }
        let old = std::mem::replace(&mut self.config, config);
        let (enabled, side) = (self.config.enabled, self.config.side);
        self.set_status(|s| {
            s.enabled = enabled;
            s.side = side;
            s.problem = None;
        });
        Some(old)
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save(&self.paths.config_file) {
            log(format!("settings not saved: {e}"));
            self.set_status(|s| s.problem = Some(e));
        }
    }

    fn set_status(&self, change: impl FnOnce(&mut Status)) {
        self.status.send_if_modified(|status| {
            let before = status.clone();
            change(status);
            *status != before
        });
    }
}
