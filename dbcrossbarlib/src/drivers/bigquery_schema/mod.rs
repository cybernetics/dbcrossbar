//! Support for `bigquery-schema` locators.

use std::{fmt, str::FromStr};

use crate::common::*;
use crate::drivers::bigquery_shared::{BqColumn, BqTable, TableName, Usage};

/// A JSON file containing BigQuery table schema.
#[derive(Clone, Debug)]
pub struct BigQuerySchemaLocator {
    path: PathOrStdio,
}

impl fmt::Display for BigQuerySchemaLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.fmt_locator_helper(Self::scheme(), f)
    }
}

impl FromStr for BigQuerySchemaLocator {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let path = PathOrStdio::from_str_locator_helper(Self::scheme(), s)?;
        Ok(BigQuerySchemaLocator { path })
    }
}

impl Locator for BigQuerySchemaLocator {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self, _ctx: &Context) -> Result<Option<Table>> {
        // Read our input.
        let mut input = self.path.open_sync()?;
        let mut data = String::new();
        input
            .read_to_string(&mut data)
            .with_context(|_| format!("error reading {}", self.path))?;

        // Parse our input as a list of columns.
        let columns: Vec<BqColumn> = serde_json::from_str(&data)
            .with_context(|_| format!("error parsing {}", self.path))?;

        // Build a `BqTable`, convert it, and set a placeholder name.
        let arbitrary_name = TableName::from_str(&"unused:unused.unused")?;
        let bq_table = BqTable {
            name: arbitrary_name,
            columns,
        };
        let mut table = bq_table.to_table()?;
        table.name = "unnamed".to_owned();
        Ok(Some(table))
    }

    fn write_schema(
        &self,
        ctx: &Context,
        table: &Table,
        if_exists: IfExists,
    ) -> Result<()> {
        // The BigQuery table name doesn't matter here, because BigQuery won't
        // use it.
        let arbitrary_name = TableName::from_str(&"unused:unused.unused")?;

        // Generate our JSON.
        let mut f = self.path.create_sync(ctx, &if_exists)?;
        let bq_table = BqTable::for_table_name_and_columns(
            arbitrary_name,
            &table.columns,
            Usage::FinalTable,
        )?;
        bq_table.write_json_schema(&mut f)
    }
}

impl LocatorStatic for BigQuerySchemaLocator {
    fn scheme() -> &'static str {
        "bigquery-schema:"
    }

    fn features() -> Features {
        Features {
            locator: LocatorFeatures::SCHEMA | LocatorFeatures::WRITE_SCHEMA,
            write_schema_if_exists: IfExistsFeatures::no_append(),
            source_args: SourceArgumentsFeatures::empty(),
            dest_args: DestinationArgumentsFeatures::empty(),
            dest_if_exists: IfExistsFeatures::empty(),
            _placeholder: (),
        }
    }
}
