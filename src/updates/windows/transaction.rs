//! Apply/health/recovery ordering. Native authority is supplied by the live owner,
//! never reconstructed from a recorded result or a progress percentage.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Role {
    Previous,
    Selected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Installed {
    Previous,
    Selected,
    Other,
    Missing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Event {
    ApplyIntent,
    ApplySucceeded,
    ApplyFailed,
    RollbackIntent,
    RollbackSucceeded,
    RollbackFailed,
    Healthy,
    Recovered,
    Hold,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Healthy,
    Recovered,
}

pub(super) trait Host {
    /// Real kernel parent exit, acknowledged final writer seal, owned gate and
    /// independently acquired profile lock. Every call also rechecks all bytes,
    /// identities, source/profile bindings and exact transition inventory.
    fn guard(&mut self) -> Result<()>;
    fn installed(&mut self) -> Result<Installed>;
    /// Exclusive durable record, bound to both signed media and final profile.
    fn record(&mut self, event: Event) -> Result<()>;
    /// Return only after actual native terminal observation; cancellation is
    /// drained under ownership. An unknown operation is never treated as failure.
    fn deploy(&mut self, role: Role) -> Result<bool>;
    /// Authenticated actual package child, isolated copy of the final profile;
    /// no document/default/profile mutation in the user's normal environment.
    /// Ok(false) requires an observed failed/exited child. Err means uncertain
    /// ownership/lifetime and must hold, never roll back beneath a live process.
    fn health(&mut self, role: Role) -> Result<bool>;
    /// Recheck compatibility and prepare exact recovery-media promotion. Do not
    /// retire anything until the subsequent durable final record is read back.
    fn finish(&mut self, outcome: Outcome) -> Result<()>;
}

pub(super) fn execute(host: &mut dyn Host) -> Result<Outcome> {
    let result = run(host);
    if result.is_err() {
        // Failure to record is itself a hold. Never overwrite an earlier record,
        // infer health, release startup admission or launch a normal workspace.
        let _ = host.record(Event::Hold);
    }
    result
}
fn run(host: &mut dyn Host) -> Result<Outcome> {
    host.guard()?;
    if host.installed()? != Installed::Previous {
        return Err(STAGE_HOLD.into());
    }
    host.record(Event::ApplyIntent)?;
    host.guard()?;
    let applied = host.deploy(Role::Selected)?;
    host.record(if applied {
        Event::ApplySucceeded
    } else {
        Event::ApplyFailed
    })?;
    host.guard()?;
    let installed = host.installed()?;
    if applied && installed == Installed::Selected && host.health(Role::Selected)? {
        host.guard()?;
        if host.installed()? != Installed::Selected {
            return Err(STAGE_HOLD.into());
        }
        host.finish(Outcome::Healthy)?;
        host.record(Event::Healthy)?;
        return Ok(Outcome::Healthy);
    }
    match installed {
        Installed::Other => return Err(STAGE_HOLD.into()),
        Installed::Selected | Installed::Missing => {
            host.guard()?;
            host.record(Event::RollbackIntent)?;
            host.guard()?;
            let restored = host.deploy(Role::Previous)?;
            host.record(if restored {
                Event::RollbackSucceeded
            } else {
                Event::RollbackFailed
            })?;
            if !restored {
                return Err(STAGE_HOLD.into());
            }
        }
        Installed::Previous => {}
    }
    host.guard()?;
    if host.installed()? != Installed::Previous {
        return Err(STAGE_HOLD.into());
    }
    if !host.health(Role::Previous)? {
        return Err(STAGE_HOLD.into());
    }
    host.guard()?;
    if host.installed()? != Installed::Previous {
        return Err(STAGE_HOLD.into());
    }
    host.finish(Outcome::Recovered)?;
    host.record(Event::Recovered)?;
    Ok(Outcome::Recovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        installed: Installed,
        fail_at: Option<usize>,
        calls: Vec<&'static str>,
        events: Vec<Event>,
        new_health: bool,
        apply: bool,
        rollback: bool,
        terminal_installed: Option<Installed>,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                installed: Installed::Previous,
                fail_at: None,
                calls: vec![],
                events: vec![],
                new_health: true,
                apply: true,
                rollback: true,
                terminal_installed: None,
            }
        }
        fn call(&mut self, name: &'static str) -> Result<()> {
            self.calls.push(name);
            if self.fail_at == Some(self.calls.len()) {
                Err("injected boundary refusal".into())
            } else {
                Ok(())
            }
        }
    }
    impl Host for Fixture {
        fn guard(&mut self) -> Result<()> {
            self.call("guard")
        }
        fn installed(&mut self) -> Result<Installed> {
            self.call("installed")?;
            Ok(self.installed)
        }
        fn record(&mut self, event: Event) -> Result<()> {
            self.call("record")?;
            self.events.push(event);
            Ok(())
        }
        fn deploy(&mut self, role: Role) -> Result<bool> {
            self.call(if role == Role::Selected {
                "apply"
            } else {
                "rollback"
            })?;
            assert!(self.events.contains(if role == Role::Selected {
                &Event::ApplyIntent
            } else {
                &Event::RollbackIntent
            }));
            let success = if role == Role::Selected {
                self.apply
            } else {
                self.rollback
            };
            if success {
                self.installed = if role == Role::Selected {
                    Installed::Selected
                } else {
                    Installed::Previous
                };
            }
            if role == Role::Selected {
                if let Some(installed) = self.terminal_installed {
                    self.installed = installed;
                }
            }
            Ok(success)
        }
        fn health(&mut self, role: Role) -> Result<bool> {
            self.call(if role == Role::Selected {
                "new health"
            } else {
                "old health"
            })?;
            Ok(role != Role::Selected || self.new_health)
        }
        fn finish(&mut self, outcome: Outcome) -> Result<()> {
            self.call("finish")?;
            assert!(self.calls.contains(if outcome == Outcome::Healthy {
                &"new health"
            } else {
                &"old health"
            }));
            Ok(())
        }
    }
    #[test]
    fn forward_health_and_compatible_rollback_require_fresh_guards_and_terminal_deployment() {
        let mut healthy = Fixture::new();
        assert_eq!(execute(&mut healthy).unwrap(), Outcome::Healthy);
        assert_eq!(
            healthy.events,
            [Event::ApplyIntent, Event::ApplySucceeded, Event::Healthy]
        );
        assert!(!healthy.calls.contains(&"rollback"));
        let mut failed = Fixture::new();
        failed.new_health = false;
        assert_eq!(execute(&mut failed).unwrap(), Outcome::Recovered);
        assert_eq!(
            failed.events,
            [
                Event::ApplyIntent,
                Event::ApplySucceeded,
                Event::RollbackIntent,
                Event::RollbackSucceeded,
                Event::Recovered
            ]
        );
        let mut refused = Fixture::new();
        refused.apply = false;
        assert_eq!(execute(&mut refused).unwrap(), Outcome::Recovered);
        assert!(!refused.calls.contains(&"rollback"));
        let mut broken = Fixture::new();
        broken.new_health = false;
        broken.rollback = false;
        assert!(execute(&mut broken).is_err());
        assert!(!broken.calls.contains(&"finish"));
    }
    #[test]
    fn known_terminal_missing_registration_can_restore_only_the_previous_package() {
        let mut missing = Fixture::new();
        missing.apply = false;
        missing.terminal_installed = Some(Installed::Missing);
        assert_eq!(execute(&mut missing).unwrap(), Outcome::Recovered);
        assert!(missing.events.contains(&Event::RollbackIntent));
        assert!(missing.events.contains(&Event::RollbackSucceeded));
        let mut foreign = Fixture::new();
        foreign.terminal_installed = Some(Installed::Other);
        assert!(execute(&mut foreign).is_err());
        assert!(!foreign.calls.contains(&"rollback"));
        let mut initially_missing = Fixture::new();
        initially_missing.installed = Installed::Missing;
        assert!(execute(&mut initially_missing).is_err());
        assert!(!initially_missing.calls.contains(&"apply"));
    }
    #[test]
    fn every_failed_irreversible_boundary_holds_without_claiming_health_or_retirement() {
        for recover in [false, true] {
            let mut complete = Fixture::new();
            complete.new_health = !recover;
            execute(&mut complete).unwrap();
            for index in 1..=complete.calls.len() {
                let mut failed = Fixture::new();
                failed.new_health = !recover;
                failed.fail_at = Some(index);
                let result = execute(&mut failed);
                assert!(
                    result.is_err(),
                    "boundary {index}; uncertain health must hold"
                );
                assert!(!failed.events.contains(&Event::Healthy));
                assert!(!failed.events.contains(&Event::Recovered));
            }
        }
        let mut foreign = Fixture::new();
        foreign.installed = Installed::Other;
        assert!(execute(&mut foreign).is_err());
        assert!(!foreign.calls.contains(&"apply"));
    }
}
