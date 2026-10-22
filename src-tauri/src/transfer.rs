//! The resumable chunked upload loop.

use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::model::{ErrorKind, UploadError};
use crate::net::{Api, Head, OnSent, Reply};

/// Bytes per PATCH request.
const CHUNK: u64 = 8 * 1024 * 1024;
/// Bytes per body piece; progress is reported as each piece is handed to the connection.
const PIECE: usize = 256 * 1024;
/// One entry per allowed retry.
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

pub enum Sent {
    /// The server's URL from the 201 response.
    Url(String),
    /// The file was already complete on the server (an earlier 201 was lost).
    Exists,
}

pub fn auth_error() -> UploadError {
    UploadError::new(ErrorKind::Auth, "The token wasn’t accepted.")
}

pub fn network_error() -> UploadError {
    UploadError::new(ErrorKind::Network, "Can’t reach the server.")
}

fn server_error(message: impl Into<String>) -> UploadError {
    UploadError::new(ErrorKind::Server, message)
}

fn unexpected() -> UploadError {
    server_error("The server sent an unexpected reply.")
}

enum Resume {
    At(u64),
    Done,
    Unknown,
}

/// Asks the server how much of `name` it has.
async fn resume_point(api: &Api, name: &str, total: u64) -> Result<Resume, UploadError> {
    match api.head(name).await {
        Ok(Head::Partial(offset)) if offset <= total => Ok(Resume::At(offset)),
        Ok(Head::Missing) => Ok(Resume::At(0)),
        Ok(Head::Complete(len)) if len == total => Ok(Resume::Done),
        Ok(Head::Status(401)) => Err(auth_error()),
        Ok(Head::Status(s)) if s >= 500 => Ok(Resume::Unknown),
        Ok(_) => Err(unexpected()),
        Err(_) => Ok(Resume::Unknown),
    }
}

async fn read_pieces(file: &mut File, offset: u64, len: u64) -> std::io::Result<Vec<Vec<u8>>> {
    file.seek(SeekFrom::Start(offset)).await?;
    let mut pieces = Vec::new();
    let mut left = len;
    while left > 0 {
        let mut piece = vec![0; left.min(PIECE as u64) as usize];
        file.read_exact(&mut piece).await?;
        left -= piece.len() as u64;
        pieces.push(piece);
    }
    Ok(pieces)
}

/// Uploads `file` (`total` bytes) as `name`, chunk by chunk. With `resume`, first asks the
/// server where to continue. Network errors, timeouts, 5xx and `busy` are retried with
/// backoff, re-syncing the offset with `HEAD` before each retry. The retry budget starts
/// over whenever the server's copy grows, so a slow link that keeps making progress never
/// runs out of retries.
pub async fn send(
    api: &Api,
    file: &mut File,
    name: &str,
    total: u64,
    resume: bool,
    progress: &OnSent,
) -> Result<Sent, UploadError> {
    let mut offset = 0;
    if resume {
        match resume_point(api, name, total).await? {
            Resume::At(at) => offset = at,
            Resume::Done => return Ok(Sent::Exists),
            Resume::Unknown => {}
        }
    }
    let mut retries = 0;
    loop {
        let start = offset;
        let len = CHUNK.min(total - offset);
        let body = read_pieces(file, offset, len)
            .await
            .map_err(|_| UploadError::new(ErrorKind::Missing, "The file changed while uploading."))?;
        let on_sent: OnSent = {
            let (progress, base) = (progress.clone(), offset);
            Arc::new(move |n| progress(base + n))
        };
        let failure = match api.patch(name, offset, total, body, on_sent).await {
            Ok(Reply::Created(url)) => return Ok(Sent::Url(url)),
            Ok(Reply::Accepted(at)) if at > offset && at <= total => {
                offset = at;
                retries = 0;
                continue;
            }
            Ok(Reply::Offset(at)) if at != offset && at <= total => {
                offset = at;
                continue;
            }
            Ok(Reply::Accepted(_) | Reply::Offset(_)) => return Err(unexpected()),
            // Our own earlier attempt finished but its reply was lost, or (astronomically
            // unlikely) the random name is taken: only the first counts as success. If the
            // server can't say which right now, ask again after the usual backoff.
            Ok(Reply::Exists) => match resume_point(api, name, total).await? {
                Resume::Done => return Ok(Sent::Exists),
                Resume::At(_) => return Err(server_error("A file with this name already exists.")),
                Resume::Unknown => network_error(),
            },
            Ok(Reply::Status(401)) => return Err(auth_error()),
            Ok(Reply::Status(413)) => return Err(server_error("The file is too large for the server.")),
            Ok(Reply::Busy) => server_error("The server is busy. Try again in a moment."),
            Ok(Reply::Status(s)) if s >= 500 => server_error("The server ran into a problem."),
            Ok(Reply::Status(s)) => return Err(server_error(format!("The server refused the upload ({s})."))),
            Err(_) => network_error(),
        };
        let Some(delay) = BACKOFF.get(retries) else { return Err(failure) };
        retries += 1;
        tokio::time::sleep(*delay).await;
        match resume_point(api, name, total).await? {
            Resume::At(at) => {
                if at > start {
                    retries = 0;
                }
                offset = at;
            }
            Resume::Done => return Ok(Sent::Exists),
            Resume::Unknown => {}
        }
    }
}
