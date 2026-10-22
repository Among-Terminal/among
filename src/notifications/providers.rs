//! identity-scoped policy/admission seam. attachment requires a real consumer;
//! metadata inspection never registers a live channel or grants permission.

use super::{Channel, Failure, Host, Job, Policy};
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub identity: String,
    pub channel: String,
}

impl Key {
    pub fn new(identity: &str, channel: &str) -> Result<Self, &'static str> {
        if identity.is_empty()
            || identity.len() > 256
            || identity.chars().any(char::is_control)
            || channel.is_empty()
            || channel.len() > 64
            || !channel
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("Invalid provider identity or channel");
        }
        Ok(Self {
            identity: identity.into(),
            channel: channel.into(),
        })
    }
}

#[derive(Default)]
pub struct Registry {
    session: Option<u64>,
    retained: BTreeMap<Key, Policy>,
    attached: BTreeMap<Key, Channel>,
}

impl Registry {
    pub fn configure(
        &mut self,
        host: &mut Host,
        retained: BTreeMap<Key, Policy>,
    ) -> Result<(), Failure> {
        self.bind(host)?;
        for (key, channel) in &self.attached {
            let before = self.policy(key);
            let after = retained
                .get(key)
                .copied()
                .unwrap_or(Policy::DELIVERY_DEFAULT);
            if before != after {
                host.set_channel_policy(*channel, after)?;
            }
        }
        self.retained = retained;
        Ok(())
    }

    pub fn policy(&self, key: &Key) -> Policy {
        self.retained
            .get(key)
            .copied()
            .unwrap_or(Policy::DELIVERY_DEFAULT)
    }

    pub fn available(&self, key: &Key) -> bool {
        self.attached.contains_key(key)
    }

    pub fn attach(&mut self, host: &mut Host, key: Key) -> Result<Channel, Failure> {
        self.bind(host)?;
        if let Some(channel) = self.attached.get(&key) {
            return Ok(*channel);
        }
        let channel = host.register_channel(self.policy(&key))?;
        self.attached.insert(key, channel);
        Ok(channel)
    }

    pub fn withdraw(&mut self, host: &mut Host, key: &Key) {
        if self.session != Some(host.session) {
            return;
        }
        if let Some(channel) = self.attached.remove(key) {
            host.remove_channel(channel);
        }
    }

    pub fn take_ready(&mut self, host: &mut Host, key: &Key, now: Instant) -> Option<Job> {
        host.take_ready(*self.attached.get(key)?, now)
    }

    fn bind(&mut self, host: &Host) -> Result<(), Failure> {
        if self.session.is_some_and(|session| session != host.session) {
            return Err(Failure::StaleChannel);
        }
        self.session = Some(host.session);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{Eligibility, Focus};

    #[test]
    fn two_real_synthetic_consumers_are_identity_scoped_and_withdrawal_keeps_choices() {
        let root =
            std::env::temp_dir().join(format!("among-provider-effects-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let now = Instant::now();
        let a = Key::new("origin-one/same-name", "delivery").unwrap();
        let b = Key::new("origin-two/same-name", "delivery").unwrap();
        let mut registry = Registry::default();
        let ca = registry.attach(&mut host, a.clone()).unwrap();
        let mut foreign = Host::new().unwrap();
        assert!(registry.attach(&mut foreign, a.clone()).is_err());
        registry.withdraw(&mut foreign, &a);
        assert!(registry.available(&a));
        registry.attach(&mut host, b.clone()).unwrap();
        let effect = |registry: &mut Registry, host: &mut Host, key: &Key, path: &str| {
            if let Some(job) = registry.take_ready(host, key, now) {
                if host
                    .authorization(job)
                    .is_some_and(|lease| lease.is_current())
                {
                    std::fs::write(root.join(path), job.event().id().to_string()).unwrap();
                }
            }
        };
        host.bell(source, Focus::default(), now).unwrap();
        effect(&mut registry, &mut host, &a, "off");
        assert!(!root.join("off").exists());
        registry
            .configure(
                &mut host,
                BTreeMap::from([(
                    a.clone(),
                    Policy {
                        enabled: true,
                        eligibility: Eligibility::Always,
                    },
                )]),
            )
            .unwrap();
        assert_eq!(registry.attach(&mut host, a.clone()).unwrap(), ca);
        host.bell(source, Focus::default(), now).unwrap();
        effect(&mut registry, &mut host, &a, "a");
        effect(&mut registry, &mut host, &b, "b");
        assert!(root.join("a").is_file() && !root.join("b").exists());
        registry.withdraw(&mut host, &a);
        assert!(!registry.available(&a) && registry.policy(&a).enabled);
        host.bell(source, Focus::default(), now).unwrap();
        effect(&mut registry, &mut host, &a, "unavailable");
        assert!(!root.join("unavailable").exists());
        assert!(!registry.policy(&b).enabled);
    }
}
