use super::*;
use axum::body::{to_bytes, Body};
use axum::http::{HeaderValue, Method, Request, StatusCode};
use openmemory_admin::{
    AdminBackupCreateReport, AdminBackupManifest, AdminBackupPreflightResponse,
    AdminIntegrationClient, AdminIntegrationInstallResponse, AdminIntegrationOutcome,
    AdminIntegrationPreview, AdminIntegrationsResponse, AdminProfilesResponse,
    AdminRestorePreflightResponse, AdminRestoreReport, AdminShutdownResponse, ComponentState, Page,
    PageRequest,
};
use openmemory_graph::{EntityType, ObservationInput, RelationInput};
use tower::ServiceExt;

fn test_config() -> DaemonConfig {
    test_config_with_home(PathBuf::from("/tmp/openmemory-test"))
}

fn test_config_with_home(home: PathBuf) -> DaemonConfig {
    DaemonConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        AdminToken::new("secret").unwrap(),
        home,
        "default",
    )
    .unwrap()
}

fn seed_default_profile(home: &Path) -> (String, String) {
    let config = Config::default();
    config.save(home.join("config.toml")).unwrap();
    let data_dir = home.join("data").join("default");
    let store = DomainStore::open(&config, &data_dir, 1).unwrap();
    let outcome = store
        .remember(
            "Raymond",
            EntityType::Person,
            &[
                ObservationInput::new("Raymond prefers Rust for daemon work")
                    .with_title("Rust preference")
                    .with_source("test"),
                ObservationInput::new("Raymond is building OpenMemory Desktop")
                    .with_concepts(vec!["desktop".into(), "daemon".into()]),
            ],
            &[RelationInput::new(
                "builds",
                "OpenMemory Desktop",
                EntityType::Project,
            )],
            "test",
        )
        .unwrap();
    let project = store
        .get_entity_by_name_and_type("OpenMemory Desktop", EntityType::Project)
        .unwrap()
        .unwrap();
    drop(store);
    (outcome.entity_id, project.id)
}

fn seed_duplicate_profile(home: &Path) {
    let config = Config::default();
    config.save(home.join("config.toml")).unwrap();
    let data_dir = home.join("data").join("default");
    let store = DomainStore::open(&config, &data_dir, 1).unwrap();
    store
        .remember(
            "Duplicate",
            EntityType::Fact,
            &[
                ObservationInput::new("hello duplicate world"),
                ObservationInput::new("hello duplicate world"),
            ],
            &[],
            "test",
        )
        .unwrap();
}

async fn get_health(auth: Option<HeaderValue>) -> Response {
    get_health_for_config(test_config(), auth).await
}

async fn get_health_for_config(config: DaemonConfig, auth: Option<HeaderValue>) -> Response {
    request_for_config(config, Method::GET, "/admin/health", auth).await
}

async fn request_for_config(
    config: DaemonConfig,
    method: Method,
    uri: &str,
    auth: Option<HeaderValue>,
) -> Response {
    request_app(build_router(config), method, uri, auth).await
}

async fn request_app(
    app: Router,
    method: Method,
    uri: &str,
    auth: Option<HeaderValue>,
) -> Response {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(value) = auth {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn request_app_json(
    app: Router,
    method: Method,
    uri: &str,
    auth: Option<HeaderValue>,
    body: serde_json::Value,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(value) = auth {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn wait_for_job(app: Router, id: &str) -> AdminJob {
    for _ in 0..100 {
        let response = request_app(
            app.clone(),
            Method::GET,
            &format!("/admin/jobs/{id}"),
            Some(HeaderValue::from_static("Bearer secret")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let job: AdminJob = read_json(response).await;
        if matches!(job.state, AdminJobState::Succeeded | AdminJobState::Failed) {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

async fn read_json<T: serde::de::DeserializeOwned>(response: Response) -> T {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn admin_token_rejects_empty_or_whitespace() {
    assert!(matches!(
        AdminToken::new(""),
        Err(DaemonError::EmptyAdminToken)
    ));
    assert!(matches!(
        AdminToken::new(" \n\t "),
        Err(DaemonError::EmptyAdminToken)
    ));
}

#[test]
fn admin_token_trims_input_and_debug_redacts_secret() {
    let token = AdminToken::new("  secret  ").unwrap();

    assert!(token.matches("secret"));
    assert!(!token.matches("  secret  "));
    assert_eq!(
        format!("{token:?}"),
        r#"AdminToken { expected: "<redacted>" }"#
    );
    assert!(!format!("{token:?}").contains("secret"));
}

#[test]
fn constant_time_eq_handles_match_mismatch_and_length_mismatch() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"diff"));
    assert!(!constant_time_eq(b"same", b"same-but-longer"));
}

#[test]
fn daemon_config_accepts_ipv4_and_ipv6_loopback() {
    for addr in ["127.0.0.1:0", "[::1]:0"] {
        let config = DaemonConfig::new(
            addr.parse().unwrap(),
            AdminToken::new("secret").unwrap(),
            PathBuf::from("/tmp/openmemory-test"),
            "default",
        );
        assert!(config.is_ok(), "{addr} should be accepted");
    }
}

#[test]
fn daemon_config_rejects_non_loopback_bind_addresses() {
    for addr in ["0.0.0.0:0", "192.168.1.10:8080", "[::]:0"] {
        let result = DaemonConfig::new(
            addr.parse().unwrap(),
            AdminToken::new("secret").unwrap(),
            PathBuf::from("/tmp/openmemory-test"),
            "default",
        );

        assert!(
            matches!(result, Err(DaemonError::NonLoopbackBind(_))),
            "{addr} should be rejected",
        );
    }
}

#[test]
fn admin_token_path_lives_under_run_dir() {
    assert_eq!(
        admin_token_path(Path::new("/tmp/om")),
        PathBuf::from("/tmp/om").join("run").join("admin-token")
    );
}

#[test]
fn runtime_info_path_lives_under_run_dir() {
    assert_eq!(
        runtime_info_path(Path::new("/tmp/om")),
        PathBuf::from("/tmp/om").join("run").join("daemon.json")
    );
}

#[test]
fn runtime_info_contains_public_discovery_fields_only() {
    let config = test_config();
    let info = runtime_info(&config, "127.0.0.1:7821".parse().unwrap()).unwrap();

    assert_eq!(info.api_version, ADMIN_API_VERSION);
    assert_eq!(info.daemon_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.bind_addr, "127.0.0.1:7821");
    assert_eq!(info.admin_url, "http://127.0.0.1:7821");
    assert_eq!(info.home, "/tmp/openmemory-test");
    assert_eq!(info.active_profile, "default");
    assert_ne!(info.pid, 0);
    assert_ne!(info.started_at_unix_secs, 0);

    let json = serde_json::to_string(&info).unwrap();
    assert!(!json.contains("secret"));
    assert!(!json.contains("admin-token"));
}

#[test]
fn runtime_info_rejects_non_loopback_bound_address() {
    let config = test_config();
    let err = runtime_info(&config, "0.0.0.0:7821".parse().unwrap()).unwrap_err();

    assert!(matches!(err, DaemonError::NonLoopbackBind(_)));
}

#[test]
fn runtime_info_round_trips_and_can_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    let config = DaemonConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        AdminToken::new("secret").unwrap(),
        dir.path().to_path_buf(),
        "default",
    )
    .unwrap();
    let info = runtime_info(&config, "127.0.0.1:7811".parse().unwrap()).unwrap();

    assert!(read_runtime_info(dir.path()).unwrap().is_none());
    write_runtime_info(dir.path(), &info).unwrap();
    assert_eq!(read_runtime_info(dir.path()).unwrap(), Some(info));
    remove_runtime_info(dir.path()).unwrap();
    assert!(read_runtime_info(dir.path()).unwrap().is_none());
    remove_runtime_info(dir.path()).unwrap();
}

#[test]
fn invalid_runtime_info_json_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = runtime_info_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "not json").unwrap();

    let err = read_runtime_info(dir.path()).unwrap_err();

    assert!(matches!(err, DaemonError::RuntimeJson(_)));
}

#[test]
fn health_warns_when_active_profile_is_not_initialized() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();
    let config = test_config_with_home(dir.path().to_path_buf());

    let health = health_response(&config);

    assert_eq!(health.store.state, ComponentState::Warning);
    assert_eq!(
        health.store.code,
        Some(AdminErrorCode::ProfileNotInitialized)
    );
    assert_eq!(health.store.details["profile"], "default");
    assert_eq!(
        health.store.details["data_dir"],
        dir.path()
            .join("data")
            .join("default")
            .display()
            .to_string()
    );
}

#[test]
fn health_opens_initialized_profile_and_reports_store_counts() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();
    std::fs::create_dir_all(dir.path().join("data").join("default")).unwrap();
    let config = test_config_with_home(dir.path().to_path_buf());

    let health = health_response(&config);

    assert_eq!(health.store.state, ComponentState::Ok);
    assert_eq!(health.store.details["domains"], 1);
    assert_eq!(health.store.details["entities"], 0);
    assert_eq!(health.store.details["observations"], 0);
    assert_eq!(health.store.details["relations"], 0);
    assert_eq!(health.store.details["schema_version"], 2);
}

#[test]
fn health_marks_invalid_config_as_component_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "not valid = [").unwrap();
    let config = test_config_with_home(dir.path().to_path_buf());

    let health = health_response(&config);

    assert_eq!(health.store.state, ComponentState::Error);
    assert_eq!(health.store.code, Some(AdminErrorCode::ConfigInvalid));
    #[cfg(feature = "embeddings")]
    {
        assert_eq!(health.model.state, ComponentState::Error);
        assert_eq!(health.model.code, Some(AdminErrorCode::ConfigInvalid));
    }
    #[cfg(not(feature = "embeddings"))]
    assert_eq!(health.model.state, ComponentState::Unknown);
    assert!(health.store.details["error"].as_str().is_some());
}

#[cfg(feature = "embeddings")]
#[test]
fn health_warns_when_embedding_model_is_not_cached() {
    without_openmemory_model(|| {
        let dir = tempfile::tempdir().unwrap();
        Config::default()
            .save(dir.path().join("config.toml"))
            .unwrap();
        let config = test_config_with_home(dir.path().to_path_buf());

        let health = health_response(&config);

        assert_eq!(health.model.state, ComponentState::Warning);
        assert_eq!(health.model.code, Some(AdminErrorCode::ModelMissing));
        assert_eq!(health.model.details["downloaded"], false);
        assert_eq!(health.model.details["model"], "nomic-embed-text-v1.5");
    });
}

#[cfg(feature = "embeddings")]
#[test]
fn health_reports_cached_embedding_model() {
    without_openmemory_model(|| {
        let dir = tempfile::tempdir().unwrap();
        Config::default()
            .save(dir.path().join("config.toml"))
            .unwrap();
        let model_dir = dir.path().join("models").join("nomic-embed-text-v1.5");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("model.onnx"), b"model").unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), b"tokenizer").unwrap();
        let config = test_config_with_home(dir.path().to_path_buf());

        let health = health_response(&config);

        assert_eq!(health.model.state, ComponentState::Ok);
        assert_eq!(health.model.details["downloaded"], true);
        assert_eq!(health.model.details["model"], "nomic-embed-text-v1.5");
        assert_eq!(
            health.model.details["path"],
            model_dir.display().to_string()
        );
    });
}

#[cfg(feature = "embeddings")]
fn without_openmemory_model<T>(f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("OPENMEMORY_MODEL");
    std::env::remove_var("OPENMEMORY_MODEL");
    let result = f();
    match previous {
        Some(value) => std::env::set_var("OPENMEMORY_MODEL", value),
        None => std::env::remove_var("OPENMEMORY_MODEL"),
    }
    result
}

#[test]
fn token_file_is_created_then_reused() {
    let dir = tempfile::tempdir().unwrap();
    let first = load_or_create_admin_token(dir.path()).unwrap();
    let second = load_or_create_admin_token(dir.path()).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn existing_token_is_trimmed_when_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = admin_token_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, " saved-token \n").unwrap();

    let token = load_or_create_admin_token(dir.path()).unwrap();

    assert_eq!(token, "saved-token");
}

#[test]
fn empty_existing_token_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = admin_token_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "\n\t ").unwrap();

    let err = load_or_create_admin_token(dir.path()).unwrap_err();

    assert!(matches!(err, DaemonError::EmptyAdminToken));
}

#[test]
fn token_path_directory_is_reported_as_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = admin_token_path(dir.path());
    std::fs::create_dir_all(&path).unwrap();

    let err = load_or_create_admin_token(dir.path()).unwrap_err();

    assert!(matches!(err, DaemonError::RuntimeIo(_)));
}

#[test]
fn rotate_admin_token_replaces_existing_token() {
    let dir = tempfile::tempdir().unwrap();
    let first = load_or_create_admin_token(dir.path()).unwrap();
    let second = rotate_admin_token(dir.path()).unwrap();
    let stored = load_admin_token(dir.path()).unwrap().unwrap();

    assert_ne!(first, second);
    assert_eq!(second, stored);
    assert_eq!(second.len(), 64);
}

#[cfg(unix)]
#[test]
fn created_token_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let _ = load_or_create_admin_token(dir.path()).unwrap();
    let mode = std::fs::metadata(admin_token_path(dir.path()))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn rotated_token_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let _ = load_or_create_admin_token(dir.path()).unwrap();
    let _ = rotate_admin_token(dir.path()).unwrap();
    let mode = std::fs::metadata(admin_token_path(dir.path()))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o600);
}

#[test]
fn log_ring_redacts_secrets_and_keeps_capacity() {
    let ring = RedactedLogRing::new(2);
    let token = "0123456789abcdef0123456789abcdef";
    ring.push(
        AdminLogLevel::Info,
        "first",
        format!("token {token}"),
        serde_json::json!({
            "admin_token": token,
            "nested": { "password": "hunter2", "safe": "value" }
        }),
    );
    ring.push(
        AdminLogLevel::Warning,
        "second",
        "safe",
        serde_json::Value::Null,
    );
    ring.push(
        AdminLogLevel::Error,
        "third",
        "safe",
        serde_json::Value::Null,
    );

    let entries = ring.snapshot();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].event, "second");
    assert_eq!(entries[1].event, "third");

    let ring = RedactedLogRing::new(8);
    ring.push(
        AdminLogLevel::Info,
        "redacted",
        format!("token {token}"),
        serde_json::json!({
            "admin_token": token,
            "nested": { "password": "hunter2", "safe": "value" }
        }),
    );
    let entry = ring.snapshot().pop().unwrap();
    assert_eq!(entry.message, "token <redacted>");
    assert_eq!(entry.details["admin_token"], "<redacted>");
    assert_eq!(entry.details["nested"]["password"], "<redacted>");
    assert_eq!(entry.details["nested"]["safe"], "value");
}

#[tokio::test]
async fn bind_listener_binds_loopback_ephemeral_port() {
    let config = test_config();
    let listener = bind_listener(&config).await.unwrap();
    let addr = listener.local_addr().unwrap();

    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0);
}

#[tokio::test]
async fn bind_listener_rejects_non_loopback_even_if_config_is_not_constructible() {
    let config = DaemonConfig {
        bind_addr: "0.0.0.0:0".parse().unwrap(),
        admin_token: AdminToken::new("secret").unwrap(),
        home: PathBuf::from("/tmp/openmemory-test"),
        active_profile: "default".into(),
    };

    let err = bind_listener(&config).await.unwrap_err();

    assert!(matches!(err, DaemonError::NonLoopbackBind(_)));
}

#[tokio::test]
async fn health_requires_auth() {
    let response = get_health(None).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::AuthRequired);
    assert!(!payload.error.retryable);
    assert!(payload.error.hint.is_some());
}

#[tokio::test]
async fn health_rejects_wrong_auth_scheme() {
    let response = get_health(Some(HeaderValue::from_static("Basic secret"))).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::AuthInvalid);
}

#[tokio::test]
async fn health_rejects_wrong_bearer_token() {
    let response = get_health(Some(HeaderValue::from_static("Bearer wrong"))).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::AuthInvalid);
}

#[tokio::test]
async fn health_rejects_non_utf8_authorization_header() {
    let response = get_health(Some(HeaderValue::from_bytes(b"Bearer \xff").unwrap())).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::AuthInvalid);
}

#[tokio::test]
async fn health_accepts_valid_auth_with_outer_token_whitespace() {
    let response = get_health(Some(HeaderValue::from_static("Bearer secret \t"))).await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_accepts_valid_auth() {
    let dir = tempfile::tempdir().unwrap();
    let config = test_config_with_home(dir.path().to_path_buf());
    let response =
        get_health_for_config(config, Some(HeaderValue::from_static("Bearer secret"))).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    let payload: HealthResponse = read_json(response).await;
    assert_eq!(payload.api_version, ADMIN_API_VERSION);
    assert_eq!(payload.daemon_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(payload.daemon.state, ComponentState::Ok);
    assert_eq!(payload.active_profile, "default");
    assert_eq!(payload.home, dir.path().display().to_string());
    assert_eq!(payload.store.state, ComponentState::Warning);
    assert_eq!(
        payload.store.code,
        Some(AdminErrorCode::ProfileNotInitialized)
    );
    #[cfg(feature = "embeddings")]
    assert_eq!(payload.model.state, ComponentState::Warning);
    #[cfg(not(feature = "embeddings"))]
    assert_eq!(payload.model.state, ComponentState::Unknown);
    assert_eq!(payload.mcp.state, ComponentState::Ok);
    assert_eq!(payload.watcher.state, ComponentState::Ok);
    assert_eq!(payload.jobs.state, ComponentState::Ok);
    assert_eq!(payload.integrations, IntegrationSummary::default());
}

#[tokio::test]
async fn doctor_endpoint_returns_typed_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();
    let response = request_for_config(
        test_config_with_home(dir.path().to_path_buf()),
        Method::GET,
        "/admin/doctor",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminDoctorResponse = read_json(response).await;
    assert_eq!(
        payload.health.store.code,
        Some(AdminErrorCode::ProfileNotInitialized)
    );
    assert!(payload
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == AdminErrorCode::ProfileNotInitialized));
}

#[tokio::test]
async fn health_is_computed_for_each_request() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();
    let app = build_router(test_config_with_home(dir.path().to_path_buf()));

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/health")
                .header(header::AUTHORIZATION, "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let first: HealthResponse = read_json(first).await;
    assert_eq!(first.store.state, ComponentState::Warning);

    std::fs::create_dir_all(dir.path().join("data").join("default")).unwrap();
    let second = app
        .oneshot(
            Request::builder()
                .uri("/admin/health")
                .header(header::AUTHORIZATION, "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let second: HealthResponse = read_json(second).await;
    assert_eq!(second.store.state, ComponentState::Ok);
}

#[tokio::test]
async fn rotate_token_endpoint_switches_auth_token_without_returning_secret() {
    let dir = tempfile::tempdir().unwrap();
    let old = load_or_create_admin_token(dir.path()).unwrap();
    let config = DaemonConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        AdminToken::new(&old).unwrap(),
        dir.path().to_path_buf(),
        "default",
    )
    .unwrap();
    let app = build_router(config);

    let response = request_app(
        app.clone(),
        Method::POST,
        "/admin/auth/rotate",
        Some(HeaderValue::from_str(&format!("Bearer {old}")).unwrap()),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!text.contains(&old));
    let payload: AdminTokenRotationResponse = serde_json::from_str(&text).unwrap();
    assert_ne!(payload.rotated_at_unix_secs, 0);

    let new = load_admin_token(dir.path()).unwrap().unwrap();
    assert_ne!(old, new);

    let old_response = request_app(
        app.clone(),
        Method::GET,
        "/admin/health",
        Some(HeaderValue::from_str(&format!("Bearer {old}")).unwrap()),
    )
    .await;
    assert_eq!(old_response.status(), StatusCode::UNAUTHORIZED);

    let new_response = request_app(
        app.clone(),
        Method::GET,
        "/admin/health",
        Some(HeaderValue::from_str(&format!("Bearer {new}")).unwrap()),
    )
    .await;
    assert_eq!(new_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn logs_endpoint_is_authenticated_and_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let token = load_or_create_admin_token(dir.path()).unwrap();
    let config = DaemonConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        AdminToken::new(&token).unwrap(),
        dir.path().to_path_buf(),
        "default",
    )
    .unwrap();
    let app = build_router(config);

    let unauthorized = request_app(app.clone(), Method::GET, "/admin/logs", None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = request_app(
        app,
        Method::GET,
        "/admin/logs",
        Some(HeaderValue::from_str(&format!("Bearer {token}")).unwrap()),
    )
    .await;

    assert_eq!(authorized.status(), StatusCode::OK);
    let body = to_bytes(authorized.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!text.contains(&token));
    let payload: AdminLogsResponse = serde_json::from_str(&text).unwrap();
    assert!(payload
        .entries
        .iter()
        .any(|entry| entry.event == "admin_router_ready"));
}

#[tokio::test]
async fn profiles_endpoint_lists_active_profile_health() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let response = request_for_config(
        test_config_with_home(dir.path().to_path_buf()),
        Method::GET,
        "/admin/profiles",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminProfilesResponse = read_json(response).await;
    assert_eq!(payload.active_profile, "default");
    assert_eq!(payload.profiles.len(), 1);
    assert_eq!(payload.profiles[0].name, "default");
    assert!(payload.profiles[0].active);
    assert!(payload.profiles[0].initialized);
    assert_eq!(payload.profiles[0].health.state, ComponentState::Ok);
}

#[tokio::test]
async fn entities_endpoint_lists_pages_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let config = test_config_with_home(dir.path().to_path_buf());

    let page = request_for_config(
        config.clone(),
        Method::GET,
        "/admin/entities?limit=1",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(page.status(), StatusCode::OK);
    let page: Page<AdminEntitySummary> = read_json(page).await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next_offset, Some(1));

    let filtered = request_for_config(
        config.clone(),
        Method::GET,
        "/admin/entities?entity_type=project",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered: Page<AdminEntitySummary> = read_json(filtered).await;
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].name, "OpenMemory Desktop");
    assert_eq!(filtered.items[0].entity_type, "project");

    let invalid = request_for_config(
        config,
        Method::GET,
        "/admin/entities?entity_type=ghost",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let payload: AdminErrorResponse = read_json(invalid).await;
    assert_eq!(payload.error.code, AdminErrorCode::InvalidRequest);
}

#[tokio::test]
async fn entity_detail_endpoint_returns_observations_and_relations() {
    let dir = tempfile::tempdir().unwrap();
    let (raymond_id, _) = seed_default_profile(dir.path());
    let response = request_for_config(
        test_config_with_home(dir.path().to_path_buf()),
        Method::GET,
        &format!("/admin/entities/{raymond_id}"),
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminEntityDetail = read_json(response).await;
    assert_eq!(payload.entity.id, raymond_id);
    assert_eq!(payload.entity.name, "Raymond");
    assert_eq!(payload.entity.observation_count, 2);
    assert_eq!(payload.observations.len(), 2);
    assert!(payload
        .observations
        .iter()
        .any(|observation| observation.title.as_deref() == Some("Rust preference")));
    assert_eq!(payload.relations.len(), 1);
    assert_eq!(payload.relations[0].relation_type, "builds");
}

#[tokio::test]
async fn entity_detail_endpoint_returns_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let response = request_for_config(
        test_config_with_home(dir.path().to_path_buf()),
        Method::GET,
        "/admin/entities/not-a-real-id",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::EntityNotFound);
}

#[tokio::test]
async fn search_endpoint_recalls_memory_and_validates_request() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let config = test_config_with_home(dir.path().to_path_buf());

    let response = request_for_config(
        config.clone(),
        Method::GET,
        "/admin/search?q=Rust&limit=5&mode=keyword",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload: Page<AdminSearchResult> = read_json(response).await;
    assert!(!payload.items.is_empty());
    assert_eq!(payload.items[0].entity_name, "Raymond");
    assert_eq!(payload.items[0].entity_type, "person");
    assert!(payload.items[0].observation.content.contains("Rust"));

    let empty = request_for_config(
        config,
        Method::GET,
        "/admin/search?q=",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
    let payload: AdminErrorResponse = read_json(empty).await;
    assert_eq!(payload.error.code, AdminErrorCode::InvalidRequest);
}

#[tokio::test]
async fn consolidate_endpoint_starts_job_and_stores_report() {
    let dir = tempfile::tempdir().unwrap();
    seed_duplicate_profile(dir.path());
    let app = build_router(test_config_with_home(dir.path().to_path_buf()));

    let response = request_app(
        app.clone(),
        Method::POST,
        "/admin/consolidate",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let job: AdminJob = read_json(response).await;
    assert_eq!(job.kind, AdminJobKind::Consolidate);
    assert_eq!(job.state, AdminJobState::Queued);

    let job = wait_for_job(app, &job.id).await;
    assert_eq!(job.state, AdminJobState::Succeeded);
    assert_eq!(job.result["duplicates_merged"], 1);
    assert!(job.finished_at_unix_secs.is_some());
}

#[tokio::test]
async fn consolidate_endpoint_rejects_uninitialized_profile() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();
    let response = request_for_config(
        test_config_with_home(dir.path().to_path_buf()),
        Method::POST,
        "/admin/consolidate",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::ProfileNotInitialized);
}

#[tokio::test]
async fn job_endpoint_returns_not_found() {
    let response = request_for_config(
        test_config(),
        Method::GET,
        "/admin/jobs/missing",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::JobNotFound);
}

#[test]
fn job_registry_persists_jobs_and_events_for_replay() {
    let dir = tempfile::tempdir().unwrap();
    let registry = JobRegistry::open(dir.path());
    let job = AdminJob {
        id: "job-1".into(),
        kind: AdminJobKind::Consolidate,
        state: AdminJobState::Queued,
        profile: "default".into(),
        created_at_unix_secs: 10,
        started_at_unix_secs: None,
        finished_at_unix_secs: None,
        message: Some("queued".into()),
        result: serde_json::Value::Null,
        error: None,
    };
    registry.insert(job.clone());
    registry.update("job-1", |job| {
        job.state = AdminJobState::Succeeded;
        job.finished_at_unix_secs = Some(11);
        job.message = Some("done".into());
    });

    let reopened = JobRegistry::open(dir.path());
    let persisted = reopened.get("job-1").expect("persisted job");
    assert_eq!(persisted.state, AdminJobState::Succeeded);
    assert_eq!(persisted.finished_at_unix_secs, Some(11));

    let events = reopened.events_after(0, 10);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].sequence, 1);
    assert_eq!(events[1].sequence, 2);
    assert_eq!(reopened.events_after(1, 10).len(), 1);
    assert_eq!(reopened.health().state, ComponentState::Ok);
    assert_eq!(reopened.health().details["durable"], true);
}

#[test]
fn job_registry_rejects_future_product_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let product_dir = dir.path().join("product");
    std::fs::create_dir_all(&product_dir).unwrap();
    let conn = rusqlite::Connection::open(product_dir.join("product.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE product_meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         INSERT INTO product_meta(key, value)
         VALUES('schema_version', '2');",
    )
    .unwrap();
    drop(conn);

    let registry = JobRegistry::open(dir.path());
    let health = registry.health();
    assert_eq!(health.state, ComponentState::Error);
    assert_eq!(health.details["durable"], false);
    assert!(health.details["error"]
        .as_str()
        .unwrap()
        .contains("newer than supported"));
}

#[tokio::test]
async fn job_endpoint_reads_persisted_job_after_router_restart() {
    let dir = tempfile::tempdir().unwrap();
    seed_duplicate_profile(dir.path());
    let config = test_config_with_home(dir.path().to_path_buf());
    let app = build_router(config.clone());

    let response = request_app(
        app.clone(),
        Method::POST,
        "/admin/consolidate",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let job: AdminJob = read_json(response).await;
    let job = wait_for_job(app, &job.id).await;
    assert_eq!(job.state, AdminJobState::Succeeded);

    let restarted = build_router(config);
    let response = request_app(
        restarted,
        Method::GET,
        &format!("/admin/jobs/{}", job.id),
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let persisted: AdminJob = read_json(response).await;
    assert_eq!(persisted.id, job.id);
    assert_eq!(persisted.state, AdminJobState::Succeeded);
}

#[tokio::test]
async fn events_endpoint_requires_auth_and_returns_sse() {
    let app = build_router(test_config());

    let unauthorized = request_app(app.clone(), Method::GET, "/admin/events", None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = request_app(
        app,
        Method::GET,
        "/admin/events",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(authorized.status(), StatusCode::OK);
    assert!(authorized
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
}

#[tokio::test]
async fn shutdown_endpoint_requires_auth_and_signals_server_shutdown() {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let app = build_router_with_shutdown(test_config(), Some(shutdown_tx));

    let unauthorized = request_app(app.clone(), Method::POST, "/admin/shutdown", None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(!*shutdown_rx.borrow());

    let authorized = request_app(
        app,
        Method::POST,
        "/admin/shutdown",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;
    assert_eq!(authorized.status(), StatusCode::OK);
    let payload: AdminShutdownResponse = read_json(authorized).await;
    assert!(payload.accepted);
    shutdown_rx.changed().await.unwrap();
    assert!(*shutdown_rx.borrow());
}

#[tokio::test]
async fn integrations_endpoint_lists_supported_clients() {
    let response = request_for_config(
        test_config(),
        Method::GET,
        "/admin/integrations",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminIntegrationsResponse = read_json(response).await;
    assert_eq!(payload.integrations.len(), 2);
    assert!(payload
        .integrations
        .iter()
        .any(|row| row.client == AdminIntegrationClient::Codex));
    assert!(payload
        .integrations
        .iter()
        .any(|row| row.client == AdminIntegrationClient::ClaudeCode));
}

#[tokio::test]
async fn codex_integration_preview_install_and_verify_job() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("codex").join("config.toml");
    let app = build_router(test_config_with_home(dir.path().join("om")));
    let body = serde_json::json!({ "config_path": config_path.display().to_string() });

    let preview = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/integrations/codex/preview",
        Some(HeaderValue::from_static("Bearer secret")),
        body.clone(),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview: AdminIntegrationPreview = read_json(preview).await;
    assert_eq!(preview.outcome, AdminIntegrationOutcome::Created);
    assert!(preview.after.contains("[mcp_servers.openmemory]"));
    assert!(!config_path.exists());

    let install = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/integrations/codex/install",
        Some(HeaderValue::from_static("Bearer secret")),
        body.clone(),
    )
    .await;
    assert_eq!(install.status(), StatusCode::OK);
    let install: AdminIntegrationInstallResponse = read_json(install).await;
    assert!(install.changed);
    assert!(config_path.exists());

    let second = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/integrations/codex/preview",
        Some(HeaderValue::from_static("Bearer secret")),
        body.clone(),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let second: AdminIntegrationPreview = read_json(second).await;
    assert_eq!(second.outcome, AdminIntegrationOutcome::Unchanged);

    let verify = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/integrations/codex/verify",
        Some(HeaderValue::from_static("Bearer secret")),
        body,
    )
    .await;
    assert_eq!(verify.status(), StatusCode::OK);
    let job: AdminJob = read_json(verify).await;
    assert_eq!(job.kind, AdminJobKind::IntegrationVerify);
    let job = wait_for_job(app, &job.id).await;
    assert_eq!(job.state, AdminJobState::Succeeded);
    assert_eq!(job.result["configured"], true);
}

#[tokio::test]
async fn claude_code_integration_install_writes_json() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join(".claude.json");
    let app = build_router(test_config_with_home(dir.path().join("om")));
    let body = serde_json::json!({ "config_path": config_path.display().to_string() });

    let install = request_app_json(
        app,
        Method::POST,
        "/admin/integrations/claude-code/install",
        Some(HeaderValue::from_static("Bearer secret")),
        body,
    )
    .await;

    assert_eq!(install.status(), StatusCode::OK);
    let install: AdminIntegrationInstallResponse = read_json(install).await;
    assert!(install.changed);
    assert_eq!(install.preview.client, AdminIntegrationClient::ClaudeCode);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(written["mcpServers"]["openmemory"]["command"], "openmemory");
    assert_eq!(
        written["mcpServers"]["openmemory"]["env"]["OPENMEMORY_PROFILE"],
        "default"
    );
}

#[tokio::test]
async fn integration_unknown_client_returns_not_found() {
    let response = request_for_config(
        test_config(),
        Method::POST,
        "/admin/integrations/unknown/preview",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::ClientNotFound);
}

#[tokio::test]
async fn backup_preflight_reports_ready_for_initialized_profile() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let app = build_router(test_config_with_home(dir.path().to_path_buf()));
    let destination = dir.path().join("backups");

    let response = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/backup/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "destination_dir": destination.display().to_string() }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminBackupPreflightResponse = read_json(response).await;
    assert!(payload.ready, "{:?}", payload.diagnostics);
    assert!(payload.estimated_files > 0);
    assert!(payload.estimated_bytes > 0);
    assert_eq!(payload.destination_dir, destination.display().to_string());
    assert!(payload.diagnostics.is_empty());

    let alias = request_app_json(
        app,
        Method::POST,
        "/admin/backups/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "destination_dir": destination.display().to_string() }),
    )
    .await;
    assert_eq!(alias.status(), StatusCode::OK);
}

#[tokio::test]
async fn backup_preflight_reports_uninitialized_profile() {
    let dir = tempfile::tempdir().unwrap();
    Config::default()
        .save(dir.path().join("config.toml"))
        .unwrap();

    let response = request_app(
        build_router(test_config_with_home(dir.path().to_path_buf())),
        Method::POST,
        "/admin/backup/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload: AdminBackupPreflightResponse = read_json(response).await;
    assert!(!payload.ready);
    assert!(payload
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == AdminErrorCode::ProfileNotInitialized));
}

#[tokio::test]
async fn backup_create_rejects_destination_inside_profile_data() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let destination = dir.path().join("data").join("default").join("backups");

    let response = request_app_json(
        build_router(test_config_with_home(dir.path().to_path_buf())),
        Method::POST,
        "/admin/backup/create",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "destination_dir": destination.display().to_string() }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload: AdminErrorResponse = read_json(response).await;
    assert_eq!(payload.error.code, AdminErrorCode::BackupPreflightFailed);
}

#[tokio::test]
async fn backup_create_job_writes_manifest_and_artifact_opens_after_copy() {
    let dir = tempfile::tempdir().unwrap();
    seed_default_profile(dir.path());
    let app = build_router(test_config_with_home(dir.path().to_path_buf()));
    let destination = dir.path().join("backups");

    let response = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/backup/create",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "destination_dir": destination.display().to_string() }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let job: AdminJob = read_json(response).await;
    assert_eq!(job.kind, AdminJobKind::BackupCreate);
    assert_eq!(job.state, AdminJobState::Queued);

    let job = wait_for_job(app.clone(), &job.id).await;
    assert_eq!(job.state, AdminJobState::Succeeded, "{:?}", job.error);
    let report: AdminBackupCreateReport = serde_json::from_value(job.result).unwrap();
    assert!(Path::new(&report.backup_dir).is_dir());
    assert!(Path::new(&report.manifest_path).is_file());
    assert_eq!(report.manifest.profile, "default");
    assert!(report.manifest.files_copied > 0);
    assert!(std::fs::read_dir(&destination).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));

    let restore = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/restore/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "backup_dir": report.backup_dir }),
    )
    .await;
    assert_eq!(restore.status(), StatusCode::OK);
    let restore: AdminRestorePreflightResponse = read_json(restore).await;
    assert!(restore.ready, "{:?}", restore.diagnostics);
    assert_eq!(restore.manifest.unwrap(), report.manifest);

    let conflict = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/restore",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "backup_dir": report.backup_dir }),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let restore_job = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/restore",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({
            "backup_dir": report.backup_dir,
            "target_profile": "restored"
        }),
    )
    .await;
    assert_eq!(restore_job.status(), StatusCode::OK);
    let restore_job: AdminJob = read_json(restore_job).await;
    assert_eq!(restore_job.kind, AdminJobKind::Restore);
    let restore_job = wait_for_job(app, &restore_job.id).await;
    assert_eq!(
        restore_job.state,
        AdminJobState::Succeeded,
        "{:?}",
        restore_job.error
    );
    let restore_report: AdminRestoreReport = serde_json::from_value(restore_job.result).unwrap();
    assert_eq!(restore_report.restored_profile, "restored");
    assert!(!restore_report.replaced_existing);

    let restored_store =
        DomainStore::open_existing(&Config::default(), Path::new(&restore_report.restored_dir))
            .unwrap();
    assert!(restored_store
        .get_entity_by_name_and_type("Raymond", EntityType::Person)
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn restore_preflight_rejects_missing_invalid_and_truncated_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(test_config_with_home(dir.path().to_path_buf()));
    let missing = dir.path().join("missing");

    let missing_response = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/restore/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "backup_dir": missing.display().to_string() }),
    )
    .await;
    assert_eq!(missing_response.status(), StatusCode::OK);
    let missing_payload: AdminRestorePreflightResponse = read_json(missing_response).await;
    assert!(!missing_payload.ready);
    assert!(missing_payload
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == AdminErrorCode::RestorePreflightFailed));

    let invalid = dir.path().join("invalid");
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(invalid.join("openmemory-backup.json"), "{not-json").unwrap();
    let invalid_response = request_app_json(
        app.clone(),
        Method::POST,
        "/admin/restore/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "backup_dir": invalid.display().to_string() }),
    )
    .await;
    assert_eq!(invalid_response.status(), StatusCode::OK);
    let invalid_payload: AdminRestorePreflightResponse = read_json(invalid_response).await;
    assert!(!invalid_payload.ready);

    let truncated = dir.path().join("truncated");
    std::fs::create_dir_all(&truncated).unwrap();
    let manifest = AdminBackupManifest {
        api_version: ADMIN_API_VERSION.to_string(),
        profile: "default".into(),
        created_at_unix_secs: 1,
        source_dir: "/tmp/source".into(),
        files_copied: 10,
        bytes_copied: 128,
    };
    std::fs::write(
        truncated.join("openmemory-backup.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();
    let truncated_response = request_app_json(
        app,
        Method::POST,
        "/admin/restore/preflight",
        Some(HeaderValue::from_static("Bearer secret")),
        serde_json::json!({ "backup_dir": truncated.display().to_string() }),
    )
    .await;
    assert_eq!(truncated_response.status(), StatusCode::OK);
    let truncated_payload: AdminRestorePreflightResponse = read_json(truncated_response).await;
    assert!(!truncated_payload.ready);
    assert!(truncated_payload.manifest.is_some());
}

#[tokio::test]
async fn health_payload_does_not_expose_admin_token() {
    let response = get_health(Some(HeaderValue::from_static("Bearer secret"))).await;
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();

    assert!(!text.contains("secret"));
    assert!(!text.contains("admin-token"));
}

#[tokio::test]
async fn unknown_admin_route_returns_404() {
    let response = build_router(test_config())
        .oneshot(
            Request::builder()
                .uri("/admin/not-found")
                .header(header::AUTHORIZATION, "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn admin_contract_helpers_round_trip() {
    let error = AdminError::new(
        AdminErrorCode::Conflict,
        "conflict",
        Option::<String>::None,
        true,
    )
    .with_details(serde_json::json!({ "field": "profile" }));
    let value = serde_json::to_value(AdminErrorResponse::new(error)).unwrap();

    assert_eq!(value["error"]["code"], "conflict");
    assert_eq!(value["error"]["details"]["field"], "profile");
    assert_eq!(
        ComponentHealth::warning(AdminErrorCode::ModelMissing, "missing").state,
        ComponentState::Warning
    );
    assert_eq!(
        ComponentHealth::error(AdminErrorCode::StoreUnreadable, "bad").state,
        ComponentState::Error
    );
    assert_eq!(PageRequest::default().limit, 50);
    assert_eq!(Page::<u8>::new(vec![1, 2], Some(2)).next_offset, Some(2));
}
