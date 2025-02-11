/*! Rebuild file writer/schema
Each (avro) record is an  `(shard_id, array of (shard) records)`.
!*/

use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use avro_rs::{AvroResult, Codec, Schema, Writer};
use log::error;
use serde::Deserialize;
use serde::Serialize;
use structopt::lazy_static::lazy_static;

use crate::lang::LANG;
use crate::{error::Error, lang::Lang};

use super::{Location, Metadata};

lazy_static! {
    static ref SCHEMA: Schema = {

      // schema of Identification struct
        let identification_schema = r#"
      {"name":"identification", "type":"record", "fields": [
        {"name": "label", "type":"string"},
        {"name": "prob", "type":"float"}
      ]}
"#;
      // schema of Metadata struct
        let metadata_schema = r#"
{
  "type":"record",
  "name":"metadata_record",
  "fields":[
    {"name":"identification", "type":"identification"},
    {"name":"annotation", "type":["null", {"type": "array", "items":"string"}]},
    {"name": "sentence_identifications", "type":"array", "items":[
      "null",
      "identification"
    ]}
  ]
}
"#;
  // schema of RebuildInformation struct
        let rebuild_schema = r#"
{
  "type":"record",
  "name":"rebuild_information",
  "fields":[
    {"name": "shard_id", "type":"long"},
    {"name": "record_id", "type":"string"},
    {"name": "line_start", "type":"long"},
    {"name": "line_end", "type":"long"},
    {"name": "loc_in_shard", "type":"long"},
    {"name":"metadata", "type":"metadata_record"}
  ]
}
"#;
  // schema of ShardResult struct
        let schema = r#"
{
  "type":"record",
  "name":"shard_result",
  "fields":[
    {"name": "shard_id", "type":"long"},
    {"name": "rebuild_info", "type":"array", "items":"rebuild_information"}
  ]
}
"#;

        Schema::parse_list(&[
            identification_schema,
            metadata_schema,
            rebuild_schema,
            schema,
        ])
        .unwrap()[3]
            .clone()
    };
}

/// Holds the same fields as [Location], adding [Metadata].
///
/// Should be transformed into a struct that holds two attributes rather than copying some.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct RebuildInformation {
    shard_id: usize,
    record_id: String,
    line_start: usize,
    line_end: usize,
    loc_in_shard: usize,
    metadata: Metadata,
}

impl RebuildInformation {
    pub fn new(location: Location, metadata: Metadata) -> Self {
        Self {
            shard_id: location.shard_id(),
            // TODO: Useless borrow here.
            record_id: location.record_id().to_owned(),
            line_start: location.line_start(),
            line_end: location.line_end(),
            loc_in_shard: location.loc_in_shard(),
            metadata,
        }
    }

    /// Convert into a ([Location], [Metadata]) tuple.
    pub fn into_raw_parts(self) -> (Location, Metadata) {
        (
            Location::new(
                self.shard_id,
                self.record_id,
                self.line_start,
                self.line_end,
                self.loc_in_shard,
            ),
            self.metadata,
        )
    }
    /// Get a reference to the rebuild information's loc in shard.
    pub fn loc_in_shard(&self) -> usize {
        self.loc_in_shard
    }

    /// Get a reference to the rebuild information's record id.
    pub fn record_id(&self) -> &str {
        self.record_id.as_ref()
    }

    /// Get a reference to the rebuild information's line start.
    pub fn line_start(&self) -> usize {
        self.line_start
    }

    /// Get a reference to the rebuild information's line end.
    pub fn line_end(&self) -> usize {
        self.line_end
    }

    /// Get a reference to the rebuild information's metadata.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Get a reference to the rebuild information's shard id.
    pub fn shard_id(&self) -> usize {
        self.shard_id
    }
}

/// Holds multiple [RebuildInformation] for a single shard.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct ShardResult {
    shard_id: i64,
    rebuild_info: Vec<RebuildInformation>,
}

impl ShardResult {
    /// Merges `locations` and `metadata` into [RebuildInformation].
    pub fn new(shard_id: i64, locations: Vec<Location>, metadata: Vec<Metadata>) -> Self {
        let rebuild_info = locations
            .into_iter()
            .zip(metadata.into_iter())
            .map(|(loc, meta)| RebuildInformation::new(loc, meta))
            .collect();
        Self {
            shard_id,
            rebuild_info,
        }
    }

    /// extract owned parts of struct: (`shard_id`, `Vec<RebuildInformation>`)
    pub fn into_raw_parts(self) -> (i64, Vec<RebuildInformation>) {
        (self.shard_id, self.rebuild_info)
    }
    /// Get a reference to the shard result's shard id.
    pub fn shard_id(&self) -> i64 {
        self.shard_id
    }

    /// Get a reference to the shard result's rebuild info.
    pub fn rebuild_info(&self) -> &[RebuildInformation] {
        self.rebuild_info.as_ref()
    }
}
/// Holds an Avro writer.
pub struct RebuildWriter<'a, T> {
    schema: &'a Schema,
    writer: Writer<'a, T>,
}

impl<'a, T: std::io::Write> RebuildWriter<'a, T> {
    /// Create a new rebuilder.
    pub fn new(schema: &'a Schema, writer: T) -> Self {
        Self {
            schema,
            writer: Writer::with_codec(schema, writer, Codec::Snappy),
        }
    }

    /// Append a single serializable value (`value` must implement [Serialize]).
    ///
    /// This function is not guaranteed to perform a write operation
    /// See documentation of [avro_rs::Writer] for more information.
    pub fn append_ser<S: Serialize>(&mut self, value: S) -> AvroResult<usize> {
        self.writer.append_ser(value)
    }

    /// Append from an interator of values, each implementing [Serialize].
    ///
    /// This function is not guaranteed to perform a write operation
    /// See documentation of [avro_rs::Writer] for more information.
    pub fn extend_ser<I, U: Serialize>(&mut self, values: I) -> AvroResult<usize>
    where
        I: IntoIterator<Item = U>,
    {
        self.writer.extend_ser(values)
    }

    /// Flush the underlying buffer.
    ///
    /// See [avro_rs::Writer] for more information.
    pub fn flush(&mut self) -> AvroResult<usize> {
        self.writer.flush()
    }
}

impl<'a> RebuildWriter<'a, File> {
    /// Create a writer on `dst` file.
    /// Errors if provided path already exists.
    pub fn from_path(dst: &Path) -> Result<Self, Error> {
        let schema = &SCHEMA;
        let dest_file = File::create(dst)?;
        Ok(Self::new(schema, dest_file))
    }
}

/// Holds mutex-protected [RebuildWriter] for each [Lang].
pub struct RebuildWriters<'a, T>(HashMap<Lang, Arc<Mutex<RebuildWriter<'a, T>>>>);

impl<'a, T> RebuildWriters<'a, T> {
    /// Maps to [HashMap::get].
    pub fn get(&'a self, k: &Lang) -> Option<&Arc<Mutex<RebuildWriter<T>>>> {
        self.0.get(k)
    }
}

impl<'a> RebuildWriters<'a, File> {
    #[inline]
    fn forge_dst(dst: &Path, lang: &Lang) -> PathBuf {
        let mut p = PathBuf::from(dst);
        p.push(format!("{}.avro", lang));

        p
    }

    #[inline]
    /// Convinience function that creates a new ([Lang], `Arc<Mutex<RebuildWriter>>`]) pair.
    fn new_writer_mutex(
        dst: &Path,
        lang: &str,
    ) -> Result<(Lang, Arc<Mutex<RebuildWriter<'a, File>>>), Error> {
        let lang = Lang::from_str(lang).unwrap();
        let path = Self::forge_dst(dst, &lang);
        let rw = RebuildWriter::from_path(&path)?;
        let rw_mutex = Arc::new(Mutex::new(rw));
        Ok((lang, rw_mutex))
    }

    /// Use `dst` as a root path for avro files storage.
    ///
    /// Each language will have a possibly empty avro file, at `<dst>/<lang>.avro`.
    pub fn with_dst(dst: &Path) -> Result<Self, Error> {
        if !dst.exists() {
            std::fs::create_dir(dst)?;
        }
        if dst.is_file() {
            error!("rebuild destination must be an empty folder!");
        };

        if !dst.read_dir()?.next().is_none() {
            error!("rebuild destination folder must be empty!");
        }

        let ret: Result<HashMap<Lang, Arc<Mutex<RebuildWriter<'_, File>>>>, Error> = LANG
            .iter()
            .map(|lang| Self::new_writer_mutex(dst, lang))
            .collect();

        Ok(RebuildWriters(ret?))
    }
}

#[cfg(test)]
mod tests {

    use crate::pipelines::oscardoc::types::{Location, Metadata};

    use super::{RebuildInformation, RebuildWriter, ShardResult};

    #[test]
    fn rebuild_information_into_raw_parts() {
        let loc = Location::default();
        let m = Metadata::default();
        let ri = RebuildInformation::new(loc.clone(), m.clone());
        let (loc2, m2) = ri.into_raw_parts();

        assert_eq!(loc, loc2);
        assert_eq!(m, m2);
    }
    #[test]
    fn test_ser_empty() {
        let sr = ShardResult::new(0, Vec::new(), Vec::new());
        println!("{:#?}", sr);
        let buf = Vec::new();
        let mut rw = RebuildWriter::new(&super::SCHEMA, buf);

        rw.append_ser(sr).unwrap();
    }

    #[test]
    fn test_ser() {
        let meta = vec![Metadata::default()];
        let loc = vec![Location::default()];
        let sr = ShardResult::new(0, loc, meta);
        println!("{:#?}", sr);
        println!("{:#?}", *super::SCHEMA);
        let mut buf = Vec::new();
        let mut rw = RebuildWriter::new(&super::SCHEMA, &mut buf);

        rw.append_ser(&sr).unwrap();
        rw.flush().unwrap();

        let ar = avro_rs::Reader::with_schema(&super::SCHEMA, &buf[..]).unwrap();
        let result: Vec<ShardResult> = ar
            .map(|r| avro_rs::from_value::<ShardResult>(&r.unwrap()).unwrap())
            .collect();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], sr);
    }
}
