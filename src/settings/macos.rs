use super::Provider;
use crate::exec::{run, try_run, which, Cmd, Policy};
use anyhow::{bail, Result};

pub struct Defaults;

fn flag(value: &toml::Value) -> Result<&'static str> {
    Ok(match value {
        toml::Value::Integer(_) => "-int",
        toml::Value::Boolean(_) => "-bool",
        toml::Value::Float(_) => "-float",
        toml::Value::String(_) => "-string",
        _ => bail!("defaults values must be scalars"),
    })
}

impl Provider for Defaults {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn available(&self) -> bool {
        which("defaults")
    }

    fn read(&self, domain: &str, key: &str, policy: Policy) -> Result<Option<String>> {
        let (stdout, stderr, ok) = try_run(&Cmd::new("defaults", &["read", domain, key]), policy)?;
        if ok {
            return Ok(Some(stdout.trim().to_string()));
        }
        if stderr.contains("does not exist") {
            return Ok(None);
        }
        bail!("defaults read {domain} {key} failed: {}", stderr.trim())
    }

    fn write(&self, domain: &str, key: &str, value: &toml::Value, policy: Policy) -> Result<()> {
        let literal = self.normalise(value);
        run(
            &Cmd::new("defaults", &["write", domain, key, flag(value)?, &literal]),
            policy,
        )?;
        Ok(())
    }

    fn normalise(&self, value: &toml::Value) -> String {
        match value {
            toml::Value::Boolean(b) => u8::from(*b).to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}
