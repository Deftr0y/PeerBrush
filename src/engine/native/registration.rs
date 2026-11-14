//! Runtime identity is assigned only after source/fidelity qualification and
//! before project registration. The verified source and pixel owners stay intact.
use super::*;

impl NativeDocument {
    /// This prepares an unregistered owner outside Engine/registry locks. It is
    /// not a source proof, registry permission, history edit or file publication.
    pub(crate) fn for_runtime_registration(
        &self,
        filename: Option<&str>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        let bytes = filename
            .map_or(0, str::len)
            .checked_add(128)
            .ok_or("Native open identity admission overflow")?;
        let identity_charge = MetadataCharge::reserve(self.budget(), bytes, check)?;
        let name = filename
            .map(|filename| -> Result<_> {
                let mut name = String::new();
                name.try_reserve_exact(filename.len())
                    .map_err(|_| "Native open filename allocation refused")?;
                if name.capacity() > filename.len() {
                    return Err("Native open filename exceeds admission".into());
                }
                name.push_str(filename);
                Ok(name)
            })
            .transpose()?;
        let copied = copy_metadata(&self.core.metadata, self.budget(), check)?;
        let mut identity = String::new();
        identity
            .try_reserve_exact(36)
            .map_err(|_| "Runtime identity allocation refused")?;
        if identity.capacity() > 36 {
            return Err("Runtime identity exceeds admission".into());
        }
        let mut encoded = [0u8; 36];
        identity.push_str(uuid::Uuid::new_v4().hyphenated().encode_lower(&mut encoded));
        check()?;
        let roster = self.core.roster.clone();
        let resident = self.core.resident.clone();
        let disk = self.core.disk.clone();
        let core = copied.rehome_unique(|mut metadata, _| {
            metadata.id = identity;
            if let Some(name) = name {
                metadata.name = name;
            }
            Core::prepare(metadata, roster, resident, disk, check)
        })?;
        // The new header now owns the identity allocation independently of this
        // staging ticket. No old complete-header admission enters the result.
        drop(identity_charge);
        check()?;
        Ok(Self { core })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_runtime_identity_preserves_exact_sources_payloads_and_revision() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let original = super::super::tests::document(&pool);
        let key = original.key();
        let slot = original.metadata().layer(0).unwrap().pixels().slot();
        let source_key = original.source(slot).unwrap().key();
        let registered = original.for_runtime_registration(None, &|| Ok(())).unwrap();
        assert_ne!(registered.key(), key);
        assert_ne!(registered.metadata().id(), original.metadata().id());
        assert!(uuid::Uuid::parse_str(registered.metadata().id()).is_ok());
        assert_eq!(
            registered.metadata().revision(),
            original.metadata().revision()
        );
        assert_eq!(registered.metadata().name(), original.metadata().name());
        assert_eq!(
            registered.metadata().profile(),
            original.metadata().profile()
        );
        assert_eq!(
            registered.metadata().profile().unwrap().as_ptr(),
            original.metadata().profile().unwrap().as_ptr()
        );
        assert!(Arc::ptr_eq(&registered.core.roster, &original.core.roster));
        assert!(registered.source(slot).is_err());
        assert_eq!(
            registered
                .source(registered.metadata().layer(0).unwrap().pixels().slot())
                .unwrap()
                .key(),
            source_key
        );
        assert_eq!(original.key(), key);
        drop((registered, original));
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn registration_cancel_and_admission_refusal_preserve_original_owners() {
        let pool = ResidentBudget::new(8 * 1024 * 1024).unwrap();
        let original = super::super::tests::document(&pool);
        let key = original.key();
        let baseline = pool.used();
        assert!(original
            .for_runtime_registration(None, &|| Err("cancelled registration".into()))
            .is_err());
        assert_eq!(pool.used(), baseline);
        let remainder = pool.limit() - baseline - 1;
        let ballast = pool.reserve_bytes(remainder, remainder).unwrap();
        let occupied = pool.used();
        assert!(original.for_runtime_registration(None, &|| Ok(())).is_err());
        assert_eq!(pool.used(), occupied);
        assert_eq!(original.key(), key);
        drop(ballast);
        assert_eq!(pool.used(), baseline);
        drop(original);
        assert_eq!(pool.used(), 0);
    }
}
