//! Support for Amazon's S3.

use std::{fmt, str::FromStr};

use crate::common::*;
use crate::drivers::redshift::RedshiftLocator;

mod local_data;
mod prepare_as_destination;
mod write_local_data;
mod write_remote_data;

use local_data::local_data_helper;
pub(crate) use prepare_as_destination::prepare_as_destination_helper;
use write_local_data::write_local_data_helper;
use write_remote_data::write_remote_data_helper;

#[derive(Clone, Debug)]
pub(crate) struct S3Locator {
    url: Url,
}

impl S3Locator {
    /// Access the `s3://` URL in this locator.
    pub(crate) fn as_url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Display for S3Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.url.fmt(f)
    }
}

impl FromStr for S3Locator {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.starts_with(Self::scheme()) {
            let url = s
                .parse::<Url>()
                .with_context(|_| format!("cannot parse {}", s))?;
            if !url.path().starts_with('/') {
                Err(format_err!("{} must start with s3://", url))
            } else if !url.path().ends_with('/') {
                Err(format_err!("{} must end with a '/'", url))
            } else {
                Ok(S3Locator { url })
            }
        } else {
            Err(format_err!("expected {} to begin with s3://", s))
        }
    }
}

impl Locator for S3Locator {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn local_data(
        &self,
        ctx: Context,
        shared_args: SharedArguments<Unverified>,
        source_args: SourceArguments<Unverified>,
    ) -> BoxFuture<Option<BoxStream<CsvStream>>> {
        local_data_helper(ctx, self.url.clone(), shared_args, source_args).boxed()
    }

    fn write_local_data(
        &self,
        ctx: Context,
        data: BoxStream<CsvStream>,
        shared_args: SharedArguments<Unverified>,
        dest_args: DestinationArguments<Unverified>,
    ) -> BoxFuture<BoxStream<BoxFuture<()>>> {
        write_local_data_helper(ctx, self.url.clone(), data, shared_args, dest_args)
            .boxed()
    }

    fn supports_write_remote_data(&self, source: &dyn Locator) -> bool {
        // We can only do `write_remote_data` if `source` is a
        // `RedshiftLocator`. Otherwise, we need to do `write_local_data` like
        // normal.
        source.as_any().is::<RedshiftLocator>()
    }

    fn write_remote_data(
        &self,
        ctx: Context,
        source: BoxLocator,
        shared_args: SharedArguments<Unverified>,
        source_args: SourceArguments<Unverified>,
        dest_args: DestinationArguments<Unverified>,
    ) -> BoxFuture<()> {
        write_remote_data_helper(
            ctx,
            source,
            self.to_owned(),
            shared_args,
            source_args,
            dest_args,
        )
        .boxed()
    }
}

impl LocatorStatic for S3Locator {
    fn scheme() -> &'static str {
        "s3:"
    }

    fn features() -> Features {
        Features {
            locator: LocatorFeatures::LOCAL_DATA | LocatorFeatures::WRITE_LOCAL_DATA,
            write_schema_if_exists: IfExistsFeatures::empty(),
            source_args: SourceArgumentsFeatures::empty(),
            dest_args: DestinationArgumentsFeatures::empty(),
            dest_if_exists: IfExistsFeatures::OVERWRITE,
            _placeholder: (),
        }
    }
}
