//! A sample library for testing the code adapter.

/// A simple record type.
pub struct Record {
    pub id: u64,
    pub value: String,
}

/// Process a record and return a result.
pub fn process_record(rec: &Record) -> Result<String, String> {
    if rec.id == 0 {
        return Err("zero id".to_string());
    }
    Ok(format!("processed:{}", rec.value))
}

/// Validate a record's fields.
pub fn validate_record(rec: &Record) -> bool {
    !rec.value.is_empty() && rec.id > 0
}
