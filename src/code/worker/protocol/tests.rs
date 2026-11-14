use super::*;
use std::io::{Cursor, Write};

fn transfer(sender: &mut State, receiver: &mut State, message: Message, body: &[u8]) {
    let mut encoded = Vec::new();
    sender
        .send(&message, body, &mut encoded, &|| Ok(()))
        .unwrap();
    let mut storage = vec![0; body.len()];
    let frame = receiver
        .receive(&mut Cursor::new(encoded), &mut storage, &|| Ok(()))
        .unwrap();
    assert_eq!(frame.message, message);
    assert_eq!(frame.body, body);
}
fn start(depth: u16) -> (State, State) {
    let mut parent = State::parent([1; 16]).unwrap();
    let mut child = State::child();
    transfer(
        &mut parent,
        &mut child,
        Message::Start {
            config: Config {
                width: 1024,
                height: 1024,
                bit_depth: depth,
            },
            metadata_bytes: 2,
            script_bytes: 1,
            build: [7; 32],
        },
        b"{}x",
    );
    (parent, child)
}
fn terminal(parent: &mut State, child: &mut State) {
    transfer(
        child,
        parent,
        Message::Command { index: 0 },
        br#"{"op":"layer.update","layer":"layer","opacity":0.5}"#,
    );
    transfer(parent, child, Message::Continue, &[]);
    transfer(child, parent, Message::Done { commands: 1 }, &[]);
}

#[test]
fn ordered_native_words_commands_and_eof_are_required_for_transport_completion() {
    for depth in [8, 16] {
        let (mut parent, mut child) = start(depth);
        transfer(
            &mut child,
            &mut parent,
            Message::Progress { operations: 1024 },
            &[],
        );
        transfer(&mut parent, &mut child, Message::Continue, &[]);
        transfer(
            &mut child,
            &mut parent,
            Message::Point {
                layer: "layer".into(),
                point: [3, 4],
            },
            &[],
        );
        let words = if depth == 16 {
            [12347u16, 23459, 34571, 61379]
        } else {
            [31u16, 73, 149, 177]
        };
        let body: Vec<_> = words.into_iter().flat_map(u16::to_le_bytes).collect();
        transfer(&mut parent, &mut child, Message::Words { words: 4 }, &body);
        transfer(
            &mut child,
            &mut parent,
            Message::Patch {
                layer: "layer".into(),
                rect: [0, 0, 2, 1],
            },
            &[],
        );
        let body: Vec<_> = words
            .into_iter()
            .chain(words)
            .flat_map(u16::to_le_bytes)
            .collect();
        transfer(&mut parent, &mut child, Message::Words { words: 8 }, &body);
        assert_eq!(parent.pixels, 2);
        assert_eq!(child.pixels, 2);
        terminal(&mut parent, &mut child);
        assert!(!parent.successful());
        assert!(!child.successful());
        parent.exact_eof(&mut Cursor::new([]), &|| Ok(())).unwrap();
        assert!(parent.successful());
        assert!(parent
            .receive(&mut Cursor::new([]), &mut [], &|| Ok(()))
            .is_err());
        assert!(!parent.successful());
    }
}

#[test]
fn wrong_turns_counts_depths_replayed_progress_and_failed_reads_cannot_advance() {
    let (mut parent, mut child) = start(8);
    assert!(parent
        .send(&Message::Continue, &[], &mut Vec::new(), &|| Ok(()))
        .is_err());
    assert!(child
        .send(
            &Message::Done { commands: 0 },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    assert!(child
        .send(
            &Message::Point {
                layer: "layer".into(),
                point: [i64::MAX, 0]
            },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    transfer(
        &mut child,
        &mut parent,
        Message::Patch {
            layer: "layer".into(),
            rect: [0, 0, 512, 512],
        },
        &[],
    );
    assert!(parent
        .send(
            &Message::Words { words: 4 },
            &[0; 8],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    transfer(
        &mut parent,
        &mut child,
        Message::Error {
            message: "Read unavailable".into(),
        },
        &[],
    );
    assert_eq!(parent.pixels, 0);
    assert_eq!(child.pixels, 0);
    transfer(
        &mut child,
        &mut parent,
        Message::Point {
            layer: "layer".into(),
            point: [0, 0],
        },
        &[],
    );
    assert!(parent
        .send(
            &Message::Words { words: 4 },
            &[0, 1, 0, 0, 0, 0, 0, 0],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    transfer(
        &mut parent,
        &mut child,
        Message::Words { words: 4 },
        &[0; 8],
    );
    transfer(
        &mut child,
        &mut parent,
        Message::Progress { operations: 1024 },
        &[],
    );
    transfer(&mut parent, &mut child, Message::Continue, &[]);
    assert!(child
        .send(
            &Message::Progress { operations: 1024 },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    assert!(child
        .send(
            &Message::Progress {
                operations: 2_000_001
            },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    assert!(child
        .send(
            &Message::Command { index: 1 },
            b"{}",
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    assert_eq!(child.commands, 0);
}

#[test]
fn a_complete_successful_patch_consumes_the_shared_pixel_budget() {
    let (mut parent, mut child) = start(16);
    transfer(
        &mut child,
        &mut parent,
        Message::Patch {
            layer: "layer".into(),
            rect: [0, 0, 512, 512],
        },
        &[],
    );
    let body = vec![0; MAX_PIXELS * 8];
    transfer(
        &mut parent,
        &mut child,
        Message::Words {
            words: (MAX_PIXELS * 4) as u32,
        },
        &body,
    );
    assert!(child
        .send(
            &Message::Patch {
                layer: "layer".into(),
                rect: [0, 0, 1, 1]
            },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
    transfer(
        &mut child,
        &mut parent,
        Message::Point {
            layer: "layer".into(),
            point: [0, 0],
        },
        &[],
    );
    transfer(
        &mut parent,
        &mut child,
        Message::Abort {
            message: "Task ended".into(),
        },
        &[],
    );
    assert!(!parent.successful());
    assert!(!child.successful());
    assert!(child
        .send(
            &Message::Failed {
                message: "Stop".into()
            },
            &[],
            &mut Vec::new(),
            &|| Ok(())
        )
        .is_err());
}

#[test]
fn corrupt_truncated_foreign_replayed_and_oversized_frames_poison_the_receiver() {
    let (parent, mut child) = start(16);
    let mut encoded = Vec::new();
    child
        .send(
            &Message::Point {
                layer: "layer".into(),
                point: [0, 0],
            },
            &[],
            &mut encoded,
            &|| Ok(()),
        )
        .unwrap();
    for fault in [
        "magic",
        "nonce",
        "sequence",
        "control",
        "body",
        "hash",
        "truncated",
    ] {
        let mut data = encoded.clone();
        match fault {
            "magic" => data[0] ^= 1,
            "nonce" => data[8] ^= 1,
            "sequence" => data[24] ^= 1,
            "control" => data[32..36].copy_from_slice(&u32::MAX.to_le_bytes()),
            "body" => data[36..44].copy_from_slice(&u64::MAX.to_le_bytes()),
            "hash" => data[44] ^= 1,
            "truncated" => {
                data.pop();
            }
            _ => unreachable!(),
        }
        let mut receiver = parent.clone();
        assert!(
            receiver
                .receive(&mut Cursor::new(data), &mut [], &|| Ok(()))
                .is_err(),
            "{fault}"
        );
        assert!(
            receiver
                .receive(&mut Cursor::new(&encoded), &mut [], &|| Ok(()))
                .is_err(),
            "{fault}"
        );
    }
    let mut receiver = parent;
    receiver
        .receive(&mut Cursor::new(&encoded), &mut [], &|| Ok(()))
        .unwrap();
    assert!(receiver
        .receive(&mut Cursor::new(&encoded), &mut [], &|| Ok(()))
        .is_err());
}

struct ShortWrite {
    bytes: usize,
}
impl Write for ShortWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes == 0 {
            return Err(std::io::Error::other("Closed"));
        }
        let count = bytes.len().min(self.bytes);
        self.bytes -= count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("Flush failed"))
    }
}
#[test]
fn partial_io_late_flush_cancellation_and_trailing_output_cannot_be_retried_as_success() {
    let (parent, child) = start(16);
    for remaining in [1, 76, 4096] {
        let mut sender = child.clone();
        assert!(sender
            .send(
                &Message::Progress { operations: 1024 },
                &[],
                &mut ShortWrite { bytes: remaining },
                &|| Ok(())
            )
            .is_err());
        assert!(sender
            .send(
                &Message::Progress { operations: 1024 },
                &[],
                &mut Vec::new(),
                &|| Ok(())
            )
            .is_err());
    }
    let mut receiver = parent.clone();
    assert!(receiver
        .receive(&mut Cursor::new([]), &mut [], &|| Err("Canceled".into()))
        .is_err());
    assert!(receiver.poisoned);
    let (mut parent, mut child) = (parent, child);
    terminal(&mut parent, &mut child);
    assert!(parent.exact_eof(&mut Cursor::new([1]), &|| Ok(())).is_err());
    assert!(!parent.successful());
}

fn raw(control: &[u8], body: &[u8], sequence: u64) -> Vec<u8> {
    let mut header = [0u8; 76];
    header[..8].copy_from_slice(b"PBPCOD01");
    header[8..24].copy_from_slice(&[1; 16]);
    header[24..32].copy_from_slice(&sequence.to_le_bytes());
    header[32..36].copy_from_slice(&(control.len() as u32).to_le_bytes());
    header[36..44].copy_from_slice(&(body.len() as u64).to_le_bytes());
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(control);
    hash.update(body);
    header[44..].copy_from_slice(hash.finish().as_ref());
    header
        .into_iter()
        .chain(control.iter().copied())
        .chain(body.iter().copied())
        .collect()
}

#[test]
fn valid_checksums_do_not_admit_unknown_duplicate_or_wrong_depth_data() {
    let (parent, mut child) = start(8);
    for control in [
        br#"{"kind":"Point","layer":"layer","point":[0,0],"path":"unused"}"#.as_slice(),
        br#"{"kind":"Point","layer":"layer","layer":"other","point":[0,0]}"#,
        br#"{"kind":"Point","kind":"Patch","layer":"layer","point":[0,0]}"#,
        br#"{"kind":"Point","layer":"layer","point":[0,0,0]}"#,
    ] {
        let mut receiver = parent.clone();
        assert!(receiver
            .receive(&mut Cursor::new(raw(control, &[], 1)), &mut [], &|| Ok(()))
            .is_err());
    }
    child
        .send(
            &Message::Point {
                layer: "layer".into(),
                point: [0, 0],
            },
            &[],
            &mut Vec::new(),
            &|| Ok(()),
        )
        .unwrap();
    let forged = raw(
        br#"{"kind":"Words","words":4}"#,
        &[0, 1, 0, 0, 0, 0, 0, 0],
        2,
    );
    assert!(child
        .receive(&mut Cursor::new(forged), &mut [0; 8], &|| Ok(()))
        .is_err());
    assert!(child.poisoned);
    assert!(!child.successful());
}

#[test]
fn caller_storage_refusal_happens_before_control_or_body_read_and_start_bounds_are_strict() {
    let mut parent = State::parent([1; 16]).unwrap();
    let message = Message::Start {
        config: Config {
            width: 1,
            height: 1,
            bit_depth: 16,
        },
        metadata_bytes: 2,
        script_bytes: 1,
        build: [7; 32],
    };
    let mut frame = Vec::new();
    parent
        .send(&message, b"{}x", &mut frame, &|| Ok(()))
        .unwrap();
    let mut reader = Cursor::new(frame);
    let mut child = State::child();
    assert!(child.receive(&mut reader, &mut [0; 2], &|| Ok(())).is_err());
    assert_eq!(reader.position(), 76);
    assert!(child.nonce.is_none());
    assert!(child.config.is_none());
    assert!(State::parent([0; 16]).is_err());
    for invalid in [
        Message::Start {
            config: Config {
                width: 0,
                height: 1,
                bit_depth: 16,
            },
            metadata_bytes: 2,
            script_bytes: 1,
            build: [7; 32],
        },
        Message::Start {
            config: Config {
                width: 1,
                height: 1,
                bit_depth: 32,
            },
            metadata_bytes: 2,
            script_bytes: 1,
            build: [7; 32],
        },
        Message::Start {
            config: Config {
                width: 1,
                height: 1,
                bit_depth: 16,
            },
            metadata_bytes: 2,
            script_bytes: (SCRIPT_LIMIT + 1) as u32,
            build: [7; 32],
        },
        Message::Start {
            config: Config {
                width: 1,
                height: 1,
                bit_depth: 16,
            },
            metadata_bytes: 2,
            script_bytes: 1,
            build: [0; 32],
        },
    ] {
        assert!(State::parent([1; 16])
            .unwrap()
            .send(&invalid, b"{}x", &mut Vec::new(), &|| Ok(()))
            .is_err());
    }
    assert!(State::parent([1; 16])
        .unwrap()
        .send(&message, b"{} ", &mut Vec::new(), &|| Ok(()))
        .is_err());
}
