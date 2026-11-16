//! The provider's session import and export, over the session files Pi keeps.
//!
//! Pi keeps a session as a file in its own agent directory (or the one `PI_CODING_AGENT_SESSION_DIR` or the agent's
//! `settings.json` names), on the machine the provider runs on: a spawner that runs programs elsewhere is refused. The
//! directory is the account's own for a Pi key or sign-in, and the environment's otherwise.

use alloc::format;
use alloc::string::ToString;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use coqui_acp_api::guatiao::Status;
use coqui_acp_api::guatiao::library::{Object, ProviderError};
use coqui_acp_api::{
    AccountGrant, Environment, ExportPage, HistoryLoss, ImportReport, LINE_TOO_LARGE,
    ProcessSpawner, SeedPoint, SessionExport, SessionImport,
};
use coqui_acp_harness::account::same_name;
use coqui_acp_harness::stdio::accounts::apply;
use coqui_acp_history::conversation::is_uuid;
use coqui_acp_history::paging::{PageError, page};
use coqui_acp_history::{Document, Header, Origin};

use crate::PiLaunch;
use crate::accounts::DEF;
use crate::history::{DEFAULT_MAX_BYTES, HARNESS, ImportOptions, export_file, find, import_file};

fn error(status: Status, why: &str) -> ProviderError {
    ProviderError::new(status, why)
}

/// The directory the session's file is in, after the checks every call makes: the spawner runs on this machine (its
/// files are this machine's), the id is a UUID, and the working directory is absolute.
fn place(
    spawner: &Object<dyn ProcessSpawner>,
    id: &str,
    working_directory: &str,
    environment: &Environment,
    account: Option<&AccountGrant>,
) -> Result<(PathBuf, PathBuf), ProviderError> {
    if !spawner.describe()?.local {
        return Err(error(
            Status::GUATIAO_ERR_UNSUPPORTED,
            "this spawner runs where this machine's files are not, and a session is read and written there",
        ));
    }
    if !is_uuid(id) {
        return Err(error(
            Status::GUATIAO_ERR_BAD_VALUE,
            "the session id is not a UUID",
        ));
    }
    let cwd = Path::new(working_directory);
    if !cwd.is_absolute() {
        return Err(error(
            Status::GUATIAO_ERR_BAD_VALUE,
            "the working directory is not absolute",
        ));
    }
    let mut launch = PiLaunch::new(cwd);
    for (name, value) in environment.iter() {
        launch.env.insert(name.into(), value.into());
    }
    if let Some(grant) = account
        && let Some(applied) = apply(&DEF, grant)?
    {
        // The account's own directory and nothing of the host's under a credential's name, as a session runs.
        launch.env.retain(|name, _| {
            !DEF.variables
                .iter()
                .any(|variable| same_name(&name.to_string_lossy(), variable))
        });
        launch.remove_env = DEF.variables.iter().map(OsString::from).collect();
        for (name, value) in applied.environment.expose().iter() {
            launch.env.insert(name.into(), value.into());
        }
    }
    let directory = launch.sessions_dir().ok_or_else(|| {
        error(
            Status::GUATIAO_ERR_BAD_VALUE,
            "Pi's agent directory is unknown",
        )
    })?;
    Ok((directory, cwd.to_path_buf()))
}

fn read_error(status: Status, why: &crate::Error) -> ProviderError {
    error(status, &why.to_string())
}

pub(crate) fn import(
    session: &SessionImport,
    spawner: &Object<dyn ProcessSpawner>,
) -> Result<ImportReport, ProviderError> {
    let (directory, cwd) = place(
        spawner,
        &session.session_id,
        &session.working_directory,
        session.environment.expose(),
        session.account.as_ref(),
    )?;
    let doc = if session.history.trim().is_empty() {
        // A session of no turns: a header, which Pi resumes and then writes its first message after.
        Document::new(Header::new(&session.session_id, Origin::new(HARNESS)))
    } else {
        Document::from_slice(session.history.as_bytes()).map_err(|why| {
            // A version this crate does not read is the harness not importing it, not a malformed history.
            let status = if matches!(why, coqui_acp_history::HistoryError::UnknownVersion { .. }) {
                Status::GUATIAO_ERR_UNSUPPORTED
            } else {
                Status::GUATIAO_ERR_BAD_VALUE
            };
            error(status, &format!("the history cannot be read: {why}"))
        })?
    };
    let imported = import_file(
        &directory,
        &cwd,
        &session.session_id,
        &doc,
        &ImportOptions::default(),
    )
    .map_err(|why| read_error(Status::GUATIAO_ERR_BAD_VALUE, &why))?;
    let mut report = ImportReport::new();
    report.seeded_from = imported.seeded_from.map(|point| {
        let mut seeded = SeedPoint::line(point.line);
        seeded.digest = Some(point.digest);
        seeded
    });
    report.losses = imported
        .report
        .losses()
        .iter()
        .map(|loss| {
            let mut mapped = HistoryLoss::new(&loss.kind, loss.count);
            mapped.detail.clone_from(&loss.detail);
            mapped
        })
        .collect();
    Ok(report)
}

pub(crate) fn export(
    request: &SessionExport,
    spawner: &Object<dyn ProcessSpawner>,
) -> Result<ExportPage, ProviderError> {
    let (directory, _) = place(
        spawner,
        &request.session_id,
        &request.working_directory,
        request.environment.expose(),
        request.account.as_ref(),
    )?;
    let found = find(&directory, &request.session_id)
        .map_err(|why| read_error(Status::GUATIAO_ERR_BAD_VALUE, &why))?;
    if found.is_none() {
        return Err(error(
            Status::GUATIAO_ERR_NOT_FOUND,
            "no session has this id",
        ));
    }
    let text = export_file(&directory, &request.session_id, DEFAULT_MAX_BYTES)
        .and_then(|doc| {
            doc.to_string()
                .map_err(|why| crate::Error::Refused(why.to_string()))
        })
        .map_err(|why| read_error(Status::GUATIAO_ERR_BAD_VALUE, &why))?;
    match page(&text, request.after.as_deref(), request.max_bytes) {
        Ok(page) => Ok(ExportPage::new(page.text, page.next)),
        Err(PageError::LineTooLarge(size)) => Err(error(
            Status::GUATIAO_ERR_BAD_VALUE,
            &format!(
                "{LINE_TOO_LARGE}{size} bytes, more than max_bytes {}",
                request.max_bytes
            ),
        )),
        Err(other) => Err(error(Status::GUATIAO_ERR_BAD_VALUE, &other.to_string())),
    }
}
