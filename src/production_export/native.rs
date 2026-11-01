//! Native export planning over the actual immutable scene. No resident document
//! or full image is constructed; alpha bounds use charged complete stripes.
use super::*;
use crate::{engine::scene::SceneSnapshot,large_document::ScratchBytes,psd::memory36::{Admitted,MetadataCharge}};
type Result<T>=std::result::Result<T,String>;
pub(crate) enum Capture{Resident(Arc<Document>),Native(Arc<SceneSnapshot>)}
pub(crate) enum Planned{Resident(Plan),Native(Admitted<Plan>)}
impl std::ops::Deref for Planned{
    type Target=Plan;
    fn deref(&self)->&Plan{match self{Self::Resident(plan)=>plan,Self::Native(plan)=>plan}}
}
impl Capture{
    pub(crate) fn capture(shared:&crate::server::Shared,request:&serde_json::Value)->Result<Self>{
        let engine=shared.try_lock().map_err(|_|"Project is busy; export capture deferred")?;
        engine.ensure_open()?;crate::workspace::validate_target(&engine,request)?;
        let id=request["document_id"].as_str().ok_or("Export requires the observed document_id")?;
        let revision=request["expected_revision"].as_u64().ok_or("Export requires the observed expected_revision")?;
        crate::workspace::guard(&engine,id,revision)?;
        match engine.doc_snapshot(){
            crate::engine::view::DocumentSnapshot::Resident(doc)=>Ok(Self::Resident(doc)),
            crate::engine::view::DocumentSnapshot::Native(_)=>Ok(Self::Native(Arc::new(engine.capture_scene(None)?.into_native()?))),
        }
    }
    pub(crate) fn id(&self)->&str{match self{Self::Resident(doc)=>&doc.id,Self::Native(scene)=>&scene.identity().document_id}}
    pub(crate) fn revision(&self)->u64{match self{Self::Resident(doc)=>doc.revision,Self::Native(scene)=>scene.identity().revision}}
    pub(crate) fn validate(&self,shared:&crate::server::Shared)->Result<()>{
        let engine=shared.try_lock().map_err(|_|"Project is busy during export source validation")?;
        engine.ensure_open()?;crate::workspace::guard(&engine,self.id(),self.revision())?;
        if let Self::Native(scene)=self{
            if engine.project_id!=scene.identity().project_id||!matches!(crate::engine::view::current(&engine),crate::engine::view::DocumentView::Native(doc)if doc.key()==scene.key()){
                return Err("Native export source generation changed".into());
            }
        }Ok(())
    }
    pub(crate) fn plan(&self,shared:&crate::server::Shared,targets:&[String],settings:&Settings,control:&Control)->Result<Planned>{
        control.check()?;self.validate(shared)?;
        let plan=match self{
            Self::Resident(doc)=>Planned::Resident(super::plan(doc,targets,settings,control)?),
            Self::Native(scene)=>{
                let session=crate::workspace::native_dispatch::begin_read_scene(shared,scene)?;
                let active=||{control.check()?;session.check_scene(scene)};
                Planned::Native(plan(scene,targets,settings,&active)?)
            },
        };
        self.validate(shared)?;control.check()?;Ok(plan)
    }
}

pub(crate) fn plan(scene:&SceneSnapshot,targets:&[String],settings:&Settings,check:&dyn Fn()->Result<()>)->Result<Admitted<Plan>>{
    settings.validate()?;check()?;
    let doc=scene.metadata();
    dimensions(doc.width(),doc.height())?;
    if ![8,16].contains(&doc.depth()){return Err("Exports require native8/16-bit RGB channels".into());}
    if targets.len()>1024{return Err("Export selection exceeds1024 layer references".into());}
    // Charge both the planning index and all possible output label copies before
    // allocating collections. Names remain exact; no hidden truncation of IDs.
    let labels=doc.layers().try_fold(0usize,|sum,layer|sum.checked_add(layer.id().len()).and_then(|n|n.checked_add(layer.name().len())).ok_or("Export labels overflow"))?;
    let bytes=labels.checked_mul(settings.variants.len()).and_then(|n|n.checked_add(scene.identity().document_id.len())).and_then(|n|n.checked_add(1024*1024)).ok_or("Native export plan footprint overflow")?;
    let owner=MetadataCharge::reserve(scene.budget(),bytes,check)?;
    let selected:HashSet<&str>=targets.iter().map(String::as_str).collect();
    if selected.len()!=targets.len()||selected.iter().any(|id|!doc.layers().any(|layer|layer.id()==*id)){return Err("Choose unique existing export layers/folders".into());}
    let mut roots=Vec::new();roots.try_reserve_exact(targets.len()).map_err(|_|"Native export root allocation refused")?;
    for layer in doc.layers(){
        check()?;if !selected.contains(layer.id()){continue;}
        let mut parent=layer.parent();let mut contained=false;
        for _ in 0..=16{
            let Some(id)=parent else{break};if selected.contains(id){contained=true;break;}
            parent=doc.layers().find(|layer|layer.id()==id).ok_or("Export hierarchy parent is missing")?.parent();
        }
        if parent.is_some()&&!contained{return Err("Export hierarchy nesting exceeds admission".into());}
        if !contained{roots.push(Some(layer.id()));}
    }
    if roots.is_empty(){roots.push(None);}
    if roots.len().checked_mul(settings.variants.len()).is_none_or(|count|count>MAX_OUTPUTS){return Err("Export at most64 outputs per batch".into());}
    let mut outputs=Vec::new();outputs.try_reserve_exact(roots.len()*settings.variants.len()).map_err(|_|"Native export output allocation refused")?;
    let mut names=HashSet::new();
    let document_name=safe_name(std::path::Path::new(doc.name()).file_stem().and_then(|name|name.to_str()).unwrap_or(doc.name()));
    for target in &roots{
        check()?;
        // This validates adjustment/clipping/ancestor isolation identically to
        // the regional renderer, and never silently excludes a provider.
        let rows=super::native_rows::Rows::for_target(scene,*target)?;
        let target_name=target.and_then(|id|doc.layers().find(|layer|layer.id()==id)).map(|layer|layer.name()).unwrap_or("composite");
        let mut rect=if settings.crop==Crop::Alpha{bounds(&rows,*target,check)?}else{[0,0,doc.width() as i32,doc.height() as i32]};
        let pad=settings.padding as i32;rect=[rect[0]-pad,rect[1]-pad,rect[2]+pad,rect[3]+pad];
        let(rw,rh)=((rect[2]-rect[0])as u32,(rect[3]-rect[1])as u32);
        for variant in &settings.variants{
            let width=(f64::from(rw)*variant.scale).round().max(1.)as u32;
            let height=(f64::from(rh)*variant.scale).round().max(1.)as u32;dimensions(width,height)?;
            let base=safe_name(&settings.template.replace("{document}",&document_name).replace("{layer}",&safe_name(target_name)).replace("{variant}",&safe_name(&variant.name)));
            let mut filename=format!("{base}.{}",settings.format.extension());let mut number=2;
            while !names.insert(filename.to_lowercase()){
                if settings.duplicate_names==DuplicateNames::Error{return Err("Export names collide; change the template or choose numbered duplicates".into());}
                filename=format!("{base}_{number}.{}",settings.format.extension());number+=1;
            }
            outputs.push(Output{target:target.map(str::to_owned),target_name:target_name.to_owned(),variant:variant.name.clone(),filename,document_rect:rect,width,height,format:settings.format,native_source_depth:doc.depth(),bit_depth:settings.format.bit_depth(doc.depth()),scale_xy:[f64::from(width)/f64::from(rw),f64::from(height)/f64::from(rh)]});
        }
    }
    check()?;
    Ok(Admitted::from_parts(Arc::new(Plan{document_id:scene.identity().document_id.clone(),revision:scene.identity().revision,outputs,isolated:!roots.contains(&None),profile_retained:doc.profile().is_some()||doc.srgb_tagged()}),owner))
}
fn dimensions(width:u32,height:u32)->Result<()>{
    if width==0||height==0||width>16000||height>16000{return Err("Native export dimensions exceed admission".into());}Ok(())
}
fn bounds(rows:&super::native_rows::Rows<'_>,target:Option<&str>,check:&dyn Fn()->Result<()>)->Result<[i32;4]>{
    let scene=rows.scene();let doc=scene.metadata();let depth=doc.depth();let stride=usize::from(depth/2);
    let output=Output{target:target.map(str::to_owned),target_name:String::new(),variant:String::new(),filename:String::new(),document_rect:[0,0,doc.width()as i32,doc.height()as i32],width:doc.width(),height:doc.height(),format:Format::Png,native_source_depth:depth,bit_depth:depth,scale_xy:[1.,1.]};
    let count=super::native_png::stripe_rows(doc.width(),doc.height());
    let mut buffer=ScratchBytes::new(scene.budget(),doc.width()as usize*count as usize*stride,128*1024*8).map_err(|error|error.to_string())?;
    let mut bounds=[doc.width()as i32,doc.height()as i32,0,0];let mut first=0;
    while first<doc.height(){
        check()?;let count=count.min(doc.height()-first);let bytes=&mut buffer[..doc.width()as usize*count as usize*stride];
        rows.fill(&output,depth,Resampling::Nearest,first,count,bytes,check)?;
        for y in 0..count{check()?;for x in 0..doc.width(){let at=(y as usize*doc.width()as usize+x as usize)*stride;
            let opaque=if depth==8{bytes[at+3]!=0}else{bytes[at+6]!=0||bytes[at+7]!=0};
            if opaque{bounds[0]=bounds[0].min(x as i32);bounds[1]=bounds[1].min((first+y)as i32);bounds[2]=bounds[2].max(x as i32+1);bounds[3]=bounds[3].max((first+y)as i32+1);}
        }}first+=count;
    }
    if bounds[0]>=bounds[2]||bounds[1]>=bounds[3]{return Err("Target is transparent; choose canvas bounds to export an empty image".into());}Ok(bounds)
}
