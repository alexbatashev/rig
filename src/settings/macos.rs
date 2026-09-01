use super::{Observed, Provider};
use crate::exec::{query, run, which, Cmd, Policy};
use anyhow::{bail, Context, Result};

pub struct Defaults;

fn flag(kind: &str) -> Result<&'static str> {
    Ok(match kind {
        "integer" => "-int",
        "boolean" => "-bool",
        "float" => "-float",
        "string" => "-string",
        other => bail!("defaults type {other} is not something rig can write"),
    })
}

impl Provider for Defaults {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn available(&self) -> bool {
        which("defaults")
    }

    fn read(&self, domain: &str, key: &str, policy: Policy) -> Result<Option<Observed>> {
        let done = query(&Cmd::new("defaults", &["read", domain, key]), policy)?;
        if !done.ok() {
            if done.stderr.contains("does not exist") {
                return Ok(None);
            }
            bail!(
                "defaults read {domain} {key} failed: {}",
                done.stderr.trim()
            )
        }
        let typed = query(&Cmd::new("defaults", &["read-type", domain, key]), policy)?;
        let kind = typed
            .stdout
            .trim()
            .strip_prefix("Type is ")
            .filter(|k| flag(k).is_ok())
            .map(ToString::to_string);
        Ok(Some(Observed {
            value: done.stdout.trim().to_string(),
            kind,
        }))
    }

    fn write(&self, domain: &str, key: &str, value: &Observed, policy: Policy) -> Result<()> {
        let kind = value
            .kind
            .as_deref()
            .context("value has no writable type")?;
        run(
            &Cmd::new(
                "defaults",
                &["write", domain, key, flag(kind)?, &value.value],
            ),
            policy,
        )?;
        Ok(())
    }

    fn delete(&self, domain: &str, key: &str, policy: Policy) -> Result<()> {
        run(&Cmd::new("defaults", &["delete", domain, key]), policy)?;
        Ok(())
    }

    fn desired(&self, value: &toml::Value) -> Result<Observed> {
        Ok(match value {
            toml::Value::Boolean(b) => Observed {
                value: u8::from(*b).to_string(),
                kind: Some("boolean".into()),
            },
            toml::Value::Integer(i) => Observed {
                value: i.to_string(),
                kind: Some("integer".into()),
            },
            toml::Value::Float(f) => Observed {
                value: f.to_string(),
                kind: Some("float".into()),
            },
            toml::Value::String(s) => Observed {
                value: s.clone(),
                kind: Some("string".into()),
            },
            _ => bail!("defaults values must be scalars"),
        })
    }
}
