use std::sync::Arc;
use std::thread;
use std::time::Duration;

use grafton_visca::{
    Error,
    r#async::{Camera, CameraSession, Operation},
    command::{pan_tilt::Home, presets::Recall},
    operation::{AppliedOnly, Targeted},
    profiles::GenericVisca,
    raw::AppliedOnlyRequest,
    request::OperationCommand,
    runtime::TokioRuntime,
    types::{AffectedAxes, PanTiltDirection, PresetNumber, SpeedLevel},
};
#[cfg(feature = "simulator")]
use grafton_visca::{
    config::{CameraConfig, TransportOptions},
    profiles::ProfileId,
    testing::ViscaCameraSimulator,
};
use tracing::{error, info, warn};

use crate::config::ConnectionConfig;

/// Interval between application-level heartbeat inquiries. Keep this comfortably
/// below a camera's idle-session timeout so the firmware's activity timer is reset
/// and an idle close surfaces as an early, explicit session failure.
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
    Tcp {
        address: String,
    },
    Serial {
        port: String,
        baud_rate: u32,
    },
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

pub struct CameraService {
    tx: tokio::sync::mpsc::Sender<CameraCommand>,
}

impl CameraService {
    pub fn start(status_cb: Arc<dyn Fn(CameraStatus) + Send + Sync>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<CameraCommand>(64);

        thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let runtime = TokioRuntime::from_current().unwrap();
                let mut status = StatusReporter::new(status_cb);
                let mut session: Option<CameraSession<GenericVisca>> = None;
                let mut connection: Option<CameraConnection> = None;

                let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        cmd = rx.recv() => {
                            let Some(cmd) = cmd else { break };
                            match cmd {
                                CameraCommand::Action(action) => {
                                    let requires_rebuild = if let Some(sess) = &session {
                                        handle_action(sess.camera(), action, &mut status).await
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
                            let rebuild = if let Some(sess) = &session {
                                heartbeat_tick(sess.camera()).await
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
) -> Option<CameraSession<GenericVisca>> {
    loop {
        status.set(CameraStatus::Connecting);
        match connect_camera(runtime, connection).await {
            Ok(session) => {
                info!("Camera connected successfully");
                warm_state(session.camera()).await;
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
async fn heartbeat_tick(camera: &Camera<GenericVisca>) -> bool {
    match camera.power().status().await {
        Ok(_) => false,
        Err(err) if err.requires_new_session() => {
            error!("Heartbeat detected closed session: {}", err);
            true
        }
        Err(err) => {
            warn!("Heartbeat inquiry failed (non-fatal): {}", err);
            false
        }
    }
}

/// Refreshes the camera's reported power state after reconnecting.
async fn warm_state(camera: &Camera<GenericVisca>) {
    if let Err(err) = camera.power().status().await {
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

async fn connect_camera(
    runtime: &TokioRuntime,
    connection: &CameraConnection,
) -> Result<CameraSession<GenericVisca>, String> {
    match connection {
        #[cfg(feature = "simulator")]
        CameraConnection::Simulator => {
            let config = CameraConfig::new(ProfileId::GenericVisca, TransportOptions::Custom)
                .into_typed::<GenericVisca>()
                .map_err(|e| format!("Simulator configuration failed: {}", e))?;
            CameraSession::open(ViscaCameraSimulator::default(), &config, runtime.clone())
                .await
                .map_err(|e| format!("Simulator connection failed: {}", e))
        }
        CameraConnection::Tcp { address } => CameraSession::connect_tcp(address, runtime.clone())
            .await
            .map_err(|e| format!("TCP connection to {} failed: {}", address, e)),
        CameraConnection::Serial { port, baud_rate } => {
            CameraSession::connect_serial(port, *baud_rate, runtime.clone())
                .await
                .map_err(|e| format!("Serial connection to {} failed: {}", port, e))
        }
    }
}

/// Runs a camera action; returns `true` if the session requires a fresh rebuild.
async fn handle_action(
    camera: &Camera<GenericVisca>,
    action: CameraAction,
    status: &mut StatusReporter,
) -> bool {
    match apply_action(camera, action).await {
        Ok(()) => false,
        Err(err) if err.requires_new_session() => {
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

async fn apply_action(camera: &Camera<GenericVisca>, action: CameraAction) -> Result<(), Error> {
    let mut operation = match action {
        CameraAction::PanTiltMove { direction } => {
            info!("Camera: pan_tilt_move({:?})", direction);
            camera.pan_tilt().drive(direction, SpeedLevel::Medium).await
        }
        CameraAction::PanTiltStop => {
            info!("Camera: pan_tilt_stop()");
            camera.pan_tilt().stop().await
        }
        CameraAction::PanTiltHome => {
            info!("Camera: pan_tilt_home()");
            submit_generic_move(camera, &Home, AffectedAxes::PAN_TILT).await
        }
        CameraAction::ZoomIn => {
            info!("Camera: zoom_tele()");
            camera.zoom().tele().await
        }
        CameraAction::ZoomOut => {
            info!("Camera: zoom_wide()");
            camera.zoom().wide().await
        }
        CameraAction::ZoomStop => {
            info!("Camera: zoom_stop()");
            camera.zoom().stop().await
        }
        CameraAction::FocusNear => {
            info!("Camera: focus_near()");
            camera.focus().near_variable(SpeedLevel::Medium).await
        }
        CameraAction::FocusFar => {
            info!("Camera: focus_far()");
            camera.focus().far_variable(SpeedLevel::Medium).await
        }
        CameraAction::FocusStop => {
            info!("Camera: focus_stop()");
            camera.focus().stop().await
        }
        CameraAction::PresetRecall(n) => {
            info!("Camera: preset_recall({})", n);
            // Keep the preset range used by this app with the 1.x GenericVisca profile.
            const HIGHEST_PRESET: u8 = 6;
            if n > HIGHEST_PRESET {
                return Err(Error::parameter_out_of_range(
                    "preset_number",
                    i32::from(n),
                    0,
                    i32::from(HIGHEST_PRESET),
                ));
            }
            let recall = Recall::new(PresetNumber::new(n)?);
            let axes = AffectedAxes::PAN_TILT
                .union(AffectedAxes::ZOOM)
                .union(AffectedAxes::FOCUS);
            submit_generic_move(camera, &recall, axes).await
        }
    }?;
    operation.applied().await
}

// Home and recall used the protocol completion in 1.x. The 2.0 GenericVisca
// profile has no pan/tilt settlement evidence, so keep the protocol wait.
async fn submit_generic_move(
    camera: &Camera<GenericVisca>,
    command: &impl OperationCommand<Targeted>,
    axes: AffectedAxes,
) -> Result<Operation<AppliedOnly>, Error> {
    let mut frame = [0; 16];
    let len = command.write_into(camera.target().into(), &mut frame)?;
    let request = AppliedOnlyRequest::with_policy(&frame[..len], axes, command.policy())?;
    camera.submit(&request).await
}
