use std::{collections::HashMap, path::PathBuf};

use super::types::Document;
use super::types::MergedPiece;
use crate::error::Error;
use crate::identifiers::FastText;
use crate::io::writer::WriterTrait;
use crate::lang::LANG;
use crate::sources::commoncrawl::Wet;
use log::Level::Debug;
use log::{debug, error, info, log_enabled, warn};
use rayon::prelude::*;
use warc::BufferedBody;
use warc::Record;

use crate::io::LangFiles;

use crate::pipelines::pipeline::Pipeline;

use super::types::WarcHeaders;
/// OSCAR v1.5 generation pipeline
///
/// OSCAR v1.5 is a retrocompatible corpus
/// enhanced with metadata coming from CommonCrawl.
///
/// The CommonCrawl dump is composed of shards,
/// Each shard is composed of records,
/// Each record is composed of a metadata header and a body containing sentences.
///
/// # Processing
/// _every scope is concurrent, that means green threads are created on shards, records and sentences._
/// - We process each record separately, getting a list of sentence-language pairs, along with metadata from the document.
/// - Once we've treated each record of a given shard, we
///   transform out list of sentence-language pairs into chunks of contiguous same-language sentences
///   and we store shard-level line offsets on metadata.
///   Then we group same-language chunks for each language (on shard-level) and we write on disk.
/// - We also keep track of disk-level line offsets to sync shard-level offsets between writes.
///
/// TODO: Better document this step.
pub struct OscarMetadata {
    src: PathBuf,
    dst: PathBuf,
    lid_path: PathBuf,
}

impl OscarMetadata {
    pub fn new(src: PathBuf, dst: PathBuf, lid_path: PathBuf) -> Self {
        Self { src, dst, lid_path }
    }

    /// attempt to predict language on provided sentence.
    ///
    /// Returns [None] if no language is detected.
    // why return the sentence itself?
    // TODO: change return type to Option<&'static str>.
    fn identify_sentence(sentence: &str, cls: &FastText) -> Option<(String, &'static str)> {
        let prediction = cls.predict(sentence).ok();

        if let Some(Some(lang)) = prediction {
            //TODO: rewrite these two lines more elegantly
            //      we can unwrap since predict returns None if no predictions are
            //      found
            let lang = lang.get(0).unwrap();

            // check if fasttext provided lang exists
            // return None if not
            match LANG.get(lang.label.as_str()) {
                Some(lang) => Some((sentence.to_string(), *lang)),
                None => {
                    warn!("lang {} does not exist!", lang.label);
                    None
                }
            }
        } else {
            None
        }
    }

    /// Process a provided record.
    ///
    /// Here, sentences that are >100 chars are processed,
    /// and the others are discarded.
    /// See [String::chars::count].
    ///
    /// Then, we identify language for each sentence
    /// and return (sentence, language) along with headers
    /// extracted from the WARC.
    fn process_record(
        record: Record<BufferedBody>,
        cls: &FastText,
    ) -> Option<(Vec<(String, &'static str)>, WarcHeaders)> {
        if log_enabled!(Debug) {
            debug!("processing record {}", record.warc_id());
        };
        let body = String::from_utf8(record.body().to_vec()).ok();

        // process record if body is utf8-valid
        if let Some(sentences) = body {
            // filter out lines that does not contain 100 characters.
            // then convert into a parallel iterator
            let sentences = sentences
                .lines()
                .filter(|line| line.chars().count() > 100)
                .par_bridge();

            let results: Vec<(String, &'static str)> = sentences
                // predict for each sentence, discarding
                // predictions that does not meet threshold
                .filter_map(|sentence| Self::identify_sentence(sentence, cls))
                .collect();

            Some((results, record.into_raw_parts().0.headers))
        } else {
            error!("body not UTF-8 valid: {:?}", record.warc_id());
            None
        }
    }
}

impl Pipeline<()> for OscarMetadata {
    fn version() -> &'static str {
        "1.1.0"
    }

    /// Run the whole pipeline
    fn run(&self) -> Result<(), Error> {
        // let errors;

        let cls = FastText::new(&self.lid_path, 1, 0.8)?;

        // list files in source folder,
        // filter out errors from fs and from gzip/wet.
        // This means that invalid gz files and invalid
        // wet files are discarded silently
        let results = std::fs::read_dir(&self.src)?
            .filter_map(|shard| {
                shard.map_or_else(
                    |e| {
                        error!("error reading shard directory: {}", e);
                        None
                    },
                    Some,
                )
            })
            .map(|shard| shard.path());

        // convert to parallel iterator
        // /!\: We use par_bridge, that is suboptimal
        //      compared to implementing IntoParallelIterator
        //      ourselves.
        let results = results.enumerate().par_bridge();

        // holds file handles
        // let langfiles = match self.part_size {
        //     Some(ps) => LangFiles::new(&self.dst, Some(ps * 1_000_000))?,
        //     None => LangFiles::new(&self.dst, None)?,
        // };

        let langfiles = LangFiles::new(&self.dst, None)?;

        // iterate over shards
        let r: Vec<Error> = results
            .filter_map(|(idx, shard)| {
                // holds merged pieces by lang
                let mut lang_pieces: HashMap<&'static str, Vec<MergedPiece>> = HashMap::new();

                // get an atomic reference to global offsets
                // let offsets_global_arc = offsets_global.clone();
                info!("processing shard {}: {:?}", idx, &shard);

                let shard = Wet::from_path_gzip(&shard);

                if shard.is_err() {
                    error!("Could not read/open shard {}", idx);
                    return shard.err();
                }

                let shard = shard.unwrap();
                // convert into a parallel iterator
                let wetfile = shard.iter.enumerate().par_bridge();

                let shard_results: Vec<(Vec<(String, &'static str)>, WarcHeaders)> = wetfile
                    .filter_map(|(idx_record, record)| match record {
                        Ok(record) => OscarMetadata::process_record(record, &cls),
                        Err(e) => {
                            warn!("Error on record {} of shard {}: {:?}", idx_record, idx, e);
                            None
                        }
                    })
                    // collect here is blocking
                    // because we can't write concurrently into a HashMap
                    // and using Mutexes might ruin performance.
                    .collect(); //TODO: test with a for_each and a channel to send?

                // Iterate over (record, header) tuples
                let shard_results = shard_results.into_iter().filter_map(|(record, header)| {
                    // split between langs and sentences
                    let langs: Vec<&str> = record.iter().map(|(_, lang)| *lang).collect();
                    let sentences: Vec<String> =
                        record.into_iter().map(|(sentences, _)| sentences).collect();

                    // create new document for current record
                    let doc = Document::new(header, sentences, langs);

                    match doc {
                        Ok(doc) => Some(doc),
                        Err(e) => {
                            warn!("{:?}", e);
                            None
                        }
                    }
                });

                // merge all documents together
                // get a vector of merged pieces of difference languages
                let docs_merged = shard_results
                    .map(|doc| doc.into_merged_pieces_lang())
                    .flatten()
                    .collect::<Vec<MergedPiece>>();

                // sort merged pieces into different langs
                // now there's a hashmap that points each lang
                // to a vector of merged pieces
                for piece in docs_merged {
                    let e = lang_pieces
                        .entry(piece.identification())
                        .or_insert_with(Vec::new);
                    e.push(piece);
                }

                // write concurrently
                lang_pieces.into_par_iter().for_each(|(lang, pieces)| {
                    let writer = langfiles.writers().get(lang).unwrap();
                    let mut writer_lock = writer.lock().unwrap();
                    writer_lock.write(pieces).unwrap();
                });

                None
            })
            .collect();

        // fix trailing comma
        // langfiles.close_meta()?;

        for err in r {
            error!("{:?}", err);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use warc::{EmptyBody, Record};

    use crate::identifiers::FastText;

    use super::OscarMetadata;
    #[test]
    fn test_process_record() {
        let cls = FastText::new_lid().unwrap();

        // let oscar_metadata =
        //     OscarMetadata::new(temp_dir(), temp_dir(), PathBuf::from("lid.176.bin"));

        let record: Record<EmptyBody> = Record::default();
        let body = "english test that is longer than one hundred characters. english test that is longer than one hundred characters.
phrase française de plus de cent caractères. Ceci est une phrase française de plus de cent caractères.";
        println!("{}", body.len());
        let record = record.add_body(body);
        let (identifications, _) = OscarMetadata::process_record(record, &cls).unwrap();

        for (sentence, id) in identifications {
            if id == "en" {
                assert_eq!(sentence, "english test that is longer than one hundred characters. english test that is longer than one hundred characters.");
            } else if id == "fr" {
                assert_eq!(sentence, "phrase française de plus de cent caractères. Ceci est une phrase française de plus de cent caractères.");
            }
        }
    }
}
