use aviutl2_eframe::{eframe, egui};
use tap::prelude::*;

mod timecontrol;

static PRESET_PANEL_WIDTH_ID: std::sync::LazyLock<egui::Id> =
    std::sync::LazyLock::new(|| egui::Id::new("timecontrol_preset_panel_width"));

struct GlobalEditorGuiContext {
    egui_ctx: egui::Context,
    object_effects_info_sender: std::sync::mpsc::Sender<Option<crate::curve_io::ObjectEffectsInfo>>,
}

static GLOBAL_EDITOR_GUI_CONTEXT: std::sync::LazyLock<
    std::sync::Mutex<Option<GlobalEditorGuiContext>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

pub fn update_object_effects_info(info: Option<crate::curve_io::ObjectEffectsInfo>) {
    if let Some(context) = GLOBAL_EDITOR_GUI_CONTEXT.lock().unwrap().as_ref() {
        if let Err(e) = context.object_effects_info_sender.send(info) {
            tracing::error!("Failed to send object effects info: {:?}", e);
        }
        context.egui_ctx.request_repaint();
    }
}

struct SelectedTimeControlState {
    effect_handle: aviutl2::generic::EffectHandle,
    track_name: Vec<String>,
    time_control: crate::curve::TimeControl,
}

enum CurrentSelectionState {
    Selected(SelectedTimeControlState),
    NotSelected,
    NoTracks,
    None,
}
pub struct TimeControlEditorApp {
    object_effects_info_receiver:
        std::sync::mpsc::Receiver<Option<crate::curve_io::ObjectEffectsInfo>>,
    object_effects_info: Option<crate::curve_io::ObjectEffectsInfo>,
    timecontrol: CurrentSelectionState,
    timecontrol_clipboard: Option<crate::curve::TimeControl>,
    timecontrol_auto_scroll: bool,
    selected_point: usize,
    context_menu_position: Option<[f64; 2]>,
    preset_panel_width: f32,
    visible_y_bounds: Option<TimeControlVerticalBounds>,
    drag_scroll_y_bounds: Option<TimeControlVerticalBounds>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeControlVerticalBounds {
    pub min_y: f64,
    pub max_y: f64,
}

impl TimeControlVerticalBounds {
    fn y_range(self) -> f64 {
        self.max_y - self.min_y
    }

    fn center(self) -> f64 {
        (self.min_y + self.max_y) / 2.0
    }

    fn translate(self, delta: f64) -> Self {
        Self {
            min_y: self.min_y + delta,
            max_y: self.max_y + delta,
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min_y: self.min_y.min(other.min_y),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn with_center_and_range(center: f64, range: f64) -> Self {
        let half_range = range / 2.0;
        Self {
            min_y: center - half_range,
            max_y: center + half_range,
        }
    }

    fn with_anchor_and_range(anchor_y: f64, anchor_ratio: f64, range: f64) -> Self {
        let min_y = anchor_y - anchor_ratio * range;
        Self {
            min_y,
            max_y: min_y + range,
        }
    }

    fn clamp_to_content(self, content: Self) -> Self {
        let content_range = content.y_range().max(0.000_001);
        let range = self.y_range().clamp(content_range / 8.0, content_range);
        if range >= content_range {
            return content;
        }

        let mut bounds = Self::with_center_and_range(self.center(), range);
        if bounds.min_y < content.min_y {
            bounds = bounds.translate(content.min_y - bounds.min_y);
        }
        if bounds.max_y > content.max_y {
            bounds = bounds.translate(content.max_y - bounds.max_y);
        }
        bounds
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TimeControlHandleKind {
    In,
    Out,
}

impl TimeControlHandleKind {
    fn id(self) -> &'static str {
        match self {
            TimeControlHandleKind::In => "in",
            TimeControlHandleKind::Out => "out",
        }
    }
}

pub struct GuiColors {
    text: egui::Color32,
    grid_line: egui::Color32,
    zoom_gauge: egui::Color32,
    anchor: egui::Color32,
    anchor_line: egui::Color32,
    anchor_hover: egui::Color32,
    anchor_select: egui::Color32,
    object_section: egui::Color32,
}

pub static GUI_COLORS: std::sync::LazyLock<GuiColors> = std::sync::LazyLock::new(GuiColors::load);

impl GuiColors {
    fn load() -> Self {
        Self {
            text: color_code("Text"),
            grid_line: color_code("GridLine"),
            zoom_gauge: color_code("ZoomGauge"),
            anchor: color_code("Anchor"),
            anchor_line: color_code("AnchorLine"),
            anchor_hover: color_code("AnchorHover"),
            anchor_select: color_code("AnchorSelect"),
            object_section: color_code("ObjectSection"),
        }
    }
}

fn color_code(key: &str) -> egui::Color32 {
    aviutl2::config::get_color_code(key)
        .expect("色名にNull文字は含まれない")
        .unwrap_or_else(|| panic!("{key} が style.conf に存在しない"))
        .pipe(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
}

pub fn create_gui(
    cc: &eframe::CreationContext,
    _handle: aviutl2_eframe::AviUtl2EframeHandle,
) -> Result<Box<dyn eframe::App>, Box<dyn std::error::Error + Send + Sync>> {
    cc.egui_ctx.all_styles_mut(|style| {
        style.visuals = aviutl2_eframe::aviutl2_visuals();
    });
    cc.egui_ctx.set_fonts(aviutl2_eframe::aviutl2_fonts());
    let timecontrol_auto_scroll = cc.egui_ctx.data_mut(|data| {
        data.get_persisted::<bool>(*timecontrol::TIMECONTROL_AUTO_SCROLL_ID)
            .unwrap_or(true)
    });

    let (object_effects_info_sender, object_effects_info_receiver) =
        std::sync::mpsc::channel::<Option<crate::curve_io::ObjectEffectsInfo>>();
    GLOBAL_EDITOR_GUI_CONTEXT
        .lock()
        .unwrap()
        .replace(GlobalEditorGuiContext {
            egui_ctx: cc.egui_ctx.clone(),
            object_effects_info_sender,
        });

    Ok(Box::new(TimeControlEditorApp {
        object_effects_info_receiver,
        object_effects_info: None,
        timecontrol: CurrentSelectionState::None,
        timecontrol_clipboard: None,
        timecontrol_auto_scroll,
        selected_point: 0,
        context_menu_position: None,
        preset_panel_width: f32::NAN,
        visible_y_bounds: None,
        drag_scroll_y_bounds: None,
    }))
}

impl eframe::App for TimeControlEditorApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(info) = self.object_effects_info_receiver.try_recv() {
            match info {
                Some(info) => {
                    let current_object_handle = self
                        .object_effects_info
                        .as_ref()
                        .map(|current_info| current_info.handle);
                    let (timecontrol, selection_kept) = Self::selection_state_for_info(
                        &self.timecontrol,
                        current_object_handle,
                        &info,
                    );
                    self.object_effects_info = Some(info);
                    self.timecontrol = timecontrol;
                    if !selection_kept {
                        self.reset_timecontrol_editor_state();
                    }
                }
                None => {
                    self.timecontrol = CurrentSelectionState::None;
                    self.reset_timecontrol_editor_state();
                }
            };
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        match self.timecontrol {
            CurrentSelectionState::Selected(_) | CurrentSelectionState::NotSelected => {
                egui::Panel::top("timecontrol_editor_top_panel").show(ui, |ui| {
                    self.render_target_switcher(ui);
                });
                if matches!(self.timecontrol, CurrentSelectionState::NotSelected) {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.label("編集するトラックを選択してください");
                        });
                    });
                } else {
                    egui::CentralPanel::default().show(ui, |ui| {
                        self.render_timecontrol_editor(ui);
                    });
                }
            }
            CurrentSelectionState::NoTracks => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("オブジェクトに時間制御があるトラックが存在しません");
                    });
                });
            }
            CurrentSelectionState::None => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("オブジェクトを選択してください");
                    });
                });
            }
        }
    }
}

impl TimeControlEditorApp {
    fn track_group_label(track_names: &[String]) -> String {
        assert!(!track_names.is_empty(), "Track group must not be empty");
        track_names.join(", ")
    }

    fn selection_state_for_info(
        current: &CurrentSelectionState,
        current_object_handle: Option<aviutl2::generic::ObjectHandle>,
        info: &crate::curve_io::ObjectEffectsInfo,
    ) -> (CurrentSelectionState, bool) {
        let same_object = current_object_handle == Some(info.handle);
        if same_object {
            match current {
                CurrentSelectionState::Selected(current) => {
                    let current_track = info
                        .effects
                        .iter()
                        .find(|effect| effect.handle == current.effect_handle)
                        .and_then(|effect| {
                            effect
                                .tracks
                                .iter()
                                .find(|track| track.track_name == current.track_name)
                        });
                    if let Some(current_track) = current_track {
                        let time_control = match &current_track.curve {
                            Some(time_control) => time_control.clone(),
                            None => current.time_control.clone(),
                        };
                        return (
                            CurrentSelectionState::Selected(SelectedTimeControlState {
                                effect_handle: current.effect_handle,
                                track_name: current.track_name.clone(),
                                time_control,
                            }),
                            true,
                        );
                    }
                }
                CurrentSelectionState::NotSelected
                    if info.effects.iter().any(|effect| !effect.tracks.is_empty()) =>
                {
                    return (CurrentSelectionState::NotSelected, true);
                }
                CurrentSelectionState::NoTracks
                    if info.effects.iter().all(|effect| effect.tracks.is_empty()) =>
                {
                    return (CurrentSelectionState::NoTracks, true);
                }
                CurrentSelectionState::NotSelected
                | CurrentSelectionState::NoTracks
                | CurrentSelectionState::None => {}
            }
        }

        let first_timecontrol = info.effects.iter().find_map(|effect| {
            effect.tracks.iter().find_map(|track| {
                track
                    .curve
                    .as_ref()
                    .map(|time_control| SelectedTimeControlState {
                        effect_handle: effect.handle,
                        track_name: track.track_name.clone(),
                        time_control: time_control.clone(),
                    })
            })
        });
        let state = match first_timecontrol {
            Some(timecontrol) => CurrentSelectionState::Selected(timecontrol),
            None if info.effects.iter().all(|effect| effect.tracks.is_empty()) => {
                CurrentSelectionState::NoTracks
            }
            None => CurrentSelectionState::NotSelected,
        };
        (state, false)
    }

    fn reset_timecontrol_editor_state(&mut self) {
        self.selected_point = 0;
        self.context_menu_position = None;
        self.visible_y_bounds = None;
        self.drag_scroll_y_bounds = None;
    }

    fn render_target_switcher(&mut self, ui: &mut egui::Ui) {
        if let Some(info) = &self.object_effects_info {
            struct NewSelectedState {
                effect_handle: aviutl2::generic::EffectHandle,
                track_name: Vec<String>,
            }
            let mut new_selected_state = None;
            egui::ComboBox::from_id_salt("target_switcher_combobox")
                .selected_text(match &self.timecontrol {
                    CurrentSelectionState::Selected(state) => {
                        format!(
                            "{} / {}",
                            info.effects
                                .iter()
                                .find(|effect| effect.handle == state.effect_handle)
                                .map(|effect| effect.name.clone())
                                .unwrap_or_else(|| "Unknown Effect".to_string()),
                            Self::track_group_label(&state.track_name)
                        )
                    }
                    CurrentSelectionState::NotSelected => "Select a track".to_string(),
                    CurrentSelectionState::NoTracks | CurrentSelectionState::None => unreachable!(),
                })
                .show_ui(ui, |ui| {
                    egui::menu::menu_style(ui.style_mut());
                    egui::containers::ScrollArea::vertical().show(ui, |ui| {
                        for effect in &info.effects {
                            ui.add_enabled_ui(!effect.tracks.is_empty(), |ui| {
                                egui::menu::SubMenuButton::new(&effect.name).ui(ui, |ui| {
                                    egui::containers::ScrollArea::vertical().show(ui, |ui| {
                                        egui::menu::menu_style(ui.style_mut());
                                        for track in &effect.tracks {
                                            if ui
                                                .button(Self::track_group_label(&track.track_name))
                                                .on_hover_text(if track.curve.is_some() {
                                                    "時間制御トラックを編集する"
                                                } else {
                                                    "時間制御トラックが存在しません"
                                                })
                                                .clicked()
                                            {
                                                new_selected_state = Some(NewSelectedState {
                                                    effect_handle: effect.handle,
                                                    track_name: track.track_name.clone(),
                                                });
                                            }
                                        }
                                    });
                                })
                            });
                        }
                    });
                });

            if let Some(new_state) = new_selected_state {
                let new_timecontrol = info
                    .effects
                    .iter()
                    .find(|effect| effect.handle == new_state.effect_handle)
                    .and_then(|effect| {
                        effect
                            .tracks
                            .iter()
                            .find(|track| track.track_name == new_state.track_name)
                            .and_then(|track| track.curve.clone())
                    });
                match new_timecontrol {
                    Some(time_control) => {
                        self.timecontrol =
                            CurrentSelectionState::Selected(SelectedTimeControlState {
                                effect_handle: new_state.effect_handle,
                                track_name: new_state.track_name,
                                time_control,
                            });
                    }
                    None => {
                        let res = crate::EDIT_HANDLE
                            .call_edit_section(|edit| {
                                crate::curve_io::write_curve_to_track(
                                    edit,
                                    new_state.effect_handle,
                                    &new_state.track_name,
                                    &crate::curve::TimeControl::default(),
                                )
                            })
                            .map_err(|e| e.into())
                            .flatten();
                        if let Err(e) = res {
                            tracing::error!("Failed to write default curve to track: {:?}", e);
                        }
                        self.timecontrol =
                            CurrentSelectionState::Selected(SelectedTimeControlState {
                                effect_handle: new_state.effect_handle,
                                track_name: new_state.track_name,
                                time_control: crate::curve::TimeControl::default(),
                            });
                    }
                }
            }
        }
    }

    fn render_timecontrol_editor(&mut self, ui: &mut egui::Ui) {
        let content_size = ui.available_size();
        let total_width = content_size.x;
        let content_height = content_size.y;
        let separator_width = 8.0;
        if !self.preset_panel_width.is_finite() {
            self.preset_panel_width = ui
                .data_mut(|data| data.get_persisted::<f32>(*PRESET_PANEL_WIDTH_ID))
                .unwrap_or_else(|| (total_width - content_height - separator_width).max(0.0));
            assert!(self.preset_panel_width.is_finite());
        }
        self.preset_panel_width = self
            .preset_panel_width
            .clamp(0.0, (total_width - separator_width).max(0.0));

        let preset_width = self.preset_panel_width;
        let editor_width = (total_width - preset_width - separator_width).max(0.0);
        let (content_rect, _) = ui.allocate_exact_size(content_size, egui::Sense::hover());
        let editor_rect =
            egui::Rect::from_min_size(content_rect.min, egui::vec2(editor_width, content_height));
        let separator_rect = egui::Rect::from_min_size(
            egui::pos2(editor_rect.right(), content_rect.top()),
            egui::vec2(separator_width, content_height),
        );
        let preset_rect = egui::Rect::from_min_size(
            egui::pos2(separator_rect.right(), content_rect.top()),
            egui::vec2(preset_width, content_height),
        );

        if editor_width > 1.0 {
            let mut editor_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(editor_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            editor_ui.set_clip_rect(editor_rect);
            let CurrentSelectionState::Selected(ref mut state) = self.timecontrol else {
                unreachable!()
            };
            let (_changed, commit_requested) = Self::show_timecontrol_bezier_editor(
                &mut editor_ui,
                &mut state.time_control,
                &mut self.selected_point,
                &mut self.context_menu_position,
                &mut self.timecontrol_clipboard,
                &mut self.timecontrol_auto_scroll,
                &mut self.visible_y_bounds,
                &mut self.drag_scroll_y_bounds,
            );
            if commit_requested {
                Self::apply_timecontrol_state(state);
            }
        }

        let separator_response = ui.interact(
            separator_rect,
            ui.id().with("timecontrol_editor_separator"),
            egui::Sense::drag(),
        );
        if separator_response.hovered() || separator_response.dragged() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::ResizeHorizontal);
        }
        ui.painter().line_segment(
            [separator_rect.center_top(), separator_rect.center_bottom()],
            egui::Stroke::new(1.0, GUI_COLORS.grid_line),
        );
        if separator_response.dragged() {
            self.preset_panel_width = (self.preset_panel_width - separator_response.drag_delta().x)
                .clamp(0.0, (total_width - separator_width).max(0.0));
            ui.data_mut(|data| {
                data.insert_persisted(*PRESET_PANEL_WIDTH_ID, self.preset_panel_width);
            });
        }

        if preset_width > 1.0 {
            let mut preset_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(preset_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            preset_ui.set_clip_rect(preset_rect);
            if let Some(timecontrol) = Self::show_timecontrol_presets(&mut preset_ui) {
                match &mut self.timecontrol {
                    CurrentSelectionState::Selected(state) => {
                        state.time_control = timecontrol;
                        Self::apply_timecontrol_state(state);
                    }
                    _ => unreachable!(),
                }
                self.selected_point = 0;
                self.context_menu_position = None;
                self.visible_y_bounds = None;
                self.drag_scroll_y_bounds = None;
            }
        }
    }

    fn apply_timecontrol_state(state: &SelectedTimeControlState) {
        let edit_result = crate::EDIT_HANDLE.call_edit_section(|edit| {
            crate::curve_io::write_curve_to_track(
                edit,
                state.effect_handle,
                &state.track_name,
                &state.time_control,
            )
        });
        if let Err(e) = edit_result {
            tracing::error!("Failed to write curve to track: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_handle(value: usize) -> aviutl2::generic::ObjectHandle {
        aviutl2::generic::ObjectHandle::from(value as *mut std::ffi::c_void)
    }

    fn effect_handle(value: usize) -> aviutl2::generic::EffectHandle {
        aviutl2::generic::EffectHandle::from(value as *mut std::ffi::c_void)
    }

    fn effect_info(
        handle: aviutl2::generic::EffectHandle,
        tracks: Vec<(Vec<&str>, Option<crate::curve::TimeControl>)>,
    ) -> crate::curve_io::EffectTracksInfo {
        crate::curve_io::EffectTracksInfo {
            name: format!("Effect {handle:?}"),
            handle,
            tracks: tracks
                .into_iter()
                .map(|(track_name, curve)| crate::curve_io::TrackInfo {
                    track_name: track_name.into_iter().map(str::to_string).collect(),
                    curve,
                })
                .collect(),
        }
    }

    #[test]
    fn refreshed_info_keeps_a_selected_target_that_is_not_first() {
        let object = object_handle(1);
        let first_effect = effect_handle(1);
        let selected_effect = effect_handle(2);
        let current = CurrentSelectionState::Selected(SelectedTimeControlState {
            effect_handle: selected_effect,
            track_name: vec!["Selected X".to_string(), "Selected Y".to_string()],
            time_control: crate::curve::TimeControl::default(),
        });
        let info = crate::curve_io::ObjectEffectsInfo {
            handle: object,
            effects: vec![
                effect_info(
                    first_effect,
                    vec![(vec!["First"], Some(crate::curve::TimeControl::default()))],
                ),
                effect_info(
                    selected_effect,
                    vec![(
                        vec!["Selected X", "Selected Y"],
                        Some(crate::curve::TimeControl::default_for_mode(
                            crate::curve::TimeControlMode::Elastic,
                        )),
                    )],
                ),
            ],
        };

        let (next, selection_kept) =
            TimeControlEditorApp::selection_state_for_info(&current, Some(object), &info);

        assert!(selection_kept);
        let CurrentSelectionState::Selected(next) = next else {
            panic!("the selected target must remain selected");
        };
        assert_eq!(next.effect_handle, selected_effect);
        assert_eq!(next.track_name, ["Selected X", "Selected Y"]);
        assert_eq!(
            next.time_control.mode(),
            crate::curve::TimeControlMode::Elastic
        );
    }

    #[test]
    fn refreshed_info_keeps_the_current_curve_if_the_target_curve_is_temporarily_missing() {
        let object = object_handle(1);
        let selected_effect = effect_handle(1);
        let current = CurrentSelectionState::Selected(SelectedTimeControlState {
            effect_handle: selected_effect,
            track_name: vec!["Selected X".to_string(), "Selected Y".to_string()],
            time_control: crate::curve::TimeControl::default_for_mode(
                crate::curve::TimeControlMode::Bounce,
            ),
        });
        let info = crate::curve_io::ObjectEffectsInfo {
            handle: object,
            effects: vec![effect_info(
                selected_effect,
                vec![(vec!["Selected X", "Selected Y"], None)],
            )],
        };

        let (next, selection_kept) =
            TimeControlEditorApp::selection_state_for_info(&current, Some(object), &info);

        assert!(selection_kept);
        let CurrentSelectionState::Selected(next) = next else {
            panic!("the selected target must remain selected");
        };
        assert_eq!(
            next.time_control.mode(),
            crate::curve::TimeControlMode::Bounce
        );
    }

    #[test]
    fn refreshed_info_resets_selection_if_the_track_group_changes() {
        let object = object_handle(1);
        let selected_effect = effect_handle(1);
        let current = CurrentSelectionState::Selected(SelectedTimeControlState {
            effect_handle: selected_effect,
            track_name: vec!["Selected X".to_string(), "Selected Y".to_string()],
            time_control: crate::curve::TimeControl::default(),
        });
        let info = crate::curve_io::ObjectEffectsInfo {
            handle: object,
            effects: vec![effect_info(
                selected_effect,
                vec![(
                    vec!["Selected X", "Selected Z"],
                    Some(crate::curve::TimeControl::default()),
                )],
            )],
        };

        let (next, selection_kept) =
            TimeControlEditorApp::selection_state_for_info(&current, Some(object), &info);

        assert!(!selection_kept);
        let CurrentSelectionState::Selected(next) = next else {
            panic!("the changed track group must become the new selection");
        };
        assert_eq!(next.track_name, ["Selected X", "Selected Z"]);
    }

    #[test]
    fn track_group_label_lists_all_members() {
        assert_eq!(
            TimeControlEditorApp::track_group_label(&["Single".to_string()]),
            "Single"
        );
        assert_eq!(
            TimeControlEditorApp::track_group_label(&[
                "Position X".to_string(),
                "Position Y".to_string(),
            ]),
            "Position X, Position Y"
        );
    }

    #[test]
    fn refreshed_info_resets_selection_for_a_different_object() {
        let selected_effect = effect_handle(1);
        let current = CurrentSelectionState::Selected(SelectedTimeControlState {
            effect_handle: selected_effect,
            track_name: vec!["Selected".to_string()],
            time_control: crate::curve::TimeControl::default(),
        });
        let next_object = object_handle(2);
        let info = crate::curve_io::ObjectEffectsInfo {
            handle: next_object,
            effects: vec![effect_info(
                selected_effect,
                vec![(
                    vec!["First"],
                    Some(crate::curve::TimeControl::default_for_mode(
                        crate::curve::TimeControlMode::Elastic,
                    )),
                )],
            )],
        };

        let (next, selection_kept) =
            TimeControlEditorApp::selection_state_for_info(&current, Some(object_handle(1)), &info);

        assert!(!selection_kept);
        let CurrentSelectionState::Selected(next) = next else {
            panic!("the first target of the new object must be selected");
        };
        assert_eq!(next.track_name, ["First"]);
    }
}
