use super::*;

pub(super) fn controls(ui: &mut egui::Ui, value: &mut Value) -> Option<egui::Response> {
    let mut changed = None;
    for (key, title, alpha) in [
        ("color_stops", "Color stops", false),
        ("alpha_stops", "Opacity stops", true),
    ] {
        let count = value[key].as_array().map_or(0, Vec::len);
        egui::CollapsingHeader::new(title).id_salt(key).default_open(count>0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let add = ui.add_enabled(count < 16,egui::Button::new(if alpha {"Add opacity stop"}else{"Add color stop"}));
                if add.clicked() {
                    let normalized = crate::effects::normalized("gradient_overlay", value).unwrap();
                    let mut stops = value[key].as_array().cloned().unwrap_or_default();
                    if stops.is_empty() {
                        stops = if alpha { vec![json!({"position":0.,"opacity":1.}),json!({"position":1.,"opacity":1.})] }
                            else { vec![json!({"position":0.,"color":normalized["color"]}),json!({"position":1.,"color":normalized["color2"]})] };
                    }
                    let index = stops.windows(2).enumerate().max_by(|(_,a),(_,b)| {
                        let gap=|p:&[Value]|p[1]["position"].as_f64().unwrap()-p[0]["position"].as_f64().unwrap();
                        gap(a).total_cmp(&gap(b))
                    }).map(|(i,_)|i).unwrap();
                    let position=(stops[index]["position"].as_f64().unwrap()+stops[index+1]["position"].as_f64().unwrap())/2.;
                    let (rgb,opacity)=crate::effects::gradient::sample(&normalized,position);
                    stops.insert(index+1,if alpha {json!({"position":position,"opacity":opacity,"midpoint":0.5})}
                        else {json!({"position":position,"color":rgb.map(|n|f64::from(n)/65535.),"midpoint":0.5})});
                    value[key]=json!(stops);changed=Some(add);
                }
                let reset=ui.add_enabled(count>0,egui::Button::new(if alpha {"Opaque"}else{"Two colors"}));
                if reset.clicked() {value.as_object_mut().unwrap().remove(key);changed=Some(reset);}
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
                    let r=ui.add_enabled_ui(index>0&&index+1<count,|ui|controls::range(ui,(key,index,"position"),&mut n,0.0..=100.,80.,"%",1,false)).inner;
                    if r.changed() || r.double_clicked() {if r.changed() {let epsilon=(next-previous)*1e-6;stop["position"]=json!((f64::from(n)/100.).clamp(previous+epsilon,next-epsilon));}changed=Some(r);}
                    let r=ui.add_enabled(index>0&&index+1<count,egui::Button::new("×")).on_hover_text("Remove this stop");
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
                    let old_alpha = crate::effects::gradient::opacity_at(&before_colors,position);
                    let mut opacity = old_alpha;
                    ui.horizontal(|ui| {
                        controls::label(ui,"Color");
                        if let Some(response) = effect_color::control(ui,(key,index,"color"),&format!("Stop {}",index+1),&mut rgb,&mut opacity,"Stop opacity") {
                            stop["color"] = json!(rgb);
                            if opacity != old_alpha { alpha_edits.push((position,opacity)); }
                            changed = Some(response);
                        }
                    });
                }
                if index+1<count {
                    ui.horizontal(|ui| {
                        controls::label(ui,"Midpoint");let mut n=stop["midpoint"].as_f64().unwrap_or(0.5) as f32*100.;
                        let r=controls::range(ui,(key,index,"midpoint"),&mut n,1.0..=99.,160.,"%",1,false);
                        if r.changed() || r.double_clicked(){if r.changed(){stop["midpoint"]=json!((f64::from(n)/100.).clamp(0.01,0.99));}changed=Some(r);}
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gradient_midpoint_limit_drags_preview_native_pixels_and_commit_once() {
        for depth in [8, 16] {
            for (delta, endpoint) in [(-300., 0.01), (300., 0.99)] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                for y in 0..24 {
                    for x in 0..32 {
                        engine.doc.layers[0]
                            .pixels
                            .set16(x, y, [12347, 23459, 34571, 65535]);
                    }
                }
                let layer = engine.doc.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":{"opacity":1.,"angle":0.,"color_stops":[{"position":0.,"color":[0.,0.,0.],"midpoint":0.35},{"position":1.,"color":[1.,1.,1.]}]}})], None, None, "Gradient").unwrap();
                engine.undo.clear();
                let before = engine.doc.clone();
                let effect = engine.doc.layers[0].effects[0].id.clone();
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
                    assert!(engine.undo.is_empty());
                    assert_eq!(engine.doc.revision, before.revision);
                }
                draw(&mut app, vec![button(end, false)]);
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(native(&engine.doc), native(&draft));
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                engine.undo("human").unwrap();
                assert_eq!(native(&engine.doc), native(&before));
            }
        }
    }
}
