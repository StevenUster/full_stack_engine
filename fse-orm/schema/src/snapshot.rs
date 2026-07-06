//! The snapshot file (`.fse/schema.json`): the schema state the last
//! generated migration brought the database to. Committed to git so
//! migration generation is deterministic and reviewable.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::model::Schema;

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct SnapshotFile {
    version: u32,
    schema: Schema,
}

pub fn schema_to_json(schema: &Schema) -> String {
    let file = SnapshotFile {
        version: SNAPSHOT_VERSION,
        schema: schema.clone(),
    };
    let mut json = serde_json::to_string_pretty(&file).expect("schema serializes");
    json.push('\n');
    json
}

pub fn schema_from_json(json: &str) -> Result<Schema, Error> {
    let file: SnapshotFile = serde_json::from_str(json)
        .map_err(|e| Error::new(format!("invalid schema snapshot: {e}")))?;
    if file.version != SNAPSHOT_VERSION {
        return Err(Error::new(format!(
            "schema snapshot version {} is not supported (expected {SNAPSHOT_VERSION})",
            file.version
        )));
    }
    Ok(file.schema)
}
