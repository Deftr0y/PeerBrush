//! A one-shot CLI consumes JSON and image strings without a response DOM.
#[path = "cli/files.rs"]
mod files;
#[path = "cli/json.rs"]
mod json;
use files::{Files, Spool};
use serde_json::Value;
use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
};

pub enum CliStatus {
    Success,
    Failed,
}
pub fn run(state: &Path, method: &str, params: &Value) -> Result<CliStatus, String> {
    let response = super::connection_request(
        state,
        "rpc",
        &serde_json::json!({"method":method,"params":params}),
        std::time::Duration::from_secs(60),
    )
    .map_err(|failure| failure.message.to_owned())?;
    if response.status() != 200
        || !response.header("Content-Type").is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err("Invalid PeerBrush CLI response".into());
    }
    let stdout = io::stdout();
    let stderr = io::stderr();
    consume(
        response.into_reader(),
        state,
        &mut stdout.lock(),
        &mut stderr.lock(),
        &mut || Ok(()),
    )
}
pub(super) fn consume(
    input: impl Read,
    state: &Path,
    output: &mut impl Write,
    error: &mut impl Write,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<CliStatus, String> {
    let mut files = Files::new(state)?;
    let mut json = BufWriter::with_capacity(64 * 1024, Spool::temporary(&files, "response")?);
    let mut reason = BufWriter::with_capacity(64 * 1024, Spool::temporary(&files, "error")?);
    let mut input = BufReader::with_capacity(64 * 1024, input.take(512 * 1024 * 1024 + 1));
    let failed = json::project(&mut input, &mut json, &mut reason, &mut files, check)?;
    let mut json = json
        .into_inner()
        .map_err(|_| "CLI response spool flush failed")?;
    let mut reason = reason
        .into_inner()
        .map_err(|_| "CLI error spool flush failed")?;
    json.seal(&files.pool, check)?;
    reason.seal(&files.pool, check)?;
    files.publish(check)?;
    json.replay(output, check)?;
    output
        .write_all(b"\n")
        .map_err(|_| "CLI output was interrupted")?;
    output.flush().map_err(|_| "CLI output was interrupted")?;
    if failed {
        error
            .write_all(b"PeerBrush: ")
            .map_err(|_| "CLI error output was interrupted")?;
        reason.replay(error, check)?;
        error
            .write_all(b"\n")
            .map_err(|_| "CLI error output was interrupted")?;
        error
            .flush()
            .map_err(|_| "CLI error output was interrupted")?;
        Ok(CliStatus::Failed)
    } else {
        Ok(CliStatus::Success)
    }
}

#[cfg(test)]
#[path = "cli/tests.rs"]
mod tests;
