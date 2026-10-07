//! Shared bounded fault confirmation and durable deduplication for device adapters.
use crate::{
    Result,
    config::{private_options, read_bounded},
    error,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::Path,
};
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub devices: BTreeMap<String, Saved>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub endpoint: String,
    pub faults: Vec<String>,
}
#[derive(Clone)]
struct Candidate {
    faults: Vec<String>,
    since: u64,
}
#[derive(Clone, Default)]
pub struct Monitor {
    pub memory: Memory,
    candidates: BTreeMap<String, Candidate>,
}
#[derive(Serialize)]
pub struct Event {
    pub device: String,
    pub event: &'static str,
    pub faults: Vec<String>,
    pub previous_faults: Vec<String>,
}
impl Monitor {
    pub fn observe_faults(
        &mut self,
        name: &str,
        identity: &str,
        faults: Option<&[String]>,
        now: u64,
    ) -> Option<Event> {
        let Some(faults) = faults else {
            self.candidates.remove(name);
            return None;
        };
        let published = self
            .memory
            .devices
            .get(name)
            .map(|s| s.faults.clone())
            .unwrap_or_default();
        if faults == published.as_slice() {
            self.candidates.remove(name);
            return None;
        }
        let candidate = self
            .candidates
            .entry(name.to_owned())
            .or_insert_with(|| Candidate {
                faults: faults.to_vec(),
                since: now,
            });
        if candidate.faults.as_slice() != faults {
            *candidate = Candidate {
                faults: faults.to_vec(),
                since: now,
            };
        }
        let delay = if faults.is_empty() { 30 } else { 3 };
        if now.saturating_sub(candidate.since) < delay {
            return None;
        }
        self.memory.devices.insert(
            name.to_owned(),
            Saved {
                endpoint: identity.to_owned(),
                faults: faults.to_vec(),
            },
        );
        self.candidates.remove(name);
        Some(Event {
            device: name.to_owned(),
            event: if faults.is_empty() {
                "recovered"
            } else {
                "fault"
            },
            faults: faults.to_vec(),
            previous_faults: published,
        })
    }
}

impl Monitor {
    pub fn from_memory(memory: Memory) -> Self {
        Self {
            memory,
            ..Self::default()
        }
    }
    pub fn load(path: &Path, identities: &BTreeMap<String, String>) -> Result<Self> {
        let mut memory: Memory = match read_bounded(path, "Device alert memory") {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| error("Invalid device alert memory"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        memory
            .devices
            .retain(|name, saved| identities.get(name) == Some(&saved.endpoint));
        if memory
            .devices
            .values()
            .any(|s| s.faults.len() > 64 || s.faults.iter().any(|f| f.len() > 128))
        {
            return Err(error("Invalid device alert memory bounds"));
        }
        Ok(Self::from_memory(memory))
    }
    pub fn active_faults(&self) -> BTreeMap<String, Vec<String>> {
        self.memory
            .devices
            .iter()
            .filter(|(_, s)| !s.faults.is_empty())
            .map(|(name, s)| (name.clone(), s.faults.clone()))
            .collect()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec(&self.memory)?;
        if bytes.len() > 8192 {
            return Err(error("Device alert memory exceeds 8 KiB"));
        }
        let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        let mut file = private_options().write(true).create_new(true).open(&temp)?;
        let result = (|| -> Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}
