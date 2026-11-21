use grafton_visca::types::PanTiltDirection;
use gtk::prelude::*;
use relm4::prelude::*;

use crate::camera::CameraAction;

pub struct DPad;

#[relm4::component(pub)]
impl SimpleComponent for DPad {
    type Init = ();
    type Input = ();
    type Output = CameraAction;

    view! {
        gtk::Grid {
            set_row_spacing: 5,
            set_column_spacing: 5,
            set_halign: gtk::Align::Center,
            set_margin_all: 10,

            #[name = "up_left_btn"]
            attach[0, 0, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::ARROW_TOP_LEFT,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "up_btn"]
            attach[1, 0, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::GO_UP,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "up_right_btn"]
            attach[2, 0, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::ARROW_TOP_RIGHT,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "left_btn"]
            attach[0, 1, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::GO_PREVIOUS,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "home_btn"]
            attach[1, 1, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::GO_HOME,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "right_btn"]
            attach[2, 1, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::GO_NEXT,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "down_left_btn"]
            attach[0, 2, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::ARROW_TOP_LEFT,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "down_btn"]
            attach[1, 2, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::GO_DOWN,
                set_width_request: 80,
                set_height_request: 80,
            },

            #[name = "down_right_btn"]
            attach[2, 2, 1, 1] = &gtk::Button {
                set_icon_name: crate::icon_names::ARROW_TOP_RIGHT,
                set_width_request: 80,
                set_height_request: 80,
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = DPad;
        let widgets = view_output!();

        widgets.down_left_btn.set_css_classes(&["rotate-180"]);
        widgets.down_right_btn.set_css_classes(&["rotate-180"]);

        let directions = [
            (&widgets.up_left_btn, PanTiltDirection::UpLeft),
            (&widgets.up_btn, PanTiltDirection::Up),
            (&widgets.up_right_btn, PanTiltDirection::UpRight),
            (&widgets.left_btn, PanTiltDirection::Left),
            (&widgets.right_btn, PanTiltDirection::Right),
            (&widgets.down_left_btn, PanTiltDirection::DownLeft),
            (&widgets.down_btn, PanTiltDirection::Down),
            (&widgets.down_right_btn, PanTiltDirection::DownRight),
        ];

        for (button, direction) in directions {
            let gesture = gtk::GestureClick::new();
            gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
            let sender_pressed = sender.clone();
            let direction_pressed = direction.clone();
            gesture.connect_pressed(move |_, _, _, _| {
                let _ = sender_pressed.output(CameraAction::PanTiltMove {
                    direction: direction_pressed.clone(),
                });
            });
            let sender_released = sender.clone();
            gesture.connect_released(move |_, _, _, _| {
                let _ = sender_released.output(CameraAction::PanTiltStop);
            });
            button.add_controller(gesture);
        }

        let home_gesture = gtk::GestureClick::new();
        home_gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let sender_pressed = sender.clone();
        home_gesture.connect_pressed(move |_, _, _, _| {
            let _ = sender_pressed.output(CameraAction::PanTiltHome);
        });
        widgets.home_btn.add_controller(home_gesture);

        ComponentParts { model, widgets }
    }
}
