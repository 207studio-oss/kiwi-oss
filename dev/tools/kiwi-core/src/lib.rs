use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub struct RuleResource {
    pub id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub path: String,
}

pub fn rule_resources(db: &Path) -> Result<Vec<RuleResource>, String> {
    let connection = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare("SELECT id, title, summary, path FROM nodes ORDER BY id")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(RuleResource {
                id: row.get(0)?,
                title: row.get(1)?,
                summary: row.get(2)?,
                path: row.get(3)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}
