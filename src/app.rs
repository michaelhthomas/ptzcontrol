use gtk::prelude::*;
use relm4::adw::prelude::*;
use relm4::prelude::*;

use crate::camera::{CameraAction, CameraConnection, CameraService, CameraStatus};
use crate::camera_preferences::{CameraPreferences, CameraPreferencesOutput};
use crate::config::AppConfig;
use crate::d_pad::DPad;
use crate::header::{Header, HeaderOutput};
use crate::presets::Presets;
use crate::welcome::{Welcome, WelcomeMsg};
use crate::zoom_focus::ZoomFocus;

pub struct App {
    config: AppConfig,
    header: Controller<Header>,
    d_pad: Controller<DPad>,
    zoom_focus: Controller<ZoomFocus>,
    presets: Controller<Presets>,
    welcome: Controller<Welcome>,
    camera_service: CameraService,
    stack: Option<gtk::Stack>,
    settings_dialog: Option<Controller<CameraPreferences>>,
}

#[derive(Debug)]
pub enum AppMsg {
    CameraAction(CameraAction),
    HeaderOutput(HeaderOutput),
    WelcomeOutput(WelcomeMsg),
    OpenSettings { add_new: bool },
    ConfigChanged(AppConfig),
    ReconnectCamera,
    CameraStatus(CameraStatus),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = u8;
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("PTZ Camera Controller"),
            set_default_size: (400, 700),

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                append = model.header.widget(),

                #[name = "content_stack"]
                append = &gtk::Stack {
                    set_vexpand: true,
                },
            }
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = AppConfig::load();

        let status_sender = sender.input_sender().clone();
        let status_cb: std::sync::Arc<dyn Fn(CameraStatus) + Send + Sync> =
            std::sync::Arc::new(move |status| {
                let _ = status_sender.emit(AppMsg::CameraStatus(status));
            });
        let camera_service = CameraService::start(status_cb);

        let header = Header::builder()
            .launch(config.clone())
            .forward(sender.input_sender(), AppMsg::HeaderOutput);

        let d_pad = DPad::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::CameraAction);

        let zoom_focus = ZoomFocus::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::CameraAction);

        let presets = Presets::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::CameraAction);

        let welcome = Welcome::builder()
            .launch(())
            .forward(sender.input_sender(), AppMsg::WelcomeOutput);

        let mut model = App {
            config,
            header,
            d_pad,
            zoom_focus,
            presets,
            welcome,
            camera_service,
            stack: None,
            settings_dialog: None,
        };

        let widgets = view_output!();

        model.stack = Some(widgets.content_stack.clone());
        build_content_area(&widgets.content_stack, &model);

        let app = relm4::main_application();
        if app.lookup_action("camera-settings").is_none() {
            let settings_sender = sender.clone();
            let settings_action = gtk::gio::SimpleAction::new("camera-settings", None);
            settings_action.connect_activate(move |_, _| {
                let _ = settings_sender.input(AppMsg::OpenSettings { add_new: false });
            });
            app.add_action(&settings_action);
        }

        if !model.config.cameras.is_empty() {
            sender.input(AppMsg::ReconnectCamera);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::CameraAction(action) => {
                self.camera_service.send_action(action);
            }
            AppMsg::HeaderOutput(output) => match output {
                HeaderOutput::CameraSelected(id) => {
                    self.config.selected_camera_id = id;
                    self.config.save();
                    self.header.emit(crate::header::HeaderInput::UpdateCameras(
                        self.config.clone(),
                    ));
                    sender.input(AppMsg::ReconnectCamera);
                }
            },
            AppMsg::WelcomeOutput(WelcomeMsg::AddCamera) => {
                sender.input(AppMsg::OpenSettings { add_new: true });
            }
            AppMsg::OpenSettings { add_new } => {
                self.show_settings_dialog(&sender, add_new);
            }
            AppMsg::ConfigChanged(new_config) => {
                let had_cameras = !self.config.cameras.is_empty();
                let has_cameras = !new_config.cameras.is_empty();
                self.config = new_config;
                self.header.emit(crate::header::HeaderInput::UpdateCameras(
                    self.config.clone(),
                ));

                if had_cameras != has_cameras {
                    if let Some(ref stack) = self.stack {
                        build_content_area(stack, self);
                    }
                }

                if has_cameras {
                    sender.input(AppMsg::ReconnectCamera);
                }
            }
            AppMsg::ReconnectCamera => {
                #[cfg(feature = "simulator")]
                {
                    self.camera_service.reconnect(CameraConnection::Simulator);
                }
                #[cfg(not(feature = "simulator"))]
                {
                    if let Some(camera) = self.config.get_selected_camera() {
                        let conn = CameraConnection::from(&camera.connection);
                        self.camera_service.reconnect(conn);
                    }
                }
            }
            AppMsg::CameraStatus(status) => {
                self.header.emit(crate::header::HeaderInput::SetStatus(status));
            }
        }
    }
}

impl App {
    fn show_settings_dialog(&mut self, sender: &ComponentSender<Self>, add_new: bool) {
        let dialog = CameraPreferences::builder()
            .launch(self.config.clone())
            .forward(sender.input_sender(), |output| match output {
                CameraPreferencesOutput::ConfigChanged(config) => AppMsg::ConfigChanged(config),
            });

        if let Some(window) = relm4::main_application().active_window() {
            dialog.widget().present(Some(&window));
        }

        if add_new {
            dialog.emit(crate::camera_preferences::CameraPreferencesMsg::AddCamera);
        }

        self.settings_dialog = Some(dialog);
    }
}

fn build_content_area(stack: &gtk::Stack, model: &App) {
    while let Some(child) = stack.first_child() {
        stack.remove(&child);
    }

    if model.config.cameras.is_empty() {
        stack.add_named(model.welcome.widget(), Some("welcome"));
        stack.set_visible_child_name("welcome");
    } else {
        let controls_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        controls_box.set_margin_all(10);
        controls_box.append(model.d_pad.widget());
        controls_box.append(model.zoom_focus.widget());
        controls_box.append(model.presets.widget());
        stack.add_named(&controls_box, Some("controls"));
        stack.set_visible_child_name("controls");
    }
}
