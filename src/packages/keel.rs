use super::PackageBackend;
use crate::exec::Policy;
use crate::repo::Backend;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

/// Keel does not exist yet; every operation is an error the user can read.
pub struct Keel;

impl PackageBackend for Keel {
    fn kind(&self) -> Backend {
        Backend::Keel
    }

    fn available(&self) -> bool {
        true
    }

    fn version(&self, _policy: Policy) -> Option<String> {
        None
    }

    fn installed(&self, _names: &BTreeSet<String>, _policy: Policy) -> Result<BTreeSet<String>> {
        bail!("keel backend not implemented yet")
    }

    fn install(&mut self, _names: &[String], _policy: Policy) -> Result<()> {
        bail!("keel backend not implemented yet")
    }

    fn remove(&mut self, _names: &[String], _policy: Policy) -> Result<()> {
        bail!("keel backend not implemented yet")
    }
}
