//! Exact physical owner and connected manual state, serialized on the host control thread.
use super::*;
use crate::{
    abi,
    management::{
        Effect,
        maintenance::{Grant, QueryResult},
    },
};
use rpt_advanced_core::command_language::{MaintenanceAction as M, MixerTarget};

fn request(operation: u32) -> abi::rptadv_radio_control_v1 {
    abi::rptadv_radio_control_v1 {
        struct_size: std::mem::size_of::<abi::rptadv_radio_control_v1>() as u32,
        abi_version: 1,
        operation,
        keyed: 0,
        forced_ctcss_tenths_hz: 0,
        command: abi::UrpAstChannelCommand {
            struct_size: std::mem::size_of::<abi::UrpAstChannelCommand>() as u32,
            abi_version: 1,
            command: 0,
            target: 0,
            value: 0,
            flags: 0,
            eeprom_words: [0; 64],
        },
    }
}
impl Lease {
    pub(super) fn clear_manual(&mut self) -> Result<(), i32> {
        if self.maintenance.is_some() {
            self.worker
                .as_mut()
                .ok_or(-7)?
                .maintenance(&mut request(4))?;
            self.maintenance = None;
        }
        Ok(())
    }
}
impl Host {
    pub(crate) fn clear_maintenance(&mut self, grant: &Arc<Grant>) -> Result<(), i32> {
        for (_, lease) in &self.leases {
            let mut lease = lease.lock().unwrap_or_else(|e| e.into_inner());
            if lease
                .maintenance
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, grant))
            {
                lease.clear_manual()?;
            }
        }
        Ok(())
    }
    pub(super) fn expire_maintenance(&mut self) {
        for (name, lease) in &self.leases {
            let mut lease = lease.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(grant) = &lease.maintenance
                && (!grant.valid()
                    || !self.modular_revision_matches(&grant.revision)
                    || self
                        .runtime
                        .node(name)
                        .and_then(|node| node.command_work())
                        .is_none_or(|w| w.id() != grant.generation || !w.is_current()))
            {
                grant.revoke();
                let _ = lease.clear_manual(); // A failed physical clear stays pending on this owner and is retried.
            }
        }
    }
    pub(crate) fn execute_maintenance(&mut self, grant: Arc<Grant>, action: M) -> Effect {
        let local = grant.port.to_string();
        if !grant.valid()
            || self
                .runtime
                .node(&local)
                .and_then(|node| node.command_work())
                .is_none_or(|w| !w.is_current() || w.id() != grant.generation)
        {
            return Effect::Rejected;
        }
        let Some((_, owner)) = self.leases.iter().rev().find(|(name, _)| *name == local) else {
            return Effect::Rejected;
        };
        let mut lease = owner.lock().unwrap_or_else(|e| e.into_inner());
        if lease.quiesced || lease.quiescing || lease.worker.is_none() {
            return Effect::Rejected;
        }
        if let Some(old) = &lease.maintenance
            && !Arc::ptr_eq(old, &grant)
            && (old.valid() || lease.clear_manual().is_err())
        {
            return Effect::Rejected;
        }
        if !grant.valid()
            || !self.modular_revision_matches(&grant.revision)
            || self
                .runtime
                .node(&local)
                .and_then(|node| node.command_work())
                .is_none_or(|w| !w.is_current() || w.id() != grant.generation)
        {
            return Effect::Rejected;
        }
        let mut req = request(2);
        match &action {
            M::Status | M::ProcessingStatus | M::ActiveConfiguration => {
                let view = if matches!(action, M::ActiveConfiguration) {
                    "effective"
                } else {
                    "status"
                };
                return Effect::Query(QueryResult::View {
                    href: format!("{}/{view}/ports/{local}/", grant.mount),
                });
            }
            M::GetMixer(t) | M::SetMixer(t, _) => {
                req.command.target = match t {
                    MixerTarget::Receive => abi::URP_AST_MIXER_RECEIVE,
                    MixerTarget::TransmitA => abi::URP_AST_MIXER_TRANSMIT_A,
                    MixerTarget::TransmitB => abi::URP_AST_MIXER_TRANSMIT_B,
                };
                if let M::SetMixer(_, level) = &action {
                    req.command.command = abi::URP_AST_COMMAND_SET_MIXER;
                    req.command.value = i64::from(*level);
                } else {
                    req.command.command = abi::URP_AST_COMMAND_GET_MIXER;
                }
            }
            M::TestTone(on) | M::CtcssInhibit(on) | M::SubaudibleOverride(on) => {
                req.command.command = match &action {
                    M::TestTone(_) => abi::URP_AST_COMMAND_SET_TEST_TONE,
                    M::CtcssInhibit(_) => abi::URP_AST_COMMAND_SET_CTCSS_INHIBIT,
                    _ => abi::URP_AST_COMMAND_SET_SUBAUDIBLE_OVERRIDE,
                };
                req.command.value = i64::from(*on);
            }
            M::ReadEeprom => req.command.command = abi::URP_AST_COMMAND_READ_EEPROM,
            M::WriteEeprom(words) => {
                req.command.command = abi::URP_AST_COMMAND_WRITE_EEPROM;
                req.command.eeprom_words = **words;
            }
            M::SaveTuning => req.command.command = abi::URP_AST_COMMAND_SAVE_TUNING_EEPROM,
            M::Key(tone) => {
                req.operation = 3;
                req.keyed = 1;
                req.forced_ctcss_tenths_hz = u32::from(tone.unwrap_or_default());
            }
            M::Unkey => req.operation = 3,
            M::ClearManual => req.operation = 4,
        }
        // Register cleanup before issuing a request; even an uncertain provider return can have taken effect.
        if matches!(
            action,
            M::TestTone(_) | M::CtcssInhibit(_) | M::SubaudibleOverride(_) | M::Key(_) | M::Unkey
        ) {
            lease.maintenance = Some(grant);
        }
        if lease
            .worker
            .as_mut()
            .expect("live radio")
            .maintenance(&mut req)
            .is_err()
        {
            return Effect::Indeterminate;
        }
        match action {
            M::GetMixer(_) => match u16::try_from(req.command.value) {
                Ok(level) if level <= 999 => Effect::Query(QueryResult::Mixer { level }),
                _ => Effect::Indeterminate,
            },
            M::ReadEeprom => Effect::Query(QueryResult::Eeprom {
                words: req.command.eeprom_words.to_vec(),
            }),
            M::ClearManual => {
                lease.maintenance = None;
                Effect::Admitted
            }
            _ => Effect::Admitted,
        }
    }
}
