//! Authenticated daemon-backed space, audit, review, and merge commands.
//!
//! Product-control and audited semantic writes are deliberately serialized by
//! the daemon. These commands discover that daemon and never open the product
//! catalog or a graph writer in the CLI process.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use openmemory_admin::{
    AdminChangeDecisionRequest, AdminChangeSetState, AdminCreateProjectRequest,
    AdminCreateSpaceRequest, AdminDeleteSpaceRequest, AdminEditMemoryRequest,
    AdminIdentityDecisionRequest, AdminLifecycleRequest, AdminMapWorkspaceRequest,
    AdminMergeConfirmRequest, AdminMergePreviewRequest, AdminMergeResolutionRequest,
    AdminResolveContextRequest, AdminRevertRequest, AdminSpaceContext, AdminSpaceDetail,
    AdminSpaceOwner,
};
use openmemory_core::config::Config;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::{
    ChangeSetCommand, ContextCommand, MemoryCommand, MergeCommand, ProjectCommand, ReviewCommand,
    SpaceCommand,
};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALLATION_PRINCIPAL: &str = "local:installation";

pub fn space(command: SpaceCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        SpaceCommand::List(args) => render(client.get("/admin/spaces")?, args.json),
        SpaceCommand::Create(args) => {
            let payload = AdminCreateSpaceRequest {
                owner: parse_owner(&args.owner)?,
                context: parse_space_context(&args.context)?,
                display_name: args.name,
                domain_count: args.domains,
            };
            render(client.post("/admin/spaces", &payload)?, args.json);
        }
        SpaceCommand::Show(args) => render(
            client.get(&format!("/admin/spaces/{}", segment(&args.id)))?,
            args.json,
        ),
        SpaceCommand::Close(args) => render(
            client.post_value(
                &format!("/admin/spaces/{}/close", segment(&args.id)),
                &json!({}),
            )?,
            args.json,
        ),
        SpaceCommand::Delete(args) => {
            if !args.yes {
                bail!("space deletion requires --yes");
            }
            let detail: AdminSpaceDetail = serde_json::from_value(
                client.get(&format!("/admin/spaces/{}", segment(&args.id)))?,
            )
            .context("decoding current space generation")?;
            let request = AdminDeleteSpaceRequest {
                expected_catalog_generation: detail.catalog_generation,
                confirmation: args.id.clone(),
                no_backup: args.no_backup,
            };
            render(
                client.post(
                    &format!("/admin/spaces/{}/delete", segment(&args.id)),
                    &request,
                )?,
                args.json,
            );
        }
    }
    Ok(())
}

pub fn project(command: ProjectCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        ProjectCommand::Map(args) => {
            let project_id = if let Some(id) = args.project {
                id
            } else {
                let created = client.post(
                    "/admin/projects",
                    &AdminCreateProjectRequest {
                        display_name: args.new.expect("clap requires project selection"),
                    },
                )?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .context("daemon project response omitted id")?
                    .to_string()
            };
            let workspace = args
                .path
                .to_str()
                .context("workspace path is not valid UTF-8")?
                .to_string();
            let resolved = client.post_value(
                "/admin/workspaces/resolve",
                &json!({ "workspace": workspace }),
            )?;
            let workspace_id = resolved
                .get("workspace_id")
                .and_then(Value::as_str)
                .context("daemon workspace response omitted workspace_id")?;
            let payload = AdminMapWorkspaceRequest {
                workspace,
                project_id,
                vcs_fingerprint: None,
            };
            render(
                client.put(
                    &format!("/admin/workspaces/{}/project", segment(workspace_id)),
                    &payload,
                )?,
                args.json,
            );
        }
    }
    Ok(())
}

pub fn context(command: ContextCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        ContextCommand::Show(args) => {
            let workspace = args
                .workspace
                .as_deref()
                .map(|path| {
                    path.to_str()
                        .map(str::to_string)
                        .context("workspace path is not valid UTF-8")
                })
                .transpose()?;
            if workspace.is_none() && args.team.is_none() {
                render(client.get("/admin/context")?, args.json);
            } else {
                let request = AdminResolveContextRequest {
                    principal_id: INSTALLATION_PRINCIPAL.to_string(),
                    actor_kind: "human".to_string(),
                    workspace,
                    project_id: None,
                    active_team_id: args.team,
                    read_mode: "contextual".to_string(),
                    write_target: "default".to_string(),
                };
                render(
                    client.post("/admin/context/resolve", &request)?,
                    args.json,
                );
            }
        }
    }
    Ok(())
}

pub fn changeset(command: ChangeSetCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        ChangeSetCommand::List(args) => {
            let space = current_or_explicit_space(&client, args.space)?;
            let mut path = format!(
                "/admin/changesets?space_id={}&limit={}",
                query(&space),
                args.limit
            );
            if let Some(state) = args.state {
                path.push_str("&state=");
                path.push_str(&query(&state));
            }
            render(client.get(&path)?, args.json);
        }
        ChangeSetCommand::Show(args) => {
            let space = current_or_explicit_space(&client, args.space)?;
            render(
                client.get(&format!(
                    "/admin/changesets/{}?space_id={}",
                    segment(&args.id),
                    query(&space)
                ))?,
                args.json,
            );
        }
        ChangeSetCommand::Revert(args) => {
            let space = current_or_explicit_space(&client, args.space)?;
            let request = json!({
                "idempotency_key": args.idempotency_key.unwrap_or_else(new_cli_key),
                "reason": args.reason,
            });
            render(
                client.post_value(
                    &format!(
                        "/admin/changesets/{}/revert?space_id={}",
                        segment(&args.id),
                        query(&space)
                    ),
                    &request,
                )?,
                args.json,
            );
        }
    }
    Ok(())
}

pub fn review(command: ReviewCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    let (args, action) = match command {
        ReviewCommand::Approve(args) => (args, "approve"),
        ReviewCommand::Reject(args) => (args, "reject"),
    };
    let space = current_or_explicit_space(&client, args.space)?;
    let request = AdminChangeDecisionRequest {
        expected_state: AdminChangeSetState::Proposed,
        authority_generation: args.authority_generation,
        reason: args.reason,
    };
    render(
        client.post(
            &format!(
                "/admin/changesets/{}/{}?space_id={}",
                segment(&args.id),
                action,
                query(&space)
            ),
            &request,
        )?,
        args.json,
    );
    Ok(())
}

pub fn memory(command: MemoryCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        MemoryCommand::History(args) => render(
            client.get(&format!(
                "/admin/memories/{}/history?space_id={}&object_kind={}&limit={}",
                segment(&args.logical_id),
                query(&args.space),
                query(&args.object_kind),
                args.limit
            ))?,
            args.json,
        ),
        MemoryCommand::Diff(args) => {
            let mut path = format!(
                "/admin/memories/{}/diff?space_id={}&object_kind={}",
                segment(&args.logical_id),
                query(&args.space),
                query(&args.object_kind)
            );
            optional_query(&mut path, "from", args.from.as_deref());
            optional_query(&mut path, "to", args.to.as_deref());
            optional_query(&mut path, "expected", args.expected.as_deref());
            render(client.get(&path)?, args.json);
        }
        MemoryCommand::Edit(args) => {
            let fields =
                serde_json::from_str(&args.fields_json).context("--fields-json is invalid JSON")?;
            let request = AdminEditMemoryRequest {
                expected_revision_id: args.expected_revision,
                expected_row_version: args.expected_row_version,
                expected_lifecycle: args.expected_lifecycle,
                reason: args.reason,
                fields,
            };
            render(
                client.post(
                    &format!(
                        "/admin/memories/{}/edit?space_id={}",
                        segment(&args.logical_id),
                        query(&args.space)
                    ),
                    &request,
                )?,
                args.json,
            );
        }
        MemoryCommand::Retire(args) => lifecycle(&client, args, "retire")?,
        MemoryCommand::Restore(args) => lifecycle(&client, args, "restore")?,
        MemoryCommand::Revert(args) => {
            let request = AdminRevertRequest {
                expected_revision_id: args.expected_revision,
                expected_row_version: args.expected_row_version,
                revision_id: args.revision,
                reason: args.reason,
            };
            render(
                client.post(
                    &format!(
                        "/admin/memories/{}/revert?space_id={}&object_kind={}",
                        segment(&args.logical_id),
                        query(&args.space),
                        query(&args.object_kind)
                    ),
                    &request,
                )?,
                args.json,
            );
        }
        MemoryCommand::CherryPick(args) => {
            let request = json!({
                "target_space_id": args.to,
                "object_kind": args.object_kind,
                "idempotency_key": args.idempotency_key.unwrap_or_else(new_cli_key),
                "reason": args.reason,
            });
            render(
                client.post_value(
                    &format!(
                        "/admin/spaces/{}/cherry-pick/{}",
                        segment(&args.from),
                        segment(&args.logical_id)
                    ),
                    &request,
                )?,
                args.json,
            );
        }
        MemoryCommand::DestroyPreview(args) => render(
            client.post_value(
                &format!(
                    "/admin/memories/{}/destroy/preview?space_id={}",
                    segment(&args.logical_id),
                    query(&args.space)
                ),
                &json!({
                    "object_kind": args.object_kind,
                    "scope": args.scope,
                }),
            )?,
            args.json,
        ),
        MemoryCommand::Destroy(args) => {
            if !args.yes {
                bail!("irreversible memory destruction requires --yes");
            }
            render(
                client.post_value(
                    &format!(
                        "/admin/memories/{}/destroy?space_id={}",
                        segment(&args.logical_id),
                        query(&args.space)
                    ),
                    &json!({
                        "object_kind": args.object_kind,
                        "scope": args.scope,
                        "confirmation": args.confirmation,
                        "reason": args.reason,
                    }),
                )?,
                args.json,
            );
        }
    }
    Ok(())
}

pub fn merge(command: MergeCommand) -> Result<()> {
    let client = AdminClient::connect()?;
    match command {
        MergeCommand::Preview(args) => {
            let request = AdminMergePreviewRequest {
                source_space_id: args.source,
                target_space_id: args.target,
                idempotency_key: args.idempotency_key.unwrap_or_else(new_cli_key),
                keep_undetermined_distinct: args.keep_undetermined_distinct,
            };
            render(client.post("/admin/merges/preview", &request)?, args.json);
        }
        MergeCommand::Candidates(args) => {
            let mut path = format!("/admin/merges/{}/candidates", segment(&args.job));
            if let Some(state) = args.state {
                path.push_str("?state=");
                path.push_str(&query(&state));
            }
            render(client.get(&path)?, args.json);
        }
        MergeCommand::Decide(args) => {
            let request = AdminIdentityDecisionRequest {
                decision: args.decision,
                packet_hash: args.packet_hash,
                authority_generation: args.authority_generation,
                reason: args.reason,
            };
            render(
                client.post(
                    &format!(
                        "/admin/identity/candidates/{}/decide",
                        segment(&args.candidate)
                    ),
                    &request,
                )?,
                args.json,
            );
        }
        MergeCommand::Resolve(args) => {
            let request = AdminMergeResolutionRequest {
                expected_plan_hash: String::new(),
                resolutions: Vec::new(),
                idempotency_key: new_cli_key(),
            };
            render(
                client.post(
                    &format!("/admin/merges/{}/resolve", segment(&args.job)),
                    &request,
                )?,
                args.json,
            );
        }
        MergeCommand::Apply(args) => {
            if !args.yes {
                bail!("directional material merge requires --yes");
            }
            let request = AdminMergeConfirmRequest {
                expected_plan_hash: args.plan_hash,
                expected_target_hash: args.target_hash,
                confirmation: args.confirmation,
                idempotency_key: new_cli_key(),
            };
            render(
                client.post(
                    &format!("/admin/merges/{}/confirm", segment(&args.job)),
                    &request,
                )?,
                args.json,
            );
        }
        MergeCommand::Status(args) => render(
            client.get(&format!("/admin/merges/{}", segment(&args.job)))?,
            args.json,
        ),
        MergeCommand::Recover(args) => {
            let job = args
                .job
                .context("merge recovery requires --job until global recovery scanning is ready")?;
            render(
                client.post_value(
                    &format!("/admin/merges/recover?job_id={}", query(&job)),
                    &json!({}),
                )?,
                args.json,
            );
        }
        MergeCommand::Cancel(args) => {
            client.post_value(
                &format!("/admin/merges/{}/cancel", segment(&args.job)),
                &json!({}),
            )?;
            render(json!({ "cancelled": true, "job_id": args.job }), args.json);
        }
    }
    Ok(())
}

fn lifecycle(
    client: &AdminClient,
    args: crate::cli::MemoryLifecycleArgs,
    operation: &str,
) -> Result<()> {
    let request = AdminLifecycleRequest {
        expected_revision_id: args.expected_revision,
        expected_row_version: args.expected_row_version,
        expected_lifecycle: args.expected_lifecycle,
        reason: args.reason,
    };
    render(
        client.post(
            &format!(
                "/admin/memories/{}/{}?space_id={}&object_kind={}",
                segment(&args.logical_id),
                operation,
                query(&args.space),
                query(&args.object_kind)
            ),
            &request,
        )?,
        args.json,
    );
    Ok(())
}

fn current_or_explicit_space(client: &AdminClient, space: Option<String>) -> Result<String> {
    if let Some(space) = space {
        return Ok(space);
    }
    client
        .get("/admin/context")?
        .get("write_target")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("daemon context response omitted write_target")
}

fn parse_owner(value: &str) -> Result<AdminSpaceOwner> {
    if value == "personal" {
        return Ok(AdminSpaceOwner::User(INSTALLATION_PRINCIPAL.to_string()));
    }
    value
        .strip_prefix("team:")
        .filter(|id| !id.is_empty())
        .map(|id| AdminSpaceOwner::Team(id.to_string()))
        .context("--owner must be `personal` or `team:<id>`")
}

fn parse_space_context(value: &str) -> Result<AdminSpaceContext> {
    if value == "global" {
        return Ok(AdminSpaceContext::Global);
    }
    value
        .strip_prefix("project:")
        .filter(|id| !id.is_empty())
        .map(|id| AdminSpaceContext::Project(id.to_string()))
        .context("--context must be `global` or `project:<id>`")
}

fn optional_query(path: &mut String, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        path.push('&');
        path.push_str(name);
        path.push('=');
        path.push_str(&query(value));
    }
}

fn new_cli_key() -> String {
    format!(
        "cli:{}:{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    )
}

fn render(value: Value, compact: bool) {
    if compact {
        println!("{}", serde_json::to_string(&value).expect("JSON value serializes"));
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value serializes")
        );
    }
}

fn segment(value: &str) -> String {
    percent_encode(value)
}

fn query(value: &str) -> String {
    percent_encode(value)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing into String cannot fail");
        }
    }
    encoded
}

struct AdminClient {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl AdminClient {
    fn connect() -> Result<Self> {
        let home = Config::home_dir().context("resolving OpenMemory home")?;
        let runtime = openmemory_daemon::read_runtime_info(&home)
            .context("reading daemon runtime metadata")?
            .context(
                "review/space/merge administration requires a running daemon; \
                 start it with `openmemory daemon start --foreground`",
            )?;
        let token = openmemory_daemon::load_admin_token(&home)
            .context("reading daemon admin token")?
            .context("daemon admin token is missing; restart the daemon")?;
        Ok(Self {
            base: runtime.admin_url.trim_end_matches('/').to_string(),
            token,
            agent: ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build(),
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.decode(self.authorize(self.agent.get(&self.url(path))).call())
    }

    fn post<T: Serialize>(&self, path: &str, body: &T) -> Result<Value> {
        self.post_value(path, &serde_json::to_value(body)?)
    }

    fn post_value(&self, path: &str, body: &Value) -> Result<Value> {
        self.decode(
            self.authorize(self.agent.post(&self.url(path)))
                .send_json(body.clone()),
        )
    }

    fn put<T: Serialize>(&self, path: &str, body: &T) -> Result<Value> {
        self.decode(
            self.authorize(self.agent.put(&self.url(path)))
                .send_json(serde_json::to_value(body)?),
        )
    }

    fn authorize(&self, request: ureq::Request) -> ureq::Request {
        request.set("Authorization", &format!("Bearer {}", self.token))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn decode(&self, result: Result<ureq::Response, ureq::Error>) -> Result<Value> {
        match result {
            Ok(response) if response.status() == 204 => Ok(Value::Null),
            Ok(response) => response.into_json().context("daemon returned invalid JSON"),
            Err(ureq::Error::Status(status, response)) => {
                let payload = response.into_json::<Value>().unwrap_or(Value::Null);
                let message = payload
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("daemon request failed");
                bail!("{message} (HTTP {status})")
            }
            Err(ureq::Error::Transport(error)) => bail!("daemon request failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_values_are_encoded_without_reinterpreting_ids() {
        assert_eq!(percent_encode("space:abc/def?x=1"), "space:abc%2Fdef%3Fx%3D1");
    }

    #[test]
    fn strict_owner_and_context_parsing() {
        assert!(matches!(
            parse_owner("personal").unwrap(),
            AdminSpaceOwner::User(_)
        ));
        assert!(parse_owner("other").is_err());
        assert_eq!(
            parse_space_context("global").unwrap(),
            AdminSpaceContext::Global
        );
        assert!(parse_space_context("project:").is_err());
    }
}
