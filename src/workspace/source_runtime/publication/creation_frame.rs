//! Fixed creation transport, with no writer/adoption constructors.
use super::frame::IntentFrame;
use crate::file_safety::ExternalCreationRole;
use ring::digest::{Context, SHA256};
use std::io::{Cursor, Write};
pub(super) const BYTES: usize = 209;
const MAGIC: &[u8; 16] = b"PBEXCREATED1\0\0\0\0";
const DOMAIN: &[u8] = b"PeerBrushExternalCreation1\0";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) id: [u8; 16],
    pub(super) owner: [u8; 16],
    pub(super) intent: [u8; 32],
    pub(super) target: [u8; 32],
    pub(super) growth: u64,
    pub(super) role: ExternalCreationRole,
    pub(super) parent: (u64, u64),
    pub(super) fact: crate::large_document::ExternalFact,
}
impl Record {
    pub(super) fn validate(&self, frame: &IntentFrame, intent: [u8; 32]) -> Result<(), String> {
        let parent = match self.role {
            ExternalCreationRole::Temporary => frame.parent,
            ExternalCreationRole::Backup => {
                if frame.backup.is_none() {
                    return Err("Creation has no planned backup".into());
                }
                frame.backups_identity
            }
        };
        if self.id != frame.id
            || self.owner != frame.owner
            || self.intent != intent
            || self.target != frame.target_key
            || self.growth != frame.growth
            || self.parent != parent
            || self.fact.bytes != 0
        {
            return Err("Creation registration differs from its exact zero-byte intent".into());
        }
        Ok(())
    }
    pub(super) fn encode(&self) -> [u8; BYTES] {
        let mut bytes = [0; BYTES];
        let mut out = Cursor::new(&mut bytes[..BYTES - 32]);
        for part in [
            MAGIC.as_slice(),
            self.id.as_slice(),
            self.owner.as_slice(),
            self.intent.as_slice(),
            self.target.as_slice(),
            &self.growth.to_be_bytes(),
            &[tag(self.role) as u8],
            &self.parent.0.to_be_bytes(),
            &self.parent.1.to_be_bytes(),
            &self.fact.identity.0.to_be_bytes(),
            &self.fact.identity.1.to_be_bytes(),
            &self.fact.bytes.to_be_bytes(),
            &self.fact.modified.to_be_bytes(),
        ] {
            out.write_all(part).expect("fixed creation frame layout");
        }
        debug_assert_eq!(out.position() as usize, BYTES - 32);
        drop(out);
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        hash.update(&bytes[..BYTES - 32]);
        bytes[BYTES - 32..].copy_from_slice(hash.finish().as_ref());
        bytes
    }
    pub(super) fn decode(
        bytes: &[u8],
        frame: &IntentFrame,
        intent: [u8; 32],
    ) -> Result<Self, String> {
        if bytes.len() != BYTES {
            return Err("Creation record is incomplete".into());
        }
        let mut hash = Context::new(&SHA256);
        hash.update(DOMAIN);
        hash.update(&bytes[..BYTES - 32]);
        if hash.finish().as_ref() != &bytes[BYTES - 32..] {
            return Err("Creation record SHA differs".into());
        }
        let mut input = Input { bytes, at: 0 };
        if &input.array::<16>()? != MAGIC {
            return Err("Unknown creation record retained".into());
        }
        let id = input.array()?;
        let owner = input.array()?;
        let intent_sha = input.array()?;
        let target = input.array()?;
        let growth = input.u64()?;
        let role = match input.array::<1>()?[0] {
            0 => ExternalCreationRole::Temporary,
            1 => ExternalCreationRole::Backup,
            _ => return Err("Unknown creation role".into()),
        };
        let parent = input.id()?;
        let fact = crate::large_document::ExternalFact {
            identity: input.id()?,
            bytes: input.u64()?,
            modified: u128::from_be_bytes(input.array()?),
        };
        let record = Self {
            id,
            owner,
            intent: intent_sha,
            target,
            growth,
            role,
            parent,
            fact,
        };
        record.validate(frame, intent)?;
        Ok(record)
    }
    pub(super) fn incomplete(
        bytes: &[u8],
        frame: &IntentFrame,
        intent: [u8; 32],
        role: ExternalCreationRole,
    ) -> Result<(), String> {
        if bytes.len() >= BYTES || role == ExternalCreationRole::Backup && frame.backup.is_none() {
            return Err("Creation record is not incomplete".into());
        }
        let expected = Self {
            id: frame.id,
            owner: frame.owner,
            intent,
            target: frame.target_key,
            growth: frame.growth,
            role,
            parent: match role {
                ExternalCreationRole::Temporary => frame.parent,
                ExternalCreationRole::Backup => frame.backups_identity,
            },
            fact: crate::large_document::ExternalFact {
                identity: (0, 0),
                bytes: 0,
                modified: 0,
            },
        }
        .encode();
        // Physical ID/stamp are unknown until the complete creation seal.
        let count = bytes.len().min(137);
        if bytes[..count] != expected[..count] {
            return Err("Incomplete creation has an unknown intent or role".into());
        }
        Ok(())
    }
}
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl Input<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let end = self
            .at
            .checked_add(N)
            .filter(|n| *n <= BYTES - 32)
            .ok_or("Truncated creation record")?;
        let out = self.bytes[self.at..end].try_into().unwrap();
        self.at = end;
        Ok(out)
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn id(&mut self) -> Result<(u64, u64), String> {
        Ok((self.u64()?, self.u64()?))
    }
}

fn tag(role: ExternalCreationRole) -> usize {
    match role {
        ExternalCreationRole::Temporary => 0,
        ExternalCreationRole::Backup => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::super::frame::{Expected, NamedFact, Role};
    use super::*;
    fn fixture() -> IntentFrame {
        let parent = std::env::current_dir().unwrap();
        IntentFrame {
            id: [1; 16],
            owner: [2; 16],
            operation: [3; 16],
            target: parent.join("n.psb"),
            target_key: [4; 32],
            previous: None,
            parent: (1, 99),
            backups: parent.join(".peerbrush-versions"),
            backups_identity: (1, 98),
            expected: Some(Expected {
                identity: (1, 1),
                bytes: 4,
                modified: 1,
                sha256: [5; 32],
            }),
            candidate_bytes: 8,
            candidate_sha256: [6; 32],
            provenance: [7; 32],
            core: [8; 32],
            render: [9; 32],
            temporary: "native.tmp".into(),
            backup: Some("native-backup.psb".into()),
            growth: 12,
            facts: vec![NamedFact {
                role: Role::Target,
                name: "n.psb".into(),
                fact: crate::large_document::ExternalFact {
                    identity: (1, 1),
                    bytes: 4,
                    modified: 1,
                },
            }],
        }
    }
    fn record(role: ExternalCreationRole) -> Record {
        Record {
            id: [1; 16],
            owner: [2; 16],
            intent: [10; 32],
            target: [4; 32],
            growth: 12,
            role,
            parent: if role == ExternalCreationRole::Temporary {
                (1, 99)
            } else {
                (1, 98)
            },
            fact: crate::large_document::ExternalFact {
                identity: (1, 77),
                bytes: 0,
                modified: 123,
            },
        }
    }
    #[test]
    fn zero_byte_creation_roles_roundtrip_all_fixed_fields_and_classify_partial_prefixes() {
        let frame = fixture();
        for role in [
            ExternalCreationRole::Temporary,
            ExternalCreationRole::Backup,
        ] {
            let expected = record(role);
            let bytes = expected.encode();
            assert_eq!(Record::decode(&bytes, &frame, [10; 32]).unwrap(), expected);
            for at in 0..BYTES {
                assert!(Record::decode(&bytes[..at], &frame, [10; 32]).is_err());
                Record::incomplete(&bytes[..at], &frame, [10; 32], role).unwrap();
            }
        }
    }
    #[test]
    fn wrong_intent_role_parent_nonzero_file_and_corrupt_seal_cannot_register() {
        let frame = fixture();
        let expected = record(ExternalCreationRole::Temporary);
        let mut bytes = expected.encode();
        assert!(Record::decode(&bytes, &frame, [11; 32]).is_err());
        assert!(Record::incomplete(
            &bytes[..137],
            &frame,
            [10; 32],
            ExternalCreationRole::Backup
        )
        .is_err());
        bytes[BYTES - 1] ^= 1;
        assert!(Record::decode(&bytes, &frame, [10; 32]).is_err());
        let mut bad = expected;
        bad.fact.bytes = 1;
        assert!(Record::decode(&bad.encode(), &frame, [10; 32]).is_err());
        let mut bad = expected;
        bad.parent = (1, 55);
        assert!(Record::decode(&bad.encode(), &frame, [10; 32]).is_err());
        let mut no_backup = fixture();
        no_backup.expected = None;
        no_backup.backup = None;
        no_backup.facts.clear();
        no_backup.growth = 8;
        assert!(
            Record::incomplete(&[], &no_backup, [10; 32], ExternalCreationRole::Backup).is_err()
        );
    }
}
