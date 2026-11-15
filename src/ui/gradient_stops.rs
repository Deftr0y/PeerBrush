use super::*;

pub(super) fn controls(ui: &mut egui::Ui, value: &mut Value) -> Option<egui::Response> {
    edit(ui, value, false)
}

pub(super) fn map_controls(ui: &mut egui::Ui, value: &mut Value) -> Option<egui::Response> {
    edit(ui, value, true)
}

fn edit(ui: &mut egui::Ui, value: &mut Value, map: bool) -> Option<egui::Response> {
    let mut changed = ui.horizontal(|ui| {
        let mut reversed = value["reverse"] == true;
        let reverse = icons::toggle(ui, Icon::Swap, &mut reversed, "Reverse gradient")
            .on_hover_text("Reverse the order of stops across the gradient");
        if reverse.changed() {
            value["reverse"] = json!(reversed);
        }
        ramp(ui, value, map).or_else(|| reverse.changed().then_some(reverse))
    }).inner;
    for (key, title, alpha) in [
        ("color_stops", "Color stops", false),
        ("alpha_stops", "Opacity stops", true),
    ] {
        if map && alpha { continue; }
        let count = value[key].as_array().map_or(0, Vec::len);
        egui::CollapsingHeader::new(title).id_salt(key).default_open(count>0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let limit = if map { crate::effects::gradient_map::MAX_STOPS } else { 16 };
                let add = ui.add_enabled(count < limit,egui::Button::new(if alpha {"Add opacity stop"}else{"Add color stop"}));
                if add.clicked() {
                    let mut stops = value[key].as_array().cloned().unwrap_or_default();
                    let normalized = (!map).then(|| crate::effects::normalized("gradient_overlay", value).unwrap());
                    if stops.is_empty() {
                        let Some(normalized) = &normalized else { return; };
                        stops = if alpha { vec![json!({"position":0.,"opacity":1.}),json!({"position":1.,"opacity":1.})] }
                            else { vec![json!({"position":0.,"color":normalized["color"]}),json!({"position":1.,"color":normalized["color2"]})] };
                    }
                    let index = stops.windows(2).enumerate().max_by(|(_,a),(_,b)| {
                        let gap=|p:&[Value]|p[1]["position"].as_f64().unwrap()-p[0]["position"].as_f64().unwrap();
                        gap(a).total_cmp(&gap(b))
                    }).map(|(i,_)|i).unwrap();
                    let left = stops[index]["position"].as_f64().unwrap();
                    let right = stops[index+1]["position"].as_f64().unwrap();
                    let mut position = (left + right) / 2.;
                    let (rgb, opacity) = if map {
                        position = (position * 4096.).round() / 4096.;
                        if position <= left || position >= right { return; }
                        let Ok(settings) = crate::effects::gradient_map::Settings::new(value) else { return; };
                        (settings.prepare().color(position), 1.)
                    } else {
                        let (rgb, opacity) = crate::effects::gradient::sample(normalized.as_ref().unwrap(), position);
                        (rgb.map(|value| f64::from(value) / 65535.), opacity)
                    };
                    stops.insert(index+1,if alpha {json!({"position":position,"opacity":opacity,"midpoint":0.5})}
                        else {json!({"position":position,"color":rgb,"midpoint":0.5})});
                    value[key]=json!(stops);changed=Some(add);
                }
                let reset=ui.add_enabled(if map { count > 2 } else { count > 0 },egui::Button::new(if alpha {"Opaque"}else{"Two colors"}));
                if reset.clicked() {
                    if map {
                        let stops = value[key].as_array().unwrap();
                        value[key] = json!([stops[0],stops[count-1]]);
                    } else {
                        value.as_object_mut().unwrap().remove(key);
                    }
                    changed=Some(reset);
                }
            });
            let before_colors = value.clone();
            let mut alpha_edits = vec![];
            let Some(stops)=value[key].as_array_mut() else{return};
            let count=stops.len();let mut remove=None;
            for index in 0..count {
                let previous=if index>0 {stops[index-1]["position"].as_f64().unwrap()}else{0.};
                let next=if index+1<count {stops[index+1]["position"].as_f64().unwrap()}else{1.};
                let stop=&mut stops[index];
                ui.horizontal(|ui| {
                    controls::label(ui,&format!("Stop {}",index+1));
                    let mut n=stop["position"].as_f64().unwrap() as f32*100.;
                    let interior = index>0&&index+1<count;
                    let r=ui.add_enabled_ui(map || interior,|ui|controls::range(ui,(key,index,"position"),&mut n,0.0..=100.,80.,"%",1,false)).inner;
                    if r.changed() || r.double_clicked() {
                        if r.changed() {
                            stop["position"] = json!(stop_position(
                                f64::from(n) / 100., previous, next, index, count, map,
                            ));
                        }
                        changed=Some(r);
                    }
                    let r=ui.add_enabled(if map { count > 2 } else { interior },egui::Button::new("×")).on_hover_text("Remove this stop");
                    if r.clicked(){remove=Some(index);changed=Some(r);}
                });
                if alpha {
                    ui.horizontal(|ui| {
                        controls::label(ui,"Opacity");let mut n=stop["opacity"].as_f64().unwrap() as f32*100.;
                        let r=controls::range(ui,(key,index,"opacity"),&mut n,0.0..=100.,160.,"%",1,false);
                        if r.changed() || r.double_clicked(){if r.changed(){stop["opacity"]=json!(n/100.);}changed=Some(r);}
                    });
                } else {
                    let position = stop["position"].as_f64().unwrap();
                    let mut rgb: [f64;3] = std::array::from_fn(|c| stop["color"][c].as_f64().unwrap());
                    let old_alpha = if map { 1. } else { crate::effects::gradient::opacity_at(&before_colors,position) };
                    let mut opacity = old_alpha;
                    ui.horizontal(|ui| {
                        controls::label(ui,"Color");
                        if let Some(response) = effect_color::control(ui,(key,index,"color"),&format!("Stop {}",index+1),&mut rgb,&mut opacity,(!map).then_some("Stop opacity"),true) {
                            stop["color"] = json!(rgb);
                            if opacity != old_alpha { alpha_edits.push((position,opacity)); }
                            changed = Some(response);
                        }
                    });
                }
                if if map { index > 0 } else { index+1<count } {
                    ui.horizontal(|ui| {
                        controls::label(ui,"Midpoint");let mut n=stop["midpoint"].as_f64().unwrap_or(0.5) as f32*100.;
                        let range = if map { 0.0..=100. } else { 1.0..=99. };
                        let r=controls::range(ui,(key,index,"midpoint"),&mut n,range,160.,"%",usize::from(!map),false);
                        if r.changed() || r.double_clicked(){
                            if r.changed() {
                                stop["midpoint"] = if map { json!(f64::from(n.round()) / 100.) }
                                    else { json!((f64::from(n)/100.).clamp(0.01,0.99)) };
                            }
                            changed=Some(r);
                        }
                    });
                }
            }
            if let Some(index)=remove {stops.remove(index);}
            for (position,opacity) in alpha_edits {
                if let Err(error) = crate::effects::gradient::set_opacity_at(value,position,opacity) {
                    *value = before_colors.clone();
                    if let Some(response) = &changed {
                        ui.ctx().data_mut(|data| data.insert_temp(response.id.with("cancel parameter"),true));
                    }
                    ui.label(RichText::new(error).size(11.).color(ACCENT));
                    break;
                }
            }
        });
    }
    changed
}

fn stop_position(position: f64, previous: f64, next: f64, index: usize, count: usize, map: bool) -> f64 {
    if map {
        let low = if index == 0 { 0. } else { (previous * 4096.).round() + 1. };
        let high = if index + 1 == count { 4096. } else { (next * 4096.).round() - 1. };
        (position * 4096.).round().clamp(low, high) / 4096.
    } else {
        let epsilon = (next - previous) * 1e-6;
        position.clamp(
            (previous + epsilon).max(previous.next_up()),
            (next - epsilon).min(next.next_down()),
        )
    }
}

fn ramp(ui: &mut egui::Ui, value: &mut Value, map: bool) -> Option<egui::Response> {
    let has_alpha = !map && value["alpha_stops"].as_array().is_some_and(|stops| !stops.is_empty());
    let (outer, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().min(540.), if has_alpha { 57. } else { 43. }),
        egui::Sense::hover(),
    );
    let strip = Rect::from_min_max(
        outer.left_top() + Vec2::new(9., if has_alpha { 16. } else { 2. }),
        outer.right_bottom() - Vec2::new(9., 17.),
    );
    if strip.width() <= 1. {
        return None;
    }
    let reverse = value["reverse"] == true;
    let orient = |position: f64| if reverse { 1. - position } else { position };
    let x = |position: f64| strip.left() + strip.width() * orient(position) as f32;
    let mut changed = None;
    for (key, label, alpha) in [("color_stops", "Color", false), ("alpha_stops", "Opacity", true)] {
        if alpha && !has_alpha {
            continue;
        }
        let Some(stops) = value[key].as_array_mut() else { continue };
        let count = stops.len();
        let y = if alpha { strip.top() - 6. } else { strip.bottom() + 6. };
        for index in 0..count {
            if !map && (index == 0 || index + 1 == count) {
                continue;
            }
            let position = stops[index]["position"].as_f64().unwrap();
            let center = egui::pos2(x(position), y);
            let (mut response, point) = controls::point_handle(
                ui, ui.make_persistent_id(("gradient stop", key, index)), center,
                &format!("Drag {label} stop {}", index + 1),
            );
            if let Some(point) = point.filter(|point| (point.x - center.x).abs() >= 0.001) {
                let previous = if index == 0 { 0. } else { stops[index - 1]["position"].as_f64().unwrap() };
                let next = if index + 1 == count { 1. } else { stops[index + 1]["position"].as_f64().unwrap() };
                let updated = stop_position(
                    orient(f64::from((point.x - strip.left()) / strip.width())),
                    previous, next, index, count, map,
                );
                if updated != position {
                    stops[index]["position"] = json!(updated);
                    response.mark_changed();
                    changed = Some(response);
                }
            }
        }
    }
    let painter = ui.painter();
    view::paint_checker(painter, [strip.left_top(), strip.right_top(), strip.right_bottom(), strip.left_bottom()], view::checker(ui.ctx()).id());
    if map {
        if let Ok(settings) = crate::effects::gradient_map::Settings::new(value) {
            let prepared = settings.prepare();
            paint_ramp(painter, strip, |t| {
                let [r, g, b] = prepared.lookup(t, 255).map(|value| value as u8);
                Color32::from_rgb(r, g, b)
            });
        }
    } else if let Ok(settings) = crate::effects::normalized("gradient_overlay", value) {
        paint_ramp(painter, strip, |t| {
            let (rgb, opacity) = crate::effects::gradient::sample(&settings, orient(t));
            let [r, g, b] = rgb.map(crate::raster::project16);
            Color32::from_rgba_unmultiplied(r, g, b, (opacity * 255.).round() as u8)
        });
    }
    for (key, alpha) in [("color_stops", false), ("alpha_stops", true)] {
        if alpha && !has_alpha {
            continue;
        }
        let Some(stops) = value[key].as_array() else { continue };
        for stop in stops {
            let center = x(stop["position"].as_f64().unwrap());
            let y = if alpha { strip.top() } else { strip.bottom() };
            let direction = if alpha { -1. } else { 1. };
            let color = if alpha {
                Color32::from_gray((stop["opacity"].as_f64().unwrap() * 255.).round() as u8)
            } else {
                let [r, g, b] = std::array::from_fn(|i| (stop["color"][i].as_f64().unwrap() * 255.).round() as u8);
                Color32::from_rgb(r, g, b)
            };
            painter.add(egui::Shape::convex_polygon(
                vec![egui::pos2(center, y), egui::pos2(center - 5., y + direction * 6.),
                    egui::pos2(center - 5., y + direction * 11.), egui::pos2(center + 5., y + direction * 11.),
                    egui::pos2(center + 5., y + direction * 6.)],
                color, Stroke::new(1., ui.visuals().text_color()),
            ));
        }
    }
    changed
}

pub(super) fn paint_ramp(painter: &egui::Painter, rect: Rect, sample: impl Fn(f64) -> Color32) {
    let steps = rect.width().ceil().clamp(2., 512.) as usize;
    let mut mesh = egui::Mesh::default();
    for i in 0..=steps {
        let fraction = i as f64 / steps as f64;
        let x = rect.left() + rect.width() * fraction as f32;
        let color = sample(fraction);
        mesh.colored_vertex(egui::pos2(x, rect.top()), color);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), color);
        if i > 0 {
            let index = i as u32 * 2;
            mesh.add_triangle(index - 2, index, index + 1);
            mesh.add_triangle(index - 2, index + 1, index - 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gradient_midpoint_limit_drags_preview_native_pixels_and_commit_once() {
        for depth in [8, 16] {
            for (delta, endpoint) in [(-300., 0.01), (300., 0.99)] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                for y in 0..24 {
                    for x in 0..32 {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [12347, 23459, 34571, 65535]);
                    }
                }
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":{"opacity":1.,"angle":0.,"color_stops":[{"position":0.,"color":[0.,0.,0.],"midpoint":0.35},{"position":1.,"color":[1.,1.,1.]}]}})], None, None, "Gradient").unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                let before = engine.resident_state().expect("resident fixture").document.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                let shared = Arc::new(std::sync::Mutex::new(engine));
                let connection = crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                };
                let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
                app.selected = layer.clone();
                app.selection_layers = [layer.clone()].into_iter().collect();
                app.effect_selected = Some((layer, false, effect));
                let native = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview_native8(None, 32, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 1100.),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let start = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.job.text == "35.0%" => {
                            Some(Rect::from_min_size(t.pos, t.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Compact midpoint control");
                let end = start + Vec2::new(delta, 0.);
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                };
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                let commands = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Valid midpoint gesture")
                    .commands
                    .clone();
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_eq!(
                    draft.layers[0].effects[0].settings["color_stops"][0]["midpoint"],
                    json!(endpoint)
                );
                assert_ne!(native(&draft), native(&before));
                {
                    let engine = shared.lock().unwrap();
                    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(engine.resident_state().expect("resident fixture").document.revision, before.revision);
                }
                draw(&mut app, vec![button(end, false)]);
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(native(&engine.resident_state().expect("resident fixture").document), native(&draft));
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                engine.undo("human").unwrap();
                assert_eq!(native(&engine.resident_state().expect("resident fixture").document), native(&before));
            }
        }
    }
}
