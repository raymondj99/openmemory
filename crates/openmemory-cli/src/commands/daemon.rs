//! `openmemory daemon` — start the local admin API daemon.

use anyhow::{bail, Context, Result};
use openmemory_admin::{
    AdminError, AdminErrorCode, AdminErrorResponse, AdminShutdownResponse, DaemonRuntimeInfo,
    DaemonStatusResponse, DaemonStatusState, DaemonStopResponse,
};
use openmemory_core::config::Config;
use openmemory_daemon::{AdminToken, DaemonConfig};
use std::io::Write;
use std::time::Duration;

use crate::cli::{DaemonCommand, DaemonStartArgs, DaemonStatusArgs, DaemonStopArgs};

const DAEMON_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(profile: &str, command: DaemonCommand) -> Result<()> {
    match command {
        DaemonCommand::Start(args) => start(profile, args),
        DaemonCommand::Status(args) => status(args),
        DaemonCommand::Stop(args) => stop(args),
    }
}

fn start(profile: &str, args: DaemonStartArgs) -> Result<()> {
    let home = Config::home_dir().context("resolving OpenMemory home")?;
    ensure_no_running_daemon(&home)?;
    let token = openmemory_daemon::load_or_create_admin_token(&home)
        .context("loading daemon admin token")?;
    let admin_token = AdminToken::new(token).context("validating daemon admin token")?;
    let config = DaemonConfig::new(args.addr, admin_token, home, profile.to_string())
        .context("building daemon config")?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    let listener = runtime
        .block_on(openmemory_daemon::bind_listener(&config))
        .context("binding daemon admin API")?;
    let addr = listener
        .local_addr()
        .context("reading daemon listener address")?;
    let runtime_info =
        openmemory_daemon::runtime_info(&config, addr).context("building daemon runtime info")?;
    openmemory_daemon::write_runtime_info(config.home(), &runtime_info)
        .context("writing daemon runtime info")?;
    let home_for_cleanup = config.home().to_path_buf();

    daemon_log(format_args!(
        "openmemory daemon: admin API listening on http://{addr}"
    ));
    daemon_log(format_args!(
        "openmemory daemon: token loaded from per-home runtime storage"
    ));
    daemon_log(format_args!("openmemory daemon: press Ctrl-C to stop"));

    let serve_result = runtime
        .block_on(openmemory_daemon::serve_listener_until_shutdown(
            config,
            listener,
            async {
                let _ = tokio::signal::ctrl_c().await;
            },
        ))
        .context("serving daemon admin API");
    let cleanup_result = openmemory_daemon::remove_runtime_info(&home_for_cleanup)
        .context("removing daemon runtime metadata after daemon exit");
    serve_result.and(cleanup_result)
}

fn daemon_log(args: std::fmt::Arguments<'_>) {
    // A foreground daemon can outlive the process supervising its stderr.
    let _ = writeln!(std::io::stderr().lock(), "{args}");
}

fn status(args: DaemonStatusArgs) -> Result<()> {
    let home = Config::home_dir().context("resolving OpenMemory home")?;
    let response = daemon_status(&home);
    if args.json {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        render_status(&response);
    }
    Ok(())
}

fn stop(args: DaemonStopArgs) -> Result<()> {
    let home = Config::home_dir().context("resolving OpenMemory home")?;
    let response = daemon_stop(&home);
    if args.json {
        println!("{}", serde_json::to_string(&response)?);
    } else {
        render_stop(&response);
    }
    if response.stopped {
        Ok(())
    } else {
        let message = response
            .error
            .as_ref()
            .map_or("daemon was not stopped", |error| error.message.as_str());
        bail!("{message}")
    }
}

fn ensure_no_running_daemon(home: &std::path::Path) -> Result<()> {
    let Some(runtime) = openmemory_daemon::read_runtime_info(home)
        .context("checking existing daemon runtime metadata")?
    else {
        return Ok(());
    };

    if daemon_endpoint_responds(&runtime) {
        bail!(
            "openmemory daemon already running at {} (profile: {}); stop it before starting another daemon",
            runtime.admin_url,
            runtime.active_profile,
        );
    }

    openmemory_daemon::remove_runtime_info(home).context("removing stale daemon runtime metadata")
}

fn daemon_endpoint_responds(runtime: &DaemonRuntimeInfo) -> bool {
    let url = format!("{}/admin/health", runtime.admin_url.trim_end_matches('/'));
    match daemon_http_agent().get(&url).call() {
        Ok(_) | Err(ureq::Error::Status(_, _)) => true,
        Err(ureq::Error::Transport(_)) => false,
    }
}

fn daemon_status(home: &std::path::Path) -> DaemonStatusResponse {
    let runtime = match openmemory_daemon::read_runtime_info(home) {
        Ok(Some(info)) => info,
        Ok(None) => {
            return DaemonStatusResponse::not_started(AdminError::new(
                AdminErrorCode::DaemonNotFound,
                "daemon runtime metadata was not found",
                Some("Start the daemon with `openmemory daemon start --foreground`."),
                true,
            ));
        }
        Err(e) => {
            return DaemonStatusResponse::unreachable(
                None,
                AdminError::new(
                    AdminErrorCode::RuntimeMetadataInvalid,
                    "daemon runtime metadata could not be read",
                    Some("Remove the stale runtime file and restart the daemon."),
                    true,
                )
                .with_details(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    let token = match openmemory_daemon::load_admin_token(home) {
        Ok(Some(token)) => token,
        Ok(None) => {
            return DaemonStatusResponse::unreachable(
                Some(runtime),
                AdminError::new(
                    AdminErrorCode::AuthRequired,
                    "daemon admin token was not found",
                    Some("Restart the daemon to recreate local runtime credentials."),
                    true,
                ),
            );
        }
        Err(e) => {
            return DaemonStatusResponse::unreachable(
                Some(runtime),
                AdminError::new(
                    AdminErrorCode::AuthInvalid,
                    "daemon admin token could not be read",
                    Some("Check permissions on the OpenMemory run directory."),
                    true,
                )
                .with_details(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    match fetch_health(&runtime, &token) {
        Ok(health) => DaemonStatusResponse::running(runtime, health),
        Err(error) => DaemonStatusResponse::unreachable(Some(runtime), error),
    }
}

fn daemon_stop(home: &std::path::Path) -> DaemonStopResponse {
    let runtime = match openmemory_daemon::read_runtime_info(home) {
        Ok(Some(info)) => info,
        Ok(None) => {
            return DaemonStopResponse::not_stopped(
                None,
                AdminError::new(
                    AdminErrorCode::DaemonNotFound,
                    "daemon runtime metadata was not found",
                    Some("The daemon is already stopped or was never started."),
                    true,
                ),
            );
        }
        Err(e) => {
            return DaemonStopResponse::not_stopped(
                None,
                AdminError::new(
                    AdminErrorCode::RuntimeMetadataInvalid,
                    "daemon runtime metadata could not be read",
                    Some("Remove the stale runtime file and retry."),
                    true,
                )
                .with_details(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    let token = match openmemory_daemon::load_admin_token(home) {
        Ok(Some(token)) => token,
        Ok(None) => {
            return DaemonStopResponse::not_stopped(
                Some(runtime),
                AdminError::new(
                    AdminErrorCode::AuthRequired,
                    "daemon admin token was not found",
                    Some("Remove stale runtime metadata or restart the daemon."),
                    true,
                ),
            );
        }
        Err(e) => {
            return DaemonStopResponse::not_stopped(
                Some(runtime),
                AdminError::new(
                    AdminErrorCode::AuthInvalid,
                    "daemon admin token could not be read",
                    Some("Check permissions on the OpenMemory run directory."),
                    true,
                )
                .with_details(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    match request_shutdown(&runtime, &token) {
        Ok(()) => match openmemory_daemon::remove_runtime_info(home) {
            Ok(()) => DaemonStopResponse::stopped(runtime),
            Err(error) => DaemonStopResponse::not_stopped(
                Some(runtime),
                AdminError::new(
                    AdminErrorCode::RuntimeMetadataInvalid,
                    "daemon stopped but runtime metadata could not be removed",
                    Some("Remove the stale runtime file and retry status/stop."),
                    false,
                )
                .with_details(serde_json::json!({ "error": error.to_string() })),
            ),
        },
        Err(error) => DaemonStopResponse::not_stopped(Some(runtime), error),
    }
}

fn fetch_health(
    runtime: &DaemonRuntimeInfo,
    token: &str,
) -> Result<openmemory_admin::HealthResponse, AdminError> {
    let url = format!("{}/admin/health", runtime.admin_url.trim_end_matches('/'));
    let response = daemon_http_agent()
        .get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| http_error(&url, e))?;
    response.into_json().map_err(|e| {
        AdminError::new(
            AdminErrorCode::RuntimeMetadataInvalid,
            "daemon health response was not valid JSON",
            Some("Restart the daemon and try again."),
            true,
        )
        .with_details(serde_json::json!({ "error": e.to_string() }))
    })
}

fn request_shutdown(runtime: &DaemonRuntimeInfo, token: &str) -> Result<(), AdminError> {
    let url = format!("{}/admin/shutdown", runtime.admin_url.trim_end_matches('/'));
    let response = daemon_http_agent()
        .post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|e| http_error(&url, e))?;
    if !(200..300).contains(&response.status()) {
        return Err(AdminError::new(
            AdminErrorCode::DaemonUnreachable,
            format!("daemon shutdown returned HTTP {}", response.status()),
            Some("Verify that the runtime metadata points at the current daemon."),
            true,
        ));
    }
    let payload = response.into_json::<AdminShutdownResponse>().map_err(|e| {
        AdminError::new(
            AdminErrorCode::RuntimeMetadataInvalid,
            "daemon shutdown response was not valid JSON",
            Some("Restart the daemon and try again."),
            true,
        )
        .with_details(serde_json::json!({ "error": e.to_string() }))
    })?;
    if payload.accepted {
        Ok(())
    } else {
        Err(AdminError::new(
            AdminErrorCode::Conflict,
            "daemon shutdown was rejected",
            Some("Retry after checking daemon health."),
            false,
        ))
    }
}

fn daemon_http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(DAEMON_HTTP_TIMEOUT)
        .build()
}

fn http_error(url: &str, error: ureq::Error) -> AdminError {
    match error {
        ureq::Error::Status(status, response) => {
            if let Ok(payload) = response.into_json::<AdminErrorResponse>() {
                return payload.error;
            }
            AdminError::new(
                AdminErrorCode::DaemonUnreachable,
                format!("daemon health check returned HTTP {status}"),
                Some("Verify that the runtime metadata points at the current daemon."),
                true,
            )
            .with_details(serde_json::json!({ "url": url, "status": status }))
        }
        ureq::Error::Transport(e) => AdminError::new(
            AdminErrorCode::DaemonUnreachable,
            "daemon health check failed",
            Some("The daemon may have exited; restart it and try again."),
            true,
        )
        .with_details(serde_json::json!({ "url": url, "error": e.to_string() })),
    }
}

fn render_status(response: &DaemonStatusResponse) {
    match response.state {
        DaemonStatusState::Running => {
            let runtime = response.runtime.as_ref().expect("running includes runtime");
            println!(
                "openmemory daemon: running at {} (profile: {})",
                runtime.admin_url, runtime.active_profile
            );
        }
        DaemonStatusState::NotStarted => {
            println!("openmemory daemon: not running");
        }
        DaemonStatusState::Unreachable => {
            let message = response
                .error
                .as_ref()
                .map_or("unreachable", |e| e.message.as_str());
            println!("openmemory daemon: unreachable - {message}");
        }
    }
}

fn render_stop(response: &DaemonStopResponse) {
    if response.stopped {
        if let Some(runtime) = response.runtime.as_ref() {
            println!(
                "openmemory daemon: stopped {} (profile: {})",
                runtime.admin_url, runtime.active_profile
            );
        } else {
            println!("openmemory daemon: stopped");
        }
    } else {
        let message = response
            .error
            .as_ref()
            .map_or("not stopped", |e| e.message.as_str());
        println!("openmemory daemon: not stopped - {message}");
    }
}
