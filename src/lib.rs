use aviutl2::tracing;

static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();

mod curve;
mod curve_io;
mod gui;
mod watcher;

#[aviutl2::plugin(GenericPlugin)]
struct JailbrokenTimeControlEditorAux2 {
    gui: aviutl2_eframe::EframeWindow,
    watcher: watcher::WatcherThread,
}

#[derive(Debug, Clone)]
pub struct TimeControlTarget {
    effect_handle: aviutl2::generic::EffectHandle,
    track_name: String,
}

impl aviutl2::generic::GenericPlugin for JailbrokenTimeControlEditorAux2 {
    fn new(_info: aviutl2::common::AviUtl2Info) -> aviutl2::common::AnyResult<Self> {
        aviutl2::tracing_subscriber::fmt()
            .with_max_level(if cfg!(debug_assertions) {
                tracing::Level::DEBUG
            } else {
                tracing::Level::INFO
            })
            .event_format(aviutl2::logger::AviUtl2Formatter)
            .with_writer(aviutl2::logger::AviUtl2LogWriter)
            .init();

        let gui = aviutl2_eframe::EframeWindow::new(
            "anarchy_time_control_editor.aux2",
            crate::gui::create_gui,
        )?;
        Ok(Self {
            gui: gui,
            watcher: watcher::WatcherThread::start(),
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "anarchy_time_control_editor.aux2".to_string(),
            information: format!(
                "Anarchy Time Control Editor / v{} / https://github.com/sevenc-nanashi/anarchy_time_control_editor.aux2",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        EDIT_HANDLE.init(registry.create_edit_handle());
        match self.gui.handle() {
            Ok(handle) => {
                let _ =
                    registry.register_window_client("anarchy_time_control_editor.aux2", &handle);
            }
            Err(error) => {
                tracing::error!("Failed to register GUI window: {error:?}");
            }
        }
    }

    fn event_change_scene_info(&mut self) {
        self.watcher.notify_object_change();
    }

    fn event_update_object_info(&mut self) {
        self.watcher.notify_object_change();
    }

    fn event_change_focus_object(&mut self) {
        self.watcher.notify_object_selected();
    }
}

aviutl2::register_generic_plugin!(JailbrokenTimeControlEditorAux2);
