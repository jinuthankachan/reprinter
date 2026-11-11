//! Printing: printers and their options, uploads, print jobs, the queue.
//! Open to everyone who can reach the page, like the printer itself.
//! (Anyone can cancel any job, so canceling several at once is open too.)

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path as UrlPath, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{
    AppState,
    cups::{self, Job, PpdOption, Printer},
    error::{AppError, Result},
    uploads::{self, Kind, Upload},
};

/// The queues, with the unwanted ones flagged (`cups::flag_unwanted`).
pub async fn printers(State(st): State<Arc<AppState>>) -> Result<Json<Vec<Printer>>> {
    let mut list = cups::printers(&st.ipp).await?;
    cups::flag_unwanted(&mut list, &st.driver_schemes, &st.hostname);
    Ok(Json(list))
}

pub async fn options(
    State(st): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Vec<PpdOption>>> {
    let printer = cups::find_printer(&st.ipp, &name).await?;
    Ok(Json(cups::ppd_options(&printer.name).await?))
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

const SNIFF_BYTES: usize = 8192;

pub async fn upload(State(st): State<Arc<AppState>>, mut form: Multipart) -> Result<Json<Upload>> {
    let config = st.config();
    let limit = config.max_upload_mb * 1024 * 1024;
    let storage = limit * uploads::STORAGE_FACTOR;
    let broken = |e: axum::extract::multipart::MultipartError| {
        AppError::bad_request(format!("upload failed: {e}"))
    };
    let full = || {
        AppError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "the server holds too many uploaded files right now; remove some or try again \
                 later (unused files are deleted after {} minutes)",
                config.upload_ttl_minutes
            ),
        )
    };
    while let Some(mut field) = form.next_field().await.map_err(broken)? {
        if field.name() != Some("file") {
            continue;
        }
        let name = uploads::clean_name(field.file_name().unwrap_or("document"));
        // Until `finish`, any early return below deletes the file again.
        let mut incoming = st.uploads.receive().ok_or_else(full)?;
        let mut file = tokio::fs::File::create(&incoming.path).await?;
        let (mut size, mut head) = (0u64, Vec::with_capacity(SNIFF_BYTES));
        while let Some(chunk) = field.chunk().await.map_err(broken)? {
            size += chunk.len() as u64;
            if size > limit {
                return Err(AppError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!(
                        "the file is larger than the {} MB upload limit",
                        limit / 1024 / 1024
                    ),
                ));
            }
            if !incoming.reserve(chunk.len() as u64, storage) {
                return Err(full());
            }
            if head.len() < SNIFF_BYTES {
                let n = (SNIFF_BYTES - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..n]);
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        if size == 0 {
            return Err(AppError::bad_request("the file is empty"));
        }
        let (kind, mime) = uploads::sniff(&head).ok_or_else(|| {
            AppError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "{name}: this file type cannot be printed. Use PDF, PostScript, JPEG, PNG, GIF, TIFF \
                     or plain text (save other documents as PDF first)."
                ),
            )
        })?;
        return Ok(Json(incoming.finish(name, kind, mime)));
    }
    Err(AppError::bad_request("the upload contained no file"))
}

fn expired() -> AppError {
    AppError::not_found("the uploaded file has expired; upload it again")
}

/// The uploaded file itself, for the preview.
pub async fn upload_file(
    State(st): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response> {
    let upload = st.uploads.get(&id).ok_or_else(expired)?;
    let file = tokio::fs::File::open(st.uploads.path(&upload.id))
        .await
        .map_err(|_| expired())?;
    let content_type = match upload.kind {
        Kind::Text => "text/plain; charset=utf-8",
        _ => upload.mime,
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, upload.size.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "inline; filename*=UTF-8''{}",
                    uploads::pct_encode(&upload.name)
                ),
            ),
            (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

pub async fn upload_delete(
    State(st): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> StatusCode {
    st.uploads.remove(&id);
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageSet {
    #[default]
    All,
    Odd,
    Even,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    #[default]
    Auto,
    Portrait,
    Landscape,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scaling {
    #[default]
    Auto,
    /// Shrink or enlarge to fit the page.
    Fit,
    /// Fill the page, cropping what sticks out.
    Fill,
    /// Actual size.
    None,
}

fn one() -> u32 {
    1
}

/// The page setup for one job. Standard settings have their own fields;
/// `options` holds the printer's own (PPD) options such as PageSize, Duplex
/// or toner density, checked against what the printer offers.
#[derive(Debug, Deserialize)]
pub struct PrintRequest {
    pub upload_id: String,
    pub printer: String,
    #[serde(default = "one")]
    pub copies: u32,
    #[serde(default)]
    pub collate: bool,
    /// "1-3,5"; empty for all pages
    #[serde(default)]
    pub page_ranges: String,
    #[serde(default)]
    pub page_set: PageSet,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default)]
    pub scaling: Scaling,
    #[serde(default = "one")]
    pub number_up: u32,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub title: String,
}

/// Builds the `lp` command line for a validated request. The document itself
/// goes to lp on standard input (see `cups::submit`).
pub fn lp_args(
    req: &PrintRequest,
    title: &str,
    mime: &str,
    ppd: &[PpdOption],
) -> Result<Vec<String>> {
    if !(1..=999).contains(&req.copies) {
        return Err(AppError::bad_request("copies must be between 1 and 999"));
    }
    if ![1, 2, 4, 6, 9, 16].contains(&req.number_up) {
        return Err(AppError::bad_request(
            "pages per sheet must be 1, 2, 4, 6, 9 or 16",
        ));
    }
    cups::check_options(&req.options, ppd)?;

    let mut args = vec![
        "-d".into(),
        req.printer.clone(),
        "-n".into(),
        req.copies.to_string(),
        "-t".into(),
        title.to_string(),
    ];
    let mut option = |value: String| {
        args.push("-o".into());
        args.push(value);
    };
    // Tell CUPS what the file is instead of letting it guess (a text file
    // that starts with "%!" would otherwise be printed as PostScript).
    option(format!("document-format={mime}"));
    match req.page_set {
        PageSet::All => {}
        PageSet::Odd => option("page-set=odd".into()),
        PageSet::Even => option("page-set=even".into()),
    }
    if req.reverse {
        option("outputorder=reverse".into());
    }
    match req.orientation {
        Orientation::Auto => {}
        Orientation::Portrait => option("orientation-requested=3".into()),
        Orientation::Landscape => option("orientation-requested=4".into()),
    }
    match req.scaling {
        Scaling::Auto => {}
        Scaling::Fit => option("print-scaling=fit".into()),
        Scaling::Fill => option("print-scaling=fill".into()),
        Scaling::None => option("print-scaling=none".into()),
    }
    if req.number_up > 1 {
        option(format!("number-up={}", req.number_up));
    }
    if req.copies > 1 {
        option(format!(
            "multiple-document-handling=separate-documents-{}collated-copies",
            if req.collate { "" } else { "un" }
        ));
    }
    for (key, value) in &req.options {
        option(format!("{key}={value}"));
    }
    if let Some(ranges) =
        cups::normalize_page_ranges(&req.page_ranges).map_err(AppError::bad_request)?
    {
        args.extend(["-P".into(), ranges]);
    }
    Ok(args)
}

#[derive(Serialize)]
pub struct PrintResult {
    job_id: String,
    printer: String,
}

pub async fn print(
    State(st): State<Arc<AppState>>,
    Json(req): Json<PrintRequest>,
) -> Result<Json<PrintResult>> {
    let upload = st.uploads.get(&req.upload_id).ok_or_else(expired)?;
    let printer = cups::find_printer(&st.ipp, &req.printer).await?;
    if !printer.accepting {
        return Err(AppError::bad_request(format!(
            "{} is not accepting jobs right now",
            printer.name
        )));
    }
    let ppd = cups::ppd_options(&printer.name).await?;
    let title = match cups::clean_text(&req.title, 200) {
        t if t.is_empty() => upload.name.clone(),
        t => t,
    };
    let args = lp_args(&req, &title, upload.mime, &ppd)?;
    let job_id = cups::submit(&args, Some(&st.uploads.path(&upload.id))).await?;
    st.uploads.touch(&upload.id);
    eprintln!("print: \"{}\" to {} as {job_id}", upload.name, printer.name);
    Ok(Json(PrintResult {
        job_id,
        printer: printer.name,
    }))
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
pub struct JobsQuery {
    #[serde(default)]
    printer: String,
    /// Include finished jobs.
    #[serde(default = "yes")]
    history: bool,
}

#[derive(Serialize)]
pub struct JobsResponse {
    active: Vec<Job>,
    finished: Vec<Job>,
}

const HISTORY: usize = 30;

pub async fn jobs(
    State(st): State<Arc<AppState>>,
    Query(q): Query<JobsQuery>,
) -> Result<Json<JobsResponse>> {
    let mut active = cups::jobs(&st.ipp, false).await?;
    let mut finished = if q.history {
        cups::jobs(&st.ipp, true).await?
    } else {
        Vec::new()
    };
    if !q.printer.is_empty() {
        active.retain(|j| j.printer == q.printer);
        finished.retain(|j| j.printer == q.printer);
    }
    active.sort_by_key(|j| j.id);
    finished.sort_by_key(|j| std::cmp::Reverse(j.id));
    finished.truncate(HISTORY);
    Ok(Json(JobsResponse { active, finished }))
}

pub async fn cancel(UrlPath(id): UrlPath<u32>) -> Result<StatusCode> {
    cups::cancel(id).await?;
    eprintln!("print: canceled job {id}");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CancelQuery {
    /// Only this printer's jobs; empty for all printers.
    #[serde(default)]
    printer: String,
    /// Only jobs CUPS holds after a failure.
    #[serde(default)]
    stuck: bool,
}

#[derive(Serialize)]
pub struct Canceled {
    canceled: Vec<i32>,
}

/// Cancels all active jobs, or only the stuck ones, on one or all printers.
pub async fn cancel_many(
    State(st): State<Arc<AppState>>,
    Query(q): Query<CancelQuery>,
) -> Result<Json<Canceled>> {
    let printer = match q.printer.as_str() {
        "" => None,
        name if cups::valid_queue_name(name) => Some(name),
        _ => return Err(AppError::bad_request("invalid printer name")),
    };
    let canceled = cups::cancel_jobs(&st.ipp, printer, q.stuck, std::time::Duration::ZERO).await?;
    if !canceled.is_empty() {
        eprintln!(
            "print: canceled {}job(s) {canceled:?}",
            if q.stuck { "stuck " } else { "" }
        );
    }
    Ok(Json(Canceled { canceled }))
}

/// Tries a held job again now, e.g. after the printer was switched off and on.
pub async fn retry(UrlPath(id): UrlPath<u32>) -> Result<StatusCode> {
    cups::release(id).await?;
    eprintln!("print: released job {id}");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: &str) -> PrintRequest {
        serde_json::from_str(json).unwrap()
    }

    fn ppd() -> Vec<PpdOption> {
        cups::parse_ppd_options(
            "PageSize/Media Size: Letter *A4\nTonerDensity/Toner Density: 1 2 *3 4 5\n",
        )
    }

    #[test]
    fn minimal_request() {
        let req = request(r#"{"upload_id":"x","printer":"LBP1210"}"#);
        let args = lp_args(&req, "doc.pdf", "application/pdf", &ppd()).unwrap();
        assert_eq!(
            args,
            vec![
                "-d",
                "LBP1210",
                "-n",
                "1",
                "-t",
                "doc.pdf",
                "-o",
                "document-format=application/pdf"
            ]
        );
    }

    #[test]
    fn full_page_setup() {
        let req = request(
            r#"{"upload_id":"x","printer":"P","copies":2,"collate":true,"page_ranges":"5, 1-3",
                "page_set":"odd","reverse":true,"orientation":"landscape","scaling":"fit","number_up":4,
                "options":{"PageSize":"Letter","TonerDensity":"5"}}"#,
        );
        let args = lp_args(&req, "t", "image/png", &ppd()).unwrap().join(" ");
        assert_eq!(
            args,
            "-d P -n 2 -t t -o document-format=image/png -o page-set=odd -o outputorder=reverse \
             -o orientation-requested=4 -o print-scaling=fit -o number-up=4 \
             -o multiple-document-handling=separate-documents-collated-copies \
             -o PageSize=Letter -o TonerDensity=5 -P 1-3,5"
        );
    }

    #[test]
    fn rejects_bad_values() {
        let p = ppd();
        for json in [
            r#"{"upload_id":"x","printer":"P","copies":0}"#,
            r#"{"upload_id":"x","printer":"P","copies":1000}"#,
            r#"{"upload_id":"x","printer":"P","number_up":3}"#,
            r#"{"upload_id":"x","printer":"P","page_ranges":"3-1"}"#,
            r#"{"upload_id":"x","printer":"P","options":{"PageSize":"Tabloid"}}"#,
            r#"{"upload_id":"x","printer":"P","options":{"job-hold-until":"indefinite"}}"#,
        ] {
            assert!(
                lp_args(&request(json), "t", "application/pdf", &p).is_err(),
                "{json}"
            );
        }
        assert!(
            serde_json::from_str::<PrintRequest>(
                r#"{"upload_id":"x","printer":"P","scaling":"zoom"}"#
            )
            .is_err()
        );
    }
}
