//! Driver for working with CSV files.

use csv;
use std::{ffi::OsStr, fmt, io::BufReader, path::PathBuf, str::FromStr};
use tokio::{fs, io};
use walkdir::WalkDir;

use crate::common::*;
use crate::concat::concatenate_csv_streams;
use crate::csv_stream::csv_stream_name;
use crate::schema::{Column, DataType, Table};
use crate::tokio_glue::{copy_reader_to_stream, copy_stream_to_writer};

/// Locator scheme for CSV files.
pub(crate) const CSV_SCHEME: &str = "csv:";

/// (Incomplete.) A CSV file containing data, or a directory containing CSV
/// files.
///
/// TODO: Right now, we take a file path as input and a directory path as
/// output, because we're lazy and haven't finished building this.
#[derive(Debug)]
pub(crate) struct CsvLocator {
    path: PathOrStdio,
}

impl fmt::Display for CsvLocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.fmt_locator_helper(CSV_SCHEME, f)
    }
}

impl FromStr for CsvLocator {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let path = PathOrStdio::from_str_locator_helper(CSV_SCHEME, s)?;
        Ok(CsvLocator { path })
    }
}

impl Locator for CsvLocator {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self, _ctx: &Context) -> Result<Option<Table>> {
        match &self.path {
            PathOrStdio::Stdio => {
                // This is actually fairly tricky, because we may need to first
                // read the columns from stdin, _then_ start re-reading from the
                // beginning to read the data when `local_data` is called.
                Err(format_err!("cannot yet read CSV schema from stdin"))
            }
            PathOrStdio::Path(path) => {
                // Build our columns.
                let mut rdr = csv::Reader::from_path(path)
                    .with_context(|_| format!("error opening {}", path.display()))?;
                let mut columns = vec![];
                let headers = rdr
                    .headers()
                    .with_context(|_| format!("error reading {}", path.display()))?;
                for col_name in headers {
                    columns.push(Column {
                        name: col_name.to_owned(),
                        is_nullable: true,
                        data_type: DataType::Text,
                        comment: None,
                    })
                }

                // Build our table.
                let name = path
                    .file_stem()
                    .unwrap_or_else(|| OsStr::new("data"))
                    .to_string_lossy()
                    .into_owned();
                Ok(Some(Table { name, columns }))
            }
        }
    }

    fn local_data(
        &self,
        ctx: Context,
        _schema: Table,
        query: Query,
        _temporary_storage: TemporaryStorage,
        args: DriverArgs,
    ) -> BoxFuture<Option<BoxStream<CsvStream>>> {
        local_data_helper(ctx, self.path.clone(), query, args).boxed()
    }

    fn write_local_data(
        &self,
        ctx: Context,
        schema: Table,
        data: BoxStream<CsvStream>,
        _temporary_storage: TemporaryStorage,
        args: DriverArgs,
        if_exists: IfExists,
    ) -> BoxFuture<BoxStream<BoxFuture<()>>> {
        write_local_data_helper(ctx, self.path.clone(), schema, data, args, if_exists)
            .boxed()
    }
}

async fn local_data_helper(
    ctx: Context,
    path: PathOrStdio,
    query: Query,
    args: DriverArgs,
) -> Result<Option<BoxStream<CsvStream>>> {
    query.fail_if_query_details_provided()?;
    args.fail_if_present()?;
    match path {
        PathOrStdio::Stdio => {
            let data = BufReader::with_capacity(BUFFER_SIZE, io::stdin());
            let stream = copy_reader_to_stream(ctx, data)?;
            let csv_stream = CsvStream {
                name: "data".to_owned(),
                data: Box::new(
                    stream.map_err(move |e| format_err!("cannot read stdin: {}", e)),
                ),
            };
            Ok(Some(box_stream_once(Ok(csv_stream))))
        }
        PathOrStdio::Path(base_path) => {
            // Recursively look at our paths, picking out the ones that look
            // like CSVs. We do this synchronously because it's reasonably
            // fast and we'd like to catch errors up front.
            let mut paths = vec![];
            debug!(ctx.log(), "walking {}", base_path.display());
            let walker = WalkDir::new(&base_path).follow_links(true);
            for dirent in walker.into_iter() {
                let dirent = dirent.with_context(|_| {
                    format!("error listing files in {}", base_path.display())
                })?;
                let p = dirent.path();
                trace!(ctx.log(), "found dirent {}", p.display());
                if dirent.file_type().is_dir() {
                    continue;
                } else if !dirent.file_type().is_file() {
                    return Err(format_err!("not a file: {}", p.display()));
                }

                let ext = p.extension();
                if ext == Some(OsStr::new("csv")) || ext == Some(OsStr::new("CSV")) {
                    paths.push(p.to_owned());
                } else {
                    return Err(format_err!(
                        "{} must end in *.csv or *.CSV",
                        p.display()
                    ));
                }
            }

            let csv_streams = stream::iter_ok(paths).and_then(move |file_path| {
                let ctx = ctx.clone();
                let base_path = base_path.clone();
                async move {
                    // Get the name of our stream.
                    let name = csv_stream_name(
                        &base_path.to_string_lossy(),
                        &file_path.to_string_lossy(),
                    )?
                    .to_owned();
                    let ctx = ctx.child(o!(
                        "stream" => name.clone(),
                        "path" => format!("{}", file_path.display())
                    ));

                    // Open our file.
                    let data = fs::File::open(file_path.clone())
                        .compat()
                        .await
                        .with_context(|_| {
                            format!("cannot open {}", file_path.display())
                        })?;
                    let data = BufReader::with_capacity(BUFFER_SIZE, data);
                    let stream = copy_reader_to_stream(ctx, data)?;

                    Ok(CsvStream {
                        name,
                        data: Box::new(stream.map_err(move |e| {
                            format_err!("cannot read {}: {}", file_path.display(), e)
                        })),
                    })
                }
                    .boxed()
                    .compat()
            });

            Ok(Some(Box::new(csv_streams) as BoxStream<CsvStream>))
        }
    }
}

async fn write_local_data_helper(
    ctx: Context,
    path: PathOrStdio,
    _schema: Table,
    data: BoxStream<CsvStream>,
    args: DriverArgs,
    if_exists: IfExists,
) -> Result<BoxStream<BoxFuture<()>>> {
    args.fail_if_present()?;
    match path {
        PathOrStdio::Stdio => {
            if_exists.warn_if_not_default_for_stdout(&ctx);
            let stream = concatenate_csv_streams(ctx.clone(), data)?;
            let fut = async move {
                copy_stream_to_writer(ctx.clone(), stream.data, io::stdout())
                    .await
                    .context("error writing to stdout")?;
                Ok(())
            };
            Ok(box_stream_once(Ok(fut.boxed())))
        }
        PathOrStdio::Path(path) => {
            if path.to_string_lossy().ends_with('/') {
                // Write streams to our directory as multiple files.
                let result_stream = data.map(move |stream| {
                    let path = path.clone();
                    let ctx = ctx.clone();
                    let if_exists = if_exists.clone();

                    async move {
                        // TODO: This join does not handle `..` or nested `/` in
                        // a particularly safe fashion.
                        let csv_path = path.join(&format!("{}.csv", stream.name));
                        let ctx = ctx.child(o!(
                            "stream" => stream.name.clone(),
                            "path" => format!("{}", csv_path.display()),
                        ));
                        write_stream_to_file(ctx, stream.data, csv_path, if_exists)
                            .await
                    }
                        .boxed()
                });
                Ok(Box::new(result_stream) as BoxStream<BoxFuture<()>>)
            } else {
                // Write all our streams as a single file.
                let stream = concatenate_csv_streams(ctx.clone(), data)?;
                let fut = async move {
                    let ctx = ctx.child(o!(
                        "stream" => stream.name.clone(),
                        "path" => format!("{}", path.display()),
                    ));
                    write_stream_to_file(ctx, stream.data, path, if_exists).await
                };
                Ok(box_stream_once(Ok(fut.boxed())))
            }
        }
    }
}

/// Write `data` to `dest`, honoring `if_exists`.
async fn write_stream_to_file(
    ctx: Context,
    data: BoxStream<BytesMut>,
    dest: PathBuf,
    if_exists: IfExists,
) -> Result<()> {
    // Make sure our destination directory exists.
    let dir = dest
        .parent()
        .ok_or_else(|| format_err!("cannot find parent dir for {}", dest.display()))?;
    fs::create_dir_all(dir)
        .compat()
        .await
        .with_context(|_| format!("unable to create directory {}", dir.display()))?;

    // Write our our CSV stream.
    debug!(ctx.log(), "writing stream to file {}", dest.display());
    let wtr = if_exists
        .to_async_open_options_no_append()?
        .open(dest.clone())
        .compat()
        .await
        .with_context(|_| format!("cannot open {}", dest.display()))?;
    copy_stream_to_writer(ctx.clone(), data, wtr)
        .await
        .with_context(|_| format!("error writing {}", dest.display()))?;
    Ok(())
}
