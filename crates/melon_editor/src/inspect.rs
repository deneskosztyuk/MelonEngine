//! Generic editor for reflected values: one widget per reflected field kind.

use melon_core::bevy_reflect::{
    PartialReflect, ReflectMut, ReflectRef, TypeRegistry,
    enums::{DynamicEnum, DynamicVariant, Enum, VariantInfo},
    list::List,
    std_traits::ReflectDefault,
    tuple::DynamicTuple,
};
use melon_core::glam::{EulerRot, Quat, Vec2, Vec3, Vec4};

use crate::content_view::id_label;
use crate::{inspector_value, theme};

const FLOAT_SPEED: f64 = 0.01;

/// Edit `value` in place. A struct or tuple struct draws one grid row per
/// field; each row's editor gets the id `id.with(field_name)` (nested fields
/// `id.with(field).with(sub_field)`). `registry` supplies default values for
/// new `Option` contents and list elements. True when the user changed
/// something.
pub fn reflect_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut dyn PartialReflect,
    registry: &TypeRegistry,
) -> bool {
    match value.reflect_ref() {
        ReflectRef::Struct(_) | ReflectRef::TupleStruct(_) => fields_ui(ui, id, value, registry),
        _ => value_ui(ui, id, value, registry),
    }
}

/// One grid row per field of a struct or tuple struct.
fn fields_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut dyn PartialReflect,
    registry: &TypeRegistry,
) -> bool {
    let names: Vec<String> = match value.reflect_ref() {
        ReflectRef::Struct(s) => (0..s.field_len())
            .map(|i| s.name_at(i).unwrap_or_default().to_string())
            .collect(),
        ReflectRef::TupleStruct(s) => (0..s.field_len()).map(|i| i.to_string()).collect(),
        _ => Vec::new(),
    };
    let mut changed = false;
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([theme::sizes::SPACING; 2])
        .with_row_color(|row, _| Some(theme::colors::INSPECTOR_ROWS[row % 2]))
        .show(ui, |ui| {
            for (i, name) in names.into_iter().enumerate() {
                let field = match value.reflect_mut() {
                    ReflectMut::Struct(s) => s.field_at_mut(i),
                    ReflectMut::TupleStruct(s) => s.field_mut(i),
                    _ => None,
                };
                let Some(field) = field else { continue };
                ui.label(&name);
                let row = id.with(name.as_str());
                let editor = ui.scope(|ui| value_ui(ui, row, field, registry));
                // Fixed id over the editor (widget ids are positional).
                ui.interact(editor.response.rect, row, egui::Sense::hover());
                changed |= editor.inner;
                ui.end_row();
            }
        });
    changed
}

fn value_ui(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &mut dyn PartialReflect,
    registry: &TypeRegistry,
) -> bool {
    macro_rules! drag {
        ($($t:ty),*) => {$(
            if let Some(v) = value.try_downcast_mut::<$t>() {
                return ui.add(egui::DragValue::new(v)).changed();
            }
        )*};
    }
    drag!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);
    if let Some(v) = value.try_downcast_mut::<f32>() {
        return ui.add(float_drag(v)).changed();
    }
    if let Some(v) = value.try_downcast_mut::<f64>() {
        return ui.add(float_drag(v)).changed();
    }
    if let Some(v) = value.try_downcast_mut::<bool>() {
        return ui.checkbox(v, "").changed();
    }
    if let Some(v) = value.try_downcast_mut::<String>() {
        return ui.text_edit_singleline(v).changed();
    }
    // Remote-reflected glam fields report the wrapper's type path
    // (`Vec3Reflect`) but downcast to the glam type itself.
    if let Some(v) = value.try_downcast_mut::<Vec2>() {
        let mut a = v.to_array();
        let changed = axes_ui(ui, &mut a);
        if changed {
            *v = Vec2::from_array(a);
        }
        return changed;
    }
    if let Some(v) = value.try_downcast_mut::<Vec3>() {
        let mut a = v.to_array();
        let changed = axes_ui(ui, &mut a);
        if changed {
            *v = Vec3::from_array(a);
        }
        return changed;
    }
    if let Some(v) = value.try_downcast_mut::<Vec4>() {
        let mut a = v.to_array();
        let changed = axes_ui(ui, &mut a);
        if changed {
            *v = Vec4::from_array(a);
        }
        return changed;
    }
    if let Some(q) = value.try_downcast_mut::<Quat>() {
        return quat_ui(ui, q);
    }
    if value
        .reflect_type_path()
        .starts_with("core::option::Option<")
        && let ReflectMut::Enum(e) = value.reflect_mut()
    {
        return option_ui(ui, id, e, registry);
    }
    if let ReflectMut::Enum(e) = value.reflect_mut()
        && let Some(info) = e
            .get_represented_enum_info()
            .filter(|info| info.iter().all(|v| matches!(v, VariantInfo::Unit(_))))
    {
        return unit_enum_ui(ui, id, e, info.variant_names());
    }
    if matches!(
        value.reflect_ref(),
        ReflectRef::Struct(_) | ReflectRef::TupleStruct(_)
    ) {
        let title = value.reflect_short_type_path().to_string();
        return egui::CollapsingHeader::new(title)
            .id_salt(id.with("header"))
            .default_open(true)
            .show(ui, |ui| fields_ui(ui, id, value, registry))
            .body_returned
            .unwrap_or(false);
    }
    if let ReflectMut::List(list) = value.reflect_mut() {
        return list_ui(ui, id, list, registry);
    }
    inspector_value(ui, format!("{value:?}"));
    false
}

fn float_drag<N: egui::emath::Numeric>(value: &mut N) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(FLOAT_SPEED)
        .max_decimals(3)
}

/// X / Y / Z / W drag values. True if one changed.
fn axes_ui(ui: &mut egui::Ui, values: &mut [f32]) -> bool {
    let axes = [
        ("X", theme::colors::AXIS_X),
        ("Y", theme::colors::AXIS_Y),
        ("Z", theme::colors::AXIS_Z),
        ("W", theme::colors::TEXT_MUTED),
    ];
    let mut changed = false;
    ui.horizontal(|ui| {
        for (value, (label, color)) in values.iter_mut().zip(axes) {
            ui.label(egui::RichText::new(label).color(color).strong());
            changed |= ui.add(float_drag(value)).changed();
        }
    });
    changed
}

/// Euler degrees (YXZ), shown as X / Y / Z. The quaternion is rebuilt only
/// when an angle was edited, so drawing never drifts it.
fn quat_ui(ui: &mut egui::Ui, q: &mut Quat) -> bool {
    let (y, x, z) = q.to_euler(EulerRot::YXZ);
    let before = [x, y, z].map(f32::to_degrees);
    let mut degrees = before;
    if axes_ui(ui, &mut degrees) && degrees != before {
        let [x, y, z] = degrees.map(f32::to_radians);
        *q = Quat::from_euler(EulerRot::YXZ, y, x, z);
        return true;
    }
    false
}

/// Combo box of variant names; options have fixed ids `id.with(variant)`.
fn unit_enum_ui(ui: &mut egui::Ui, id: egui::Id, e: &mut dyn Enum, names: &[&str]) -> bool {
    let current = e.variant_name().to_string();
    let mut picked = None;
    egui::ComboBox::from_id_salt(id.with("combo"))
        .selected_text(&current)
        .show_ui(ui, |ui| {
            for &name in names {
                if id_label(ui, id.with(name), name, name == current).clicked() && name != current {
                    picked = Some(name);
                }
            }
        });
    picked.is_some_and(|name| e.try_apply(&DynamicEnum::new(name, ())).is_ok())
}

/// Checkbox for Some / None, then the inner editor. Enabling inserts the
/// inner type's default; without one the checkbox is disabled while None.
fn option_ui(ui: &mut egui::Ui, id: egui::Id, e: &mut dyn Enum, registry: &TypeRegistry) -> bool {
    let inner = e
        .get_represented_enum_info()
        .and_then(|info| info.variant("Some"))
        .and_then(|variant| match variant {
            VariantInfo::Tuple(t) => t.field_at(0).map(|f| f.type_id()),
            _ => None,
        });
    let default = inner.and_then(|t| registry.get_type_data::<ReflectDefault>(t));
    let mut on = e.variant_name() == "Some";
    let mut changed = false;
    ui.horizontal(|ui| {
        let toggle = ui
            .add_enabled(
                on || default.is_some(),
                egui::Checkbox::without_text(&mut on),
            )
            .on_disabled_hover_text("The value type has no default value");
        if toggle.changed() {
            let new = match (on, default) {
                (true, Some(default)) => {
                    let mut tuple = DynamicTuple::default();
                    tuple.insert_boxed(default.default().into_partial_reflect());
                    DynamicEnum::new("Some", DynamicVariant::Tuple(tuple))
                }
                _ => DynamicEnum::new("None", ()),
            };
            changed = e.try_apply(&new).is_ok();
        } else if let Some(field) = e.field_at_mut(0) {
            changed = value_ui(ui, id.with("some"), field, registry);
        }
    });
    changed
}

/// One row per element with a remove button (`id.with(index).with("remove")`),
/// then an add button (`id.with("add")`) that pushes the element type's
/// default; disabled when it has none.
fn list_ui(ui: &mut egui::Ui, id: egui::Id, list: &mut dyn List, registry: &TypeRegistry) -> bool {
    let default = list
        .get_represented_list_info()
        .and_then(|info| registry.get_type_data::<ReflectDefault>(info.item_ty().id()));
    let mut changed = false;
    let mut remove = None;
    ui.vertical(|ui| {
        for i in 0..list.len() {
            let item = id.with(i);
            ui.horizontal(|ui| {
                if let Some(element) = list.get_mut(i) {
                    changed |= value_ui(ui, item, element, registry);
                }
                if id_label(ui, item.with("remove"), "\u{d7}", false)
                    .on_hover_text("Remove")
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        }
        let add = ui
            .add_enabled_ui(default.is_some(), |ui| {
                id_label(ui, id.with("add"), "+", false)
            })
            .inner
            .on_hover_text("Add element")
            .on_disabled_hover_text("The element type has no default value");
        if let (true, Some(default)) = (add.clicked(), default) {
            list.push(default.default().into_partial_reflect());
            changed = true;
        }
    });
    if let Some(i) = remove {
        list.remove(i);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use melon_core::bevy_reflect::{self, Reflect, std_traits::ReflectDefault};
    use melon_core::glam::{EulerRot, Quat, Vec3};
    use melon_core::{QuatReflect, Vec3Reflect};

    #[derive(Reflect, Debug, Clone, PartialEq, Default)]
    #[reflect(Default)]
    enum Mode {
        #[default]
        Walk,
        Run,
        Fly,
    }

    #[derive(Reflect, Debug, Clone, PartialEq, Default)]
    #[reflect(Default)]
    struct Inner {
        weight: f32,
    }

    #[derive(Reflect, Debug, Clone, PartialEq)]
    struct Sample {
        speed: f32,
        on: bool,
        label: String,
        #[reflect(remote = Vec3Reflect)]
        pos: Vec3,
        #[reflect(remote = QuatReflect)]
        rot: Quat,
        mode: Mode,
        inner: Inner,
        limit: Option<f32>,
        list: Vec<f32>,
    }

    const FIELDS: [&str; 9] = [
        "speed", "on", "label", "pos", "rot", "mode", "inner", "limit", "list",
    ];

    fn sample() -> Sample {
        Sample {
            speed: 2.0,
            on: true,
            label: "hi".to_string(),
            pos: Vec3::new(1.0, 2.0, 3.0),
            rot: Quat::from_euler(
                EulerRot::YXZ,
                30f32.to_radians(),
                45f32.to_radians(),
                10f32.to_radians(),
            ),
            mode: Mode::Walk,
            inner: Inner { weight: 1.5 },
            limit: Some(4.0),
            list: vec![1.0],
        }
    }

    fn root() -> egui::Id {
        egui::Id::new("sample")
    }

    struct Harness {
        ctx: egui::Context,
        registry: TypeRegistry,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let mut registry = TypeRegistry::new();
            registry.register::<Sample>();
            Self {
                ctx: egui::Context::default(),
                registry,
                time: 0.0,
            }
        }

        fn frame(&mut self, value: &mut Sample, events: Vec<egui::Event>) -> bool {
            self.time += 0.05;
            let input = egui::RawInput {
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut changed = false;
            let registry = &self.registry;
            let _ = self.ctx.run_ui(input, |ui| {
                changed = reflect_ui(ui, root(), value, registry);
            });
            changed
        }

        fn center(&self, id: egui::Id) -> egui::Pos2 {
            self.ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("no widget with id {id:?}"))
                .rect
                .center()
        }

        /// Move, press, release, then one quiet frame. True if any frame changed the value.
        fn click(&mut self, value: &mut Sample, id: egui::Id) -> bool {
            let pos = self.center(id);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let mut changed = false;
            for events in [
                vec![egui::Event::PointerMoved(pos)],
                vec![button(true)],
                vec![button(false)],
                vec![],
            ] {
                changed |= self.frame(value, events);
            }
            changed
        }
    }

    #[test]
    fn every_field_kind_gets_an_editor() {
        let mut h = Harness::new();
        let mut value = sample();
        h.frame(&mut value, vec![]);
        for name in FIELDS {
            assert!(
                h.ctx.read_response(root().with(name)).is_some(),
                "no editor for {name}"
            );
        }
        assert!(
            h.ctx
                .read_response(root().with("inner").with("weight"))
                .is_some()
        );
    }

    #[test]
    fn editing_one_field_changes_only_that_field() {
        let mut h = Harness::new();
        let mut value = sample();
        h.frame(&mut value, vec![]);
        let start = h.center(root().with("speed"));
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let end = start + egui::vec2(40.0, 0.0);
        let mut changed = false;
        for events in [
            vec![egui::Event::PointerMoved(start)],
            vec![button(start, true)],
            vec![egui::Event::PointerMoved(start + egui::vec2(10.0, 0.0))],
            vec![egui::Event::PointerMoved(end)],
            vec![button(end, false)],
        ] {
            changed |= h.frame(&mut value, events);
        }
        assert!(changed);
        assert!(value.speed > 2.0, "speed {}", value.speed);
        let mut expected = sample();
        expected.speed = value.speed;
        assert_eq!(value, expected);
    }

    #[test]
    fn untouched_quat_editor_reports_no_change() {
        let mut h = Harness::new();
        let mut value = sample();
        let before = value.rot.to_array().map(f32::to_bits);
        for _ in 0..10 {
            assert!(!h.frame(&mut value, vec![]));
        }
        assert_eq!(value.rot.to_array().map(f32::to_bits), before);
    }

    #[test]
    fn enum_combo_sets_the_variant() {
        let mut h = Harness::new();
        let mut value = sample();
        h.frame(&mut value, vec![]);
        assert!(!h.click(&mut value, root().with("mode")));
        assert!(h.click(&mut value, root().with("mode").with("Run")));
        assert_eq!(value.mode, Mode::Run);
        let mut expected = sample();
        expected.mode = Mode::Run;
        assert_eq!(value, expected);
    }

    #[test]
    fn list_add_and_remove() {
        let mut h = Harness::new();
        let mut value = sample();
        h.frame(&mut value, vec![]);
        assert!(h.click(&mut value, root().with("list").with("add")));
        assert_eq!(value.list, [1.0, 0.0]);
        assert!(h.click(&mut value, root().with("list").with(0usize).with("remove")));
        assert_eq!(value.list, [0.0]);
    }
}
