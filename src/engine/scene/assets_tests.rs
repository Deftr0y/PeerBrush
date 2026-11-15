use super::*;

fn fixture() -> Document36 {
    serde_json::from_str(r#"{
        "id":"asset-document","name":"Assets","width":4,"height":2,"revision":7,
        "bit_depth":16,"icc_profile":"AAECAw==","srgb_tagged":false,
        "psd_global_mask":{"data":[1,2,3]},
        "layers":[{"id":"layer","name":"Layer","kind":"paint","parent":null,"clip_to":null,
            "visible":true,"locked":false,"opacity":1.0,"blend":"normal","x":0,"y":0,
            "pixels":{"id":1,"width":4,"height":2,"depth":16,"retained":null},
            "color":[233,84,32,255],"mask":{"enabled":true,"linked":true,"steps":[{
                "id":"step","kind":"paint","enabled":true,"weight":1.0,"value":1.0,"settings":{"id":8},
                "pixels":{"id":2,"width":4,"height":2,"depth":16,"retained":null}}]},
            "effects":[{"id":"effect","kind":"test","enabled":true,"weight":1.0,
                "settings":{"nested":{"value":1}},"clips":[]}],
            "psd_metadata":[{"key":"test","data":[8,9]}],
            "source":{"width":4,"height":2,"matrix":[1,0,0,1,0,0],
                "content":{"kind":"svg","svg":"<svg/>"}}}],
        "filters":[],"selection":[0,0,2,2],"selection_polygon":[[0,0],[2,0],[0,2]],
        "selection_coverage":{"mask":{"id":3,"width":2,"height":2,"depth":8,"retained":null},
            "bounds":[0,0,2,2],"origin":[0,0],"contours":[[[0,0],[1,0],[0,1]]]},
        "selection_previous":null,"read_only":false,"warnings":["PBA1:99"]
    }"#).unwrap()
}
fn bound(pool: &ResidentBudget) -> Admitted<Document36> {
    let charge = MetadataCharge::reserve(pool, 1024 * 1024, &|| Ok(())).unwrap();
    let mut fixture = fixture();
    fixture.psd_global_metadata = Some(crate::psd::global_metadata::tests::fixtures().into());
    fixture.psd_file_info = Some(crate::psd::file_info::fixture().into());
    Admitted::from_parts(Arc::new(fixture), charge)
        .rehome_unique(|metadata, _| metadata.bind_runtime(pool, &|| Ok(())))
        .unwrap()
}
#[test]
fn typed_projection_restores_every_asset_without_copying_its_payload() {
    let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let original = bound(&pool);
    let wire = serde_json::to_vec(&*original).unwrap();
    let retained = pool.used();
    let projected = original.project_assets(&pool, &|| Ok(())).unwrap();
    assert_eq!(projected.entries.len(), original.asset_count().unwrap());
    assert!(serde_json::to_vec(&*projected.header).unwrap().len() < 8192);
    let mut candidate: Document36 = serde_json::from_value((*projected.header).clone()).unwrap();
    candidate
        .restore_assets(|id, kind| {
            let entry = projected
                .entries
                .get(id as usize - 1)
                .ok_or("Missing asset")?;
            if entry.asset.kind() != kind {
                return Err("Wrong asset kind".into());
            }
            entry.asset.shared()
        })
        .unwrap();
    assert_eq!(serde_json::to_vec(&candidate).unwrap(), wire);
    assert!(candidate.layers[0].effects[0]
        .settings
        .same_allocation(&original.layers[0].effects[0].settings));
    assert!(candidate
        .icc_profile
        .as_ref()
        .unwrap()
        .same_allocation(original.icc_profile.as_ref().unwrap()));
    assert!(candidate
        .psd_global_metadata
        .as_ref()
        .unwrap()
        .same_allocation(original.psd_global_metadata.as_ref().unwrap()));
    assert!(candidate.psd_file_info.as_ref().unwrap()
        .same_allocation(original.psd_file_info.as_ref().unwrap()));
    drop(candidate);
    drop(projected);
    assert_eq!(pool.used(), retained);
    drop(original);
    assert_eq!(pool.used(), 0);
}
#[test]
fn projection_refusal_and_wrong_or_missing_restoration_keep_original_unchanged() {
    let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let original = bound(&pool);
    let before = serde_json::to_vec(&*original).unwrap();
    let retained = pool.used();
    let other = ResidentBudget::new(32 * 1024 * 1024).unwrap();
    assert!(original.project_assets(&other, &|| Ok(())).is_err());
    assert_eq!(other.used(), 0);
    assert!(original
        .project_assets(&pool, &|| Err("cancel".into()))
        .is_err());
    assert_eq!(pool.used(), retained);
    let projected = original.project_assets(&pool, &|| Ok(())).unwrap();
    let mut candidate: Document36 = serde_json::from_value((*projected.header).clone()).unwrap();
    assert!(candidate
        .restore_assets(|_, _| Err("missing".into()))
        .is_err());
    let mut candidate: Document36 = serde_json::from_value((*projected.header).clone()).unwrap();
    assert!(candidate
        .restore_assets(|_, _| Ok(MetadataAsset::Warnings(vec![].into())))
        .is_err());
    assert_eq!(serde_json::to_vec(&*original).unwrap(), before);
    drop(candidate);
    drop(projected);
    drop(original);
    assert_eq!(pool.used(), 0);
}

#[test]
fn projecting_32_mib_of_unchanged_settings_only_allocates_small_headers() {
    let pool = ResidentBudget::new(768 * 1024 * 1024).unwrap();
    let construction = MetadataCharge::reserve(&pool, 128 * 1024 * 1024, &|| Ok(())).unwrap();
    let mut value = fixture();
    value.layers[0].effects.clear();
    for index in 0..8 {
        value.layers[0].effects.push(Effect36 {
            id: format!("asset{index}"),
            kind: "test".into(),
            enabled: true,
            weight: 1.,
            settings: serde_json::json!({"data":"x".repeat(4 * 1024 * 1024 - 512)}).into(),
            clips: vec![],
        });
    }
    let original = Admitted::from_parts(Arc::new(value), construction)
        .rehome_unique(|value, _| value.bind_runtime(&pool, &|| Ok(())))
        .unwrap();
    let baseline = pool.used();
    assert!(baseline < 40 * 1024 * 1024);
    let projection = original.project_assets(&pool, &|| Ok(())).unwrap();
    assert!(pool.used() < baseline + 1024 * 1024);
    assert!(serde_json::to_vec(&*projection.header).unwrap().len() < 16 * 1024);
    let mut restored: Document36 = serde_json::from_value((*projection.header).clone()).unwrap();
    restored
        .restore_assets(|id, kind| {
            let entry = projection
                .entries
                .get(id as usize - 1)
                .ok_or("Missing asset")?;
            if entry.asset.kind() != kind {
                return Err("Wrong asset kind".into());
            }
            entry.asset.shared()
        })
        .unwrap();
    for index in 0..8 {
        assert!(restored.layers[0].effects[index]
            .settings
            .same_allocation(&original.layers[0].effects[index].settings));
        assert_eq!(
            restored.layers[0].effects[index].settings["data"]
                .as_str()
                .unwrap()
                .len(),
            4 * 1024 * 1024 - 512
        );
    }
    drop(restored);
    drop(projection);
    assert_eq!(pool.used(), baseline);
    drop(original);
    assert_eq!(pool.used(), 0);
}
