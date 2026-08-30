use std::sync::Arc;
use std::thread;
use std::time::Duration;

use grafton_visca::camera::profiles::GenericVisca;
#[cfg(feature = "simulator")]
use grafton_visca::camera::CameraBuilder;
use grafton_visca::command::{PanTiltDirection, PresetNumber};
use grafton_visca::runtime::TokioRuntime;
#[cfg(feature = "simulator")]
use grafton_visca::testing::ViscaCameraSimulator;
use grafton_visca::types::{PanSpeed, SpeedLevel, TiltSpeed};
use grafton_visca::{FocusControl, PanTiltControl, PresetsControl, ZoomControl};
use tracing::{error, info, warn};

use crate::config::ConnectionConfig;

/// Interval between application-level heartbeat inquiries. Keep this comfortably
/// below a camera's idle-session timeout so the firmware's activity timer is reset
/// and an idle close surfaces as an early, explicit `ErrorKind::IoClosed` signal.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum CameraAction {
    PanTiltMove { direction: PanTiltDirection },
    PanTiltStop,
    PanTiltHome,
    ZoomIn,
    ZoomOut,
    ZoomStop,
    FocusNear,
    FocusFar,
    FocusStop,
    PresetRecall(u8),
}

#[derive(Debug, Clone)]
pub enum CameraConnection {
    #[cfg(feature = "simulator")]
    Simulator,
    Tcp { address: String },
    Serial { port: String, baud_rate: u32 },
}

impl From<&ConnectionConfig> for CameraConnection {
    fn from(config: &ConnectionConfig) -> Self {
        match config {
            ConnectionConfig::Ip { address } => CameraConnection::Tcp {
                address: address.clone(),
            },
            ConnectionConfig::Serial { port, baud_rate } => CameraConnection::Serial {
                port: port.clone(),
                baud_rate: *baud_rate,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraStatus {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

pub enum CameraCommand {
    Action(CameraAction),
    Reconnect(CameraConnection),
}

enum CameraHandle {
    #[cfg(feature = "simulator")]
    Simulator(
        grafton_visca::camera::Camera<
            grafton_visca::mode::Async,
            GenericVisca,
            ViscaCameraSimulator,
            TokioRuntime,
        >,
    ),
    Session(
        grafton_visca::camera::CameraSession<
            grafton_visca::mode::Async,
            GenericVisca,
            grafton_visca::runtime::TransportHandle<TokioRuntime>,
            TokioRuntime,
        >,
    ),
}

pub struct CameraService {
    tx: tokio::sync::mpsc::Sender<CameraCommand>,
}

/// Tracks the current live session.
struct Session {
    handle: CameraHandle,
}

impl CameraService {
    pub fn start(status_cb: Arc<dyn Fn(CameraStatus) + Send + Sync>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<CameraCommand>(64);

        thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let runtime = TokioRuntime::from_current().unwrap();
                let mut status = StatusReporter::new(status_cb);
                let mut session: Option<Session> = None;
                let mut connection: Option<CameraConnection> = None;

                let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        cmd = rx.recv() => {
                            let Some(cmd) = cmd else { break };
                            match cmd {
                                CameraCommand::Action(action) => {
                                    let requires_rebuild = if let Some(sess) = &mut session {
                                        handle_action(&mut sess.handle, action, &mut status).await
                                    } else {
                                        continue;
                                    };
                                    if requires_rebuild {
                                        // Session requires a fresh rebuild.
                                        info!("Session requires rebuild after a command failure");
                                        status.set(CameraStatus::Reconnecting);
                                        session = connect_with_retry(&runtime, &mut status, connection.as_ref().unwrap(), &mut heartbeat).await;
                                    }
                                }
                                CameraCommand::Reconnect(conn) => {
                                    info!("Reconnecting camera: {:?}", conn);
                                    connection = Some(conn);
                                    status.set(CameraStatus::Connecting);
                                    session = connect_with_retry(&runtime, &mut status, connection.as_ref().unwrap(), &mut heartbeat).await;
                                }
                            }
                        }
                        _ = heartbeat.tick(), if session.is_some() => {
                            let rebuild = if let Some(sess) = &mut session {
                                heartbeat_tick(&mut sess.handle).await
                            } else {
                                false
                            };
                            if rebuild {
                                let conn = connection.as_ref().unwrap().clone();
                                info!("Session closed during idle; rebuilding");
                                status.set(CameraStatus::Reconnecting);
                                session = connect_with_retry(&runtime, &mut status, &conn, &mut heartbeat).await;
                            }
                        }
                    }
                }
            });
        });

        Self { tx }
    }

    pub fn send_action(&self, action: CameraAction) {
        let _ = self.tx.blocking_send(CameraCommand::Action(action));
    }

    pub fn reconnect(&self, connection: CameraConnection) {
        let _ = self.tx.blocking_send(CameraCommand::Reconnect(connection));
    }
}

/// Connect to the retained transport factory, retrying indefinitely with backoff
/// until a session is established. Each round emits status updates; on success the
/// fresh session's state cache (which starts `Unknown`) is warmed with a re-query.
async fn connect_with_retry(
    runtime: &TokioRuntime,
    status: &mut StatusReporter,
    connection: &CameraConnection,
    heartbeat: &mut tokio::time::Interval,
) -> Option<Session> {
    loop {
        status.set(CameraStatus::Connecting);
        match connect_camera(runtime, connection).await {
            Ok(handle) => {
                info!("Camera connected successfully");
                let mut session = Session { handle };
                warm_state(&mut session.handle).await;
                status.set(CameraStatus::Connected);
                // Reset the heartbeat timer so the re-query isn't immediately
                // followed by another tick.
                heartbeat.reset();
                return Some(session);
            }
            Err(e) => {
                warn!("Failed to connect camera: {}", e);
                status.set(CameraStatus::Disconnected);
                // Wait before retrying; poll a little more aggressively on
                // connection failure since the device may come up shortly.
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

/// Runs a single heartbeat inquiry on the live session. Returns `true` if the
/// session must be rebuilt (the transport closed), otherwise `false`.
async fn heartbeat_tick(handle: &mut CameraHandle) -> bool {
    let result = match handle {
        #[cfg(feature = "simulator")]
        CameraHandle::Simulator(cam) => cam.power().state().await,
        CameraHandle::Session(session) => session.camera().power().state().await,
    };
    match result {
        Ok(_) => false,
        Err(err) => {
            if is_rebuild_error(&err) {
                error!("Heartbeat detected closed session: {}", err);
                true
            } else {
                warn!("Heartbeat inquiry failed (non-fatal): {}", err);
                false
            }
        }
    }
}

/// Warm the fresh session's state cache (which starts `Unknown`) so the control
/// layer reflects reality before user interaction resumes.
async fn warm_state(handle: &mut CameraHandle) {
    let result = match handle {
        #[cfg(feature = "simulator")]
        CameraHandle::Simulator(cam) => cam.power().state().await,
        CameraHandle::Session(session) => session.camera().power().state().await,
    };
    if let Err(err) = result {
        warn!("State re-query failed after reconnect: {}", err);
    }
}

/// Tracks the connection state reported to the UI and deduplicates updates so
/// only actual transitions trigger a callback.
struct StatusReporter {
    current: CameraStatus,
    cb: Arc<dyn Fn(CameraStatus) + Send + Sync>,
}

impl StatusReporter {
    fn new(cb: Arc<dyn Fn(CameraStatus) + Send + Sync>) -> Self {
        Self {
            current: CameraStatus::Disconnected,
            cb,
        }
    }

    fn set(&mut self, new: CameraStatus) {
        if self.current != new {
            info!("Camera status: {:?}", new);
            self.current = new;
            (self.cb)(new);
        }
    }
}

/// Returns true if this error means the current session can no longer be used and
/// a fresh session must be built (the liveness-recognition rule: `IoClosed`
/// covers `ConnectionClosed` and `StreamPoisoned`).
fn is_rebuild_error(err: &grafton_visca::Error) -> bool {
    err.kind() == grafton_visca::ErrorKind::IoClosed
}

async fn connect_camera(
    runtime: &TokioRuntime,
    connection: &CameraConnection,
) -> Result<CameraHandle, String> {
    match connection {
        #[cfg(feature = "simulator")]
        CameraConnection::Simulator => {
            let simulator = ViscaCameraSimulator::new();
            let cam = CameraBuilder::with_executor(runtime.clone())
                .open_async::<GenericVisca, _>(simulator)
                .await
                .map_err(|e| format!("Simulator connection failed: {}", e))?;
            Ok(CameraHandle::Simulator(cam))
        }
        CameraConnection::Tcp { address } => {
            let config = grafton_visca::camera::CameraConfig::<GenericVisca>::tcp(address);
            let session = config
                .open_async(runtime.clone())
                .await
                .map_err(|e| format!("TCP connection to {} failed: {}", address, e))?;
            Ok(CameraHandle::Session(session))
        }
        CameraConnection::Serial { port, baud_rate } => {
            let config =
                grafton_visca::camera::CameraConfig::<GenericVisca>::serial(port, *baud_rate);
            let session = config
                .open_serial_async(runtime.clone())
                .await
                .map_err(|e| format!("Serial connection to {} failed: {}", port, e))?;
            Ok(CameraHandle::Session(session))
        }
    }
}

/// Runs a camera action; returns `true` if the session requires a fresh rebuild.
async fn handle_action(
    camera: &mut CameraHandle,
    action: CameraAction,
    status: &mut StatusReporter,
) -> bool {
    let result = match camera {
        #[cfg(feature = "simulator")]
        CameraHandle::Simulator(cam) => handle_simulator_action(cam, action).await,
        CameraHandle::Session(session) => handle_session_action(session, action).await,
    };
    match result {
        Ok(()) => false,
        Err(err) if is_rebuild_error(&err) => {
            error!("Session failure, requires rebuild: {}", err);
            status.set(CameraStatus::Reconnecting);
            true
        }
        Err(err) => {
            warn!("Camera command failed (non-fatal): {}", err);
            false
        }
    }
}

type ActionResult = Result<(), grafton_visca::Error>;

#[cfg(feature = "simulator")]
async fn handle_simulator_action(
    camera: &mut grafton_visca::camera::Camera<
        grafton_visca::mode::Async,
        GenericVisca,
        ViscaCameraSimulator,
        TokioRuntime,
    >,
    action: CameraAction,
) -> ActionResult {
    match action {
        CameraAction::PanTiltMove { direction } => {
            info!("Camera: pan_tilt_move({:?})", direction);
            camera
                .pan_tilt_move(
                    direction,
                    PanSpeed::from(SpeedLevel::Medium),
                    TiltSpeed::from(SpeedLevel::Medium),
                )
                .await
        }
        CameraAction::PanTiltStop => {
            info!("Camera: pan_tilt_stop()");
            camera.pan_tilt_stop().await
        }
        CameraAction::PanTiltHome => {
            info!("Camera: pan_tilt_home()");
            camera.pan_tilt_home().await
        }
        CameraAction::ZoomIn => {
            info!("Camera: zoom_tele()");
            camera.zoom_tele(None).await
        }
        CameraAction::ZoomOut => {
            info!("Camera: zoom_wide()");
            camera.zoom_wide(None).await
        }
        CameraAction::ZoomStop => {
            info!("Camera: zoom_stop()");
            camera.zoom_stop().await
        }
        CameraAction::FocusNear => {
            info!("Camera: focus_near()");
            camera.focus_near(SpeedLevel::Medium).await
        }
        CameraAction::FocusFar => {
            info!("Camera: focus_far()");
            camera.focus_far(SpeedLevel::Medium).await
        }
        CameraAction::FocusStop => {
            info!("Camera: focus_stop()");
            camera.focus_stop().await
        }
        CameraAction::PresetRecall(n) => {
            info!("Camera: preset_recall({})", n);
            if let Ok(preset) = PresetNumber::new(n) {
                camera.preset_recall(preset).await?;
            }
            Ok(())
        }
    }
}

async fn handle_session_action(
    camera: &mut grafton_visca::camera::CameraSession<
        grafton_visca::mode::Async,
        GenericVisca,
        grafton_visca::runtime::TransportHandle<TokioRuntime>,
        TokioRuntime,
    >,
    action: CameraAction,
) -> ActionResult {
    match action {
        CameraAction::PanTiltMove { direction } => {
            info!("Camera: pan_tilt_move({:?})", direction);
            camera
                .pan_tilt_move(
                    direction,
                    PanSpeed::from(SpeedLevel::Medium),
                    TiltSpeed::from(SpeedLevel::Medium),
                )
                .await
        }
        CameraAction::PanTiltStop => {
            info!("Camera: pan_tilt_stop()");
            camera.pan_tilt_stop().await
        }
        CameraAction::PanTiltHome => {
            info!("Camera: pan_tilt_home()");
            camera.pan_tilt_home().await
        }
        CameraAction::ZoomIn => {
            info!("Camera: zoom_tele()");
            camera.zoom_tele(None).await
        }
        CameraAction::ZoomOut => {
            info!("Camera: zoom_wide()");
            camera.zoom_wide(None).await
        }
        CameraAction::ZoomStop => {
            info!("Camera: zoom_stop()");
            camera.zoom_stop().await
        }
        CameraAction::FocusNear => {
            info!("Camera: focus_near()");
            camera.focus_near(SpeedLevel::Medium).await
        }
        CameraAction::FocusFar => {
            info!("Camera: focus_far()");
            camera.focus_far(SpeedLevel::Medium).await
        }
        CameraAction::FocusStop => {
            info!("Camera: focus_stop()");
            camera.focus_stop().await
        }
        CameraAction::PresetRecall(n) => {
            info!("Camera: preset_recall({})", n);
            if let Ok(preset) = PresetNumber::new(n) {
                camera.preset_recall(preset).await?;
            }
            Ok(())
        }
    }
}
