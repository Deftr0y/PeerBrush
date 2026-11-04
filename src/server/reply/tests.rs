use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use std::io::{Read, Write};

fn bytes(pool: &ResidentBudget, source: &[u8]) -> Segments {
    let mut check = || Ok(());
    let mut output = SegmentWriter::new(pool, source.len().max(1), &mut check).unwrap();
    for part in source.chunks(701) {
        output.write_all(part).unwrap();
    }
    output.finish().unwrap()
}
fn reply(pool: &ResidentBudget, source: &[u8]) -> OwnedReply {
    let value = json!({"revision":17,"state":{"name":"Native 原😀\n\"\\"},"images":[{
        "mime_type":"image/png","width":13,"height":7,"document_rect":[-2,3,11,10],"revision":17,"data":null}]});
    let mut admission = ReplyAdmission::new(true);
    admission.admit_projection(pool, &value).unwrap();
    let data = bytes(pool, source);
    admission.image(0, data).unwrap();
    admission.finish(value).unwrap()
}
fn read(bytes: Segments) -> Vec<u8> {
    let mut result = Vec::new();
    bytes.reader().read_to_end(&mut result).unwrap();
    result
}

#[test]
fn private_rpc_prefix_binds_id_before_any_native_payload() {
    let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let source = [19, 82, 201];
    let reply = reply(&pool, &source)
        .tool_result()
        .unwrap()
        .rpc(&json!("request-🙂"))
        .unwrap()
        .ordered_rpc()
        .unwrap();
    let wire = read(reply.serialized(&mut || Ok(())).unwrap());
    let prefix = "{\"id\":\"request-🙂\",\"jsonrpc\":\"2.0\",";
    assert!(wire.starts_with(prefix.as_bytes()));
    let value: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        value["result"]["content"][1]["data"],
        STANDARD.encode(source)
    );
    assert_eq!(pool.used(), 0);
    assert!(OwnedReply::plain(json!({"jsonrpc":"2.0"}))
        .ordered_rpc()
        .is_err());
}

#[test]
fn segmented_base64_preserves_every_boundary_and_exact_wire_coordinates() {
    for length in [
        0,
        1,
        2,
        3,
        1024 * 1024 - 1,
        1024 * 1024 + 1,
        1024 * 1024 + 2,
        2 * 1024 * 1024 + 7,
    ] {
        let source: Vec<_> = (0..length).map(|n| ((n * 73 + 19) % 251) as u8).collect();
        let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let reply = reply(&pool, &source);
        let wire = reply.serialized(&mut || Ok(())).unwrap();
        assert_eq!(pool.used(), wire.retained_bytes());
        let wire = read(wire);
        let value: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(value["images"][0]["data"], STANDARD.encode(&source));
        assert_eq!(value["images"][0]["document_rect"], json!([-2, 3, 11, 10]));
        assert_eq!(value["revision"], 17);
        assert_eq!(value["state"]["name"], "Native 原😀\n\"\\");
        assert_eq!(pool.used(), 0);
    }
}
#[test]
fn mcp_moves_native_images_and_streams_the_exact_unicode_metadata_text() {
    let pool = ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let source = [0, 1, 127, 128, 254, 255, 3];
    let reply = reply(&pool, &source);
    let mut metadata = reply.value.clone();
    let mut image = metadata
        .as_object_mut()
        .unwrap()
        .remove("images")
        .unwrap()
        .as_array()
        .unwrap()[0]
        .clone();
    image.as_object_mut().unwrap().remove("data");
    metadata["views"] = json!([image]);
    let reply = reply
        .tool_result()
        .unwrap()
        .envelope(
            "result",
            serde_json::from_value(json!({"jsonrpc":"2.0","id":"client-原"})).unwrap(),
        )
        .unwrap();
    let value: Value =
        serde_json::from_slice(&read(reply.serialized(&mut || Ok(())).unwrap())).unwrap();
    assert_eq!(
        value["result"]["content"][0]["text"],
        serde_json::to_string(&metadata).unwrap()
    );
    assert_eq!(
        value["result"]["content"][1]["data"],
        STANDARD.encode(source)
    );
    assert_eq!(value["result"]["content"][1]["mimeType"], "image/png");
    assert_eq!(value["id"], "client-原");
    assert_eq!(pool.used(), 0);
}
#[test]
fn slots_are_nominal_and_cannot_be_overwritten_omitted_or_returned_as_plain_values() {
    let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    assert!(reply(&pool, &[1, 2, 3]).into_plain().is_err());
    assert_eq!(pool.used(), 0);
    let mut item = reply(&pool, &[1, 2, 3]);
    item.update(|value| {
        value["images"][0]["data"] = json!("not a slot");
    });
    assert!(item.serialized(&mut || Ok(())).is_err());
    assert_eq!(pool.used(), 0);
    let mut item = reply(&pool, &[1, 2, 3]);
    item.update(|value| {
        value.as_object_mut().unwrap().remove("images");
    });
    assert!(item.serialized(&mut || Ok(())).is_err());
    assert_eq!(pool.used(), 0);
    let mut admission = ReplyAdmission::new(false);
    assert!(admission.admit(&pool, 1).is_err());
    assert_eq!(pool.used(), 0);
    assert_eq!(
        OwnedReply::plain(json!({"data":"ordinary metadata"}))
            .into_plain()
            .unwrap(),
        json!({"data":"ordinary metadata"})
    );
}
#[test]
fn payload_and_serialization_cancellation_drop_all_partial_segments_and_tickets() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let data = vec![71; 1024 * 1024 + 17];
    let item = reply(&pool, &data);
    let mut count = 0;
    assert!(item
        .serialized(&mut || {
            count += 1;
            if count == 5 {
                Err("cancel transport".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    assert_eq!(count, 5);
    assert_eq!(pool.used(), 0);
    let mut count = 0;
    let mut check = || {
        count += 1;
        if count == 4 {
            Err("cancel payload".into())
        } else {
            Ok(())
        }
    };
    let mut output = SegmentWriter::new(&pool, data.len(), &mut check).unwrap();
    assert!(output.write_all(&data).is_err());
    assert!(output.finish().is_err());
    assert_eq!(pool.used(), 0);
}
#[test]
fn foreign_pools_failed_admission_and_writer_exhaustion_never_create_a_reply() {
    let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let other = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let mut admission = ReplyAdmission::new(true);
    admission.admit(&pool, 1).unwrap();
    assert!(admission.image(0, bytes(&other, &[1])).is_err());
    assert_eq!(other.used(), 0);
    assert!(admission.admit(&other, 1).is_err());
    drop(admission);
    assert_eq!(pool.used(), 0);
    let small = ResidentBudget::new(1024).unwrap();
    let mut admission = ReplyAdmission::new(true);
    assert!(admission.admit(&small, 1).is_err());
    drop(admission);
    assert_eq!(small.used(), 0);
    let mut check = || Ok(());
    let mut output = SegmentWriter::new(&pool, 3, &mut check).unwrap();
    assert!(output.write_all(&[1, 2, 3, 4]).is_err());
    assert!(output.finish().is_err());
    assert_eq!(pool.used(), 0);
}
#[test]
fn metadata_counter_bounds_payloads_and_container_overhead_before_any_copy() {
    let value = json!({"assets":"x".repeat(1024*1024),"many":(0..1000).map(|n|json!({"x":n})).collect::<Vec<_>>()});
    let bound = admission::bound(&value).unwrap();
    assert!(bound > 1024 * 1024);
    assert!(bound < 4 * 1024 * 1024);
    struct Huge;
    impl Serialize for Huge {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            serializer.serialize_seq(Some(8 * 1024 * 1024 + 1))?.end()
        }
    }
    assert!(admission::bound(&Huge).is_err());
}
#[test]
fn serialized_reader_keeps_only_its_actual_charge_until_the_last_send_owner_drops() {
    let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
    let item = reply(&pool, &[1, 2, 3]);
    let data = item.serialized(&mut || Ok(())).unwrap();
    let charge = data.retained_bytes();
    assert_eq!(pool.used(), charge);
    let mut reader = data.reader();
    let mut first = [0; 7];
    assert_eq!(reader.read(&mut first).unwrap(), 7);
    assert_eq!(pool.used(), charge);
    drop(reader);
    assert_eq!(pool.used(), 0);
}
