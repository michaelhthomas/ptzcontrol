use gtk::prelude::*;
use relm4::adw::prelude::*;
use relm4::prelude::*;

use crate::camera::CameraStatus;
use crate::config::AppConfig;

pub struct Header {
    cameras: Vec<(u64, String)>,
    dropdown: Option<gtk::DropDown>,
    status_dot: Option<gtk::DrawingArea>,
    selected_notify_handler: Option<gtk::glib::SignalHandlerId>,
}

#[derive(Debug)]
pub enum HeaderInput {
    UpdateCameras(AppConfig),
    SetStatus(CameraStatus),
}

#[derive(Debug)]
pub enum HeaderOutput {
    CameraSelected(Option<u64>),
}

#[relm4::component(pub)]
impl SimpleComponent for Header {
    type Init = AppConfig;
    type Input = HeaderInput;
    type Output = HeaderOutput;

    view! {
        adw::HeaderBar {
            #[name = "status_dot"]
            pack_start = &gtk::DrawingArea {
                set_size_request: (10, 10),
                set_valign: gtk::Align::Center,
                set_vexpand: false,
                set_halign: gtk::Align::Center,
                set_css_classes: &["status-dot", "status-disconnected"],
            },

            #[name = "camera_dropdown"]
            pack_start = &gtk::DropDown {
                set_halign: gtk::Align::Start,
            },

            #[wrap(Some)]
            set_title_widget = &adw::WindowTitle {},

            #[name = "menu_button"]
            pack_end = &gtk::MenuButton {
                set_icon_name: crate::icon_names::OPEN_MENU,
                set_tooltip_text: Some("Main Menu"),
                set_primary: true,
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let cameras: Vec<(u64, String)> = init
            .cameras
            .iter()
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let mut model = Header {
            cameras,
            dropdown: None,
            status_dot: None,
            selected_notify_handler: None,
        };
        let widgets = view_output!();

        model.dropdown = Some(widgets.camera_dropdown.clone());
        model.status_dot = Some(widgets.status_dot.clone());
        
        // Connect the signal and store the handler ID
        let handler = widgets.camera_dropdown.connect_selected_notify({
            let sender = sender.clone();
            let cameras = model.cameras.clone();
            move |dropdown| {
                let idx = dropdown.selected() as usize;
                let selected_id = cameras.get(idx).map(|(id, _)| *id);
                let _ = sender.output(HeaderOutput::CameraSelected(selected_id));
            }
        });
        model.selected_notify_handler = Some(handler);
        
        update_dropdown(&widgets.camera_dropdown, &model, init.selected_camera_id);

        if let Some(button) = widgets.camera_dropdown.first_child() {
            button.add_css_class("flat");
        }

        let menu = gtk::gio::Menu::new();
        menu.append(Some("_Camera Settings"), Some("app.camera-settings"));
        menu.append(Some("_About"), Some("app.about"));
        widgets.menu_button.set_menu_model(Some(&menu));

        let app = relm4::main_application();
        if app.lookup_action("about").is_none() {
            let app_for_action = app.clone();
            let about_action = gtk::gio::SimpleAction::new("about", None);
            about_action.connect_activate(move |_, _| {
                let dialog = adw::AboutDialog::builder()
                    .application_name("PTZ Camera Controller")
                    .application_icon("xyz.michaelt.ptzcontrol")
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("Michael Thomas")
                    .developers(["Michael Thomas"].as_slice())
                    .comments("Control PTZ cameras from your desktop.")
                    .license_type(gtk::License::Gpl30)
                    .copyright("© 2026 Michael Thomas")
                    .website("https://github.com/michaelhthomas/ptzcontrol")
                    .issue_url("https://github.com/michaelhthomas/ptzcontrol/issues")
                    .translator_credits("translator-credits")
                    .build();

                if let Some(window) = app_for_action.active_window() {
                    dialog.present(Some(&window));
                }
            });
            app.add_action(&about_action);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            HeaderInput::UpdateCameras(config) => {
                self.cameras = config
                    .cameras
                    .iter()
                    .map(|c| (c.id, c.name.clone()))
                    .collect();
                if let Some(ref dropdown) = self.dropdown {
                    update_dropdown(dropdown, self, config.selected_camera_id);
                }
            }
            HeaderInput::SetStatus(status) => {
                let (text, css_class) = match status {
                    CameraStatus::Connecting => ("Connecting…", "status-connecting"),
                    CameraStatus::Connected => ("Connected", "status-connected"),
                    CameraStatus::Reconnecting => ("Reconnecting…", "status-reconnecting"),
                    CameraStatus::Disconnected => ("Disconnected", "status-disconnected"),
                };
                if let Some(ref dot) = self.status_dot {
                    dot.set_tooltip_text(Some(text));
                    dot.remove_css_class("status-connecting");
                    dot.remove_css_class("status-connected");
                    dot.remove_css_class("status-reconnecting");
                    dot.remove_css_class("status-disconnected");
                    dot.add_css_class(css_class);
                }
            }
        }
    }
}

fn update_dropdown(dropdown: &gtk::DropDown, model: &Header, selected_id: Option<u64>) {
    // Block the signal handler during programmatic updates to prevent infinite loops
    if let Some(ref handler) = model.selected_notify_handler {
        dropdown.block_signal(handler);
    }
    
    let names: Vec<&str> = model.cameras.iter().map(|(_, name)| name.as_str()).collect();
    let string_list = gtk::StringList::new(&names);
    dropdown.set_model(Some(&string_list));

    if let Some(selected_id) = selected_id {
        if let Some(idx) = model.cameras.iter().position(|(id, _)| *id == selected_id) {
            dropdown.set_selected(idx as u32);
        }
    }

    dropdown.set_visible(!model.cameras.is_empty());
    
    // Unblock the signal handler
    if let Some(ref handler) = model.selected_notify_handler {
        dropdown.unblock_signal(handler);
    }
}
