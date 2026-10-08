//! Fixed-size on-disk numeric history; no database, retained snapshots or cache.
use crate::{
    Result, config,
    device::{Adapter, Hints, Input, Tool},
    entity::{Entity, Support},
    error, web,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
const RECORD: usize = 32;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub entities: Vec<String>,
    #[serde(default = "interval")]
    pub interval_seconds: u64,
    #[serde(default = "points")]
    pub max_points: usize,
}
fn interval() -> u64 {
    300
}
fn points() -> usize {
    4096
}
pub struct Manager {
    pub config: Config,
    directory: PathBuf,
    sequences: Vec<u64>,
    _lock: File,
}
impl Manager {
    pub fn open(path: &Path) -> Result<Self> {
        let config: Config = crate::media::load(path)?;
        if config.entities.is_empty()
            || config.entities.len() > 32
            || !(60..=3600).contains(&config.interval_seconds)
            || !(96..=16384).contains(&config.max_points)
            || config
                .entities
                .iter()
                .enumerate()
                .any(|(i, s)| !valid_entity(s) || config.entities[..i].contains(s))
        {
            return Err(error(
                "History requires 1–32 unique numeric entities, interval 60–3600 and 96–16384 points",
            ));
        }
        let directory = path.with_extension("history");
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = config::lock_configuration(path)?;
        let mut sequences = Vec::with_capacity(config.entities.len());
        for entity in &config.entities {
            let mut highest = 0;
            scan(
                &directory.join(format!("{}.bin", web::encode(entity))),
                config.max_points,
                |seq, _, _| highest = highest.max(seq),
            )?;
            sequences.push(highest);
        }
        Ok(Self {
            config,
            directory,
            sequences,
            _lock: lock,
        })
    }
    pub fn record(&mut self, entity: &str, timestamp: i64, value: f64) -> Result<()> {
        if timestamp <= 0 || !value.is_finite() {
            return Err(error("Invalid numeric history sample"));
        }
        let index = self
            .config
            .entities
            .iter()
            .position(|e| e == entity)
            .ok_or_else(|| error("Unconfigured history entity"))?;
        let seq = self.sequences[index]
            .checked_add(1)
            .ok_or_else(|| error("History sequence overflow"))?;
        let mut record = [0u8; RECORD];
        record[..8].copy_from_slice(&seq.to_le_bytes());
        record[8..16].copy_from_slice(&timestamp.to_le_bytes());
        record[16..24].copy_from_slice(&value.to_le_bytes());
        let crc = crc32fast::hash(&record[..24]);
        record[24..28].copy_from_slice(&crc.to_le_bytes());
        let path = self.directory.join(format!("{}.bin", web::encode(entity)));
        let mut file = config::private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if file.metadata()?.len() > self.config.max_points as u64 * RECORD as u64 {
            return Err(error(
                "History file exceeds configured retention; export it before reducing max_points",
            ));
        }
        file.seek(SeekFrom::Start(
            ((seq - 1) % self.config.max_points as u64) * RECORD as u64,
        ))?;
        file.write_all(&record)?;
        file.sync_data()?;
        self.sequences[index] = seq;
        Ok(())
    }
    pub fn query(&self, args: &Value) -> Result<Value> {
        if !valid_query(args)
            || args["start"]
                .as_i64()
                .zip(args["end"].as_i64())
                .is_some_and(|(a, b)| a > b)
        {
            return Err(error("Invalid bounded history query"));
        }
        let entity = args["entity"]
            .as_str()
            .filter(|e| self.config.entities.iter().any(|s| s == e))
            .ok_or_else(|| error("Unconfigured history entity"))?;
        let start = args["start"].as_i64().unwrap_or(0);
        let end = args["end"].as_i64().unwrap_or(i64::MAX);
        let aggregation = args["aggregation"].as_str().unwrap_or("raw");
        let limit = args["limit"].as_u64().unwrap_or(96) as usize;
        let mut raw = VecDeque::new();
        let mut buckets: BTreeMap<i64, (f64, f64, f64, u64)> = BTreeMap::new();
        let mut overflow = false;
        scan(
            &self.directory.join(format!("{}.bin", web::encode(entity))),
            self.config.max_points,
            |seq, time, value| {
                if time < start || time > end {
                    return;
                }
                if aggregation == "raw" {
                    raw.push_back((seq, time, value));
                    if raw.len() > limit {
                        let lowest = raw
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, p)| p.0)
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        raw.remove(lowest);
                    }
                } else {
                    let width = if aggregation == "hour" { 3600 } else { 86400 };
                    let key = time / width * width;
                    if !buckets.contains_key(&key) && buckets.len() >= limit {
                        overflow = true;
                        return;
                    }
                    let b = buckets.entry(key).or_insert((value, value, 0., 0));
                    b.0 = b.0.min(value);
                    b.1 = b.1.max(value);
                    b.3 += 1;
                    b.2 = b.2 * ((b.3 - 1) as f64 / b.3 as f64) + value / b.3 as f64;
                }
            },
        )?;
        if overflow {
            return Err(error(
                "History range exceeds bucket limit; narrow start/end",
            ));
        }
        let data: Vec<Value> = if aggregation == "raw" {
            let mut points: Vec<_> = raw.into_iter().collect();
            points.sort_by_key(|p| p.0);
            points
                .into_iter()
                .map(|(_, time, value)| json!({"timestamp":time,"value":value}))
                .collect()
        } else {
            buckets.into_iter().map(|(time,(min,max,sum,count))|json!({"timestamp":time,"min":min,"max":max,"mean":sum,"samples":count})).collect()
        };
        Ok(
            json!({"entity":entity,"aggregation":aggregation,"points":data,"retention_points":self.config.max_points,"note":"Observed samples only; missing periods are not interpolated. Daily counter max is observed usage, not a guaranteed midnight total."}),
        )
    }
}
fn valid_entity(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn scan(path: &Path, max: usize, mut visit: impl FnMut(u64, i64, f64)) -> Result<()> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > max as u64 * RECORD as u64 {
        return Err(error("History exceeds its configured bound"));
    }
    let mut record = [0u8; RECORD];
    loop {
        match file.read_exact(&mut record) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if record[24..28] != crc32fast::hash(&record[..24]).to_le_bytes() {
            continue;
        }
        let seq = u64::from_le_bytes(record[..8].try_into()?);
        let time = i64::from_le_bytes(record[8..16].try_into()?);
        let value = f64::from_le_bytes(record[16..24].try_into()?);
        if seq > 0 && time > 0 && value.is_finite() {
            visit(seq, time, value);
        }
    }
    Ok(())
}
impl Adapter for Manager {
    const TOOLS: &'static [Tool] = &[];
    const PUBLIC_TOOLS: &'static [Tool] = &[
        Tool {
            name: "history_series",
            description: "List recorded numeric entities and configured retention without reading devices",
            input: Input::Empty,
            hints: Hints::LIST,
        },
        Tool {
            name: "history_query",
            description: "Read bounded numeric history for a configured entity; raw samples or UTC hour/day min/max/mean. Missing periods stay missing.",
            input: Input::Object {
                schema: || json!({"type":"object","properties":{"entity":{"type":"string","maxLength":96},"start":{"type":"integer","minimum":0},"end":{"type":"integer","minimum":0},"aggregation":{"enum":["raw","hour","day"]},"limit":{"type":"integer","minimum":1,"maximum":96}},"required":["entity"],"additionalProperties":false}),
                valid: valid_query,
                error: "Expected entity, optional start/end, aggregation and limit 1–96",
            },
            hints: Hints::READ,
        },
    ];
    const SUPPORT: Support = Support::NONE;
    fn open(path: &Path) -> Result<Self> {
        Self::open(path)
    }
    fn entities<'a>(&'a self, _: &mut impl FnMut(Entity<'a>)) {}
    fn execute(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name == "history_series" {
            return Ok(
                json!({"entities":self.config.entities,"interval_seconds":self.config.interval_seconds,"max_points":self.config.max_points}),
            );
        }
        if name == "history_query" {
            self.query(args)
        } else {
            Err(error("Unknown history operation"))
        }
    }
}
fn valid_query(v: &Value) -> bool {
    v.as_object().is_some_and(|o| {
        o.get("entity")
            .and_then(Value::as_str)
            .is_some_and(valid_entity)
            && o.iter().all(|(k, v)| match k.as_str() {
                "entity" => true,
                "start" | "end" => v.as_u64().is_some_and(|n| n <= i64::MAX as u64),
                "aggregation" => v
                    .as_str()
                    .is_some_and(|s| matches!(s, "raw" | "hour" | "day")),
                "limit" => v.as_u64().is_some_and(|n| (1..=96).contains(&n)),
                _ => false,
            })
    })
}
