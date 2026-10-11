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
                    if r.changed() {let epsilon=(next-previous)*1e-6;stop["position"]=json!((f64::from(n)/100.).clamp(previous+epsilon,next-epsilon));changed=Some(r);}
                    let r=ui.add_enabled(index>0&&index+1<count,egui::Button::new("×")).on_hover_text("Remove this stop");
                    if r.clicked(){remove=Some(index);changed=Some(r);}
                });
                if alpha {
                    ui.horizontal(|ui| {
                        controls::label(ui,"Opacity");let mut n=stop["opacity"].as_f64().unwrap() as f32*100.;
                        let r=controls::range(ui,(key,index,"opacity"),&mut n,0.0..=100.,160.,"%",1,false);
                        if r.changed(){stop["opacity"]=json!(n/100.);changed=Some(r);}
                    });
                } else {
                    ui.horizontal(|ui| {
                        for (c,label) in ["R","G","B"].into_iter().enumerate() {
                            controls::label(ui,label);let mut n=stop["color"][c].as_f64().unwrap() as f32*100.;
                            let r=controls::range(ui,(key,index,c),&mut n,0.0..=100.,60.,"%",1,false);
                            if r.changed(){stop["color"][c]=json!(n/100.);changed=Some(r);}
                        }
                    });
                }
                if index+1<count {
                    ui.horizontal(|ui| {
                        controls::label(ui,"Midpoint");let mut n=stop["midpoint"].as_f64().unwrap_or(0.5) as f32*100.;
                        let r=controls::range(ui,(key,index,"midpoint"),&mut n,1.0..=99.,160.,"%",1,false);
                        if r.changed(){stop["midpoint"]=json!(n/100.);changed=Some(r);}
                    });
                }
            }
            if let Some(index)=remove {stops.remove(index);}
        });
    }
    changed
}
