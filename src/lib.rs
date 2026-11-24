use aviutl2::tracing;

static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();
static EFFECT_ITEMS_CACHE: std::sync::LazyLock<
    std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<[aviutl2::generic::EffectItemInfo]>>,
    >,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

mod curve;
mod curve_io;
mod gui;
mod utils;
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

    fn on_project_load(&mut self, _project: &mut aviutl2::generic::ProjectFile) {
        EFFECT_ITEMS_CACHE.lock().unwrap().clear();
    }

    fn on_clear_cache(&mut self, _edit: &aviutl2::generic::EditSection) {
        EFFECT_ITEMS_CACHE.lock().unwrap().clear();
        self.watcher.notify_object_change();
    }

    fn event_reload_language(&mut self) {
        gui::request_repaint();
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

fn get_effect_items(
    effect_name: &str,
) -> anyhow::Result<std::sync::Arc<[aviutl2::generic::EffectItemInfo]>> {
    let cached = EFFECT_ITEMS_CACHE.lock().unwrap().get(effect_name).cloned();
    if let Some(items) = cached {
        return Ok(items);
    }
    let items: std::sync::Arc<[_]> = EDIT_HANDLE.get_effect_items(effect_name)?.into();
    EFFECT_ITEMS_CACHE
        .lock()
        .unwrap()
        .insert(effect_name.to_string(), items.clone());
    Ok(items)
}

aviutl2::register_generic_plugin!(JailbrokenTimeControlEditorAux2);
