//! Newline-delimited JSON-RPC 2.0 transport over stdio.
//!
//! MCP-over-stdio frames each JSON-RPC message as a single line. We read
//! line by line from `stdin`, dispatch through [`OpenMemoryMcpServer::handle`],
//! and write each response (newline-terminated) to `stdout`. Logs go to
//! `stderr` so the protocol stream stays clean.
//!
//! Generic over `AsyncRead + AsyncWrite` so tests can drive the loop with
//! `tokio::io::DuplexStream` instead of real stdio handles.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
// AsyncWriteExt provides `write_all` and `flush`; needed in scope.

use crate::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::OpenMemoryMcpServer;

/// Run one server-loop iteration over `read`/`write` until `read` reaches
/// EOF. Each line is parsed as a JSON-RPC 2.0 message; responses are written
/// back terminated by `\n`. Notifications produce no response. Parse errors
/// produce a JSON-RPC `-32700` response with no `id`.
pub async fn run<R, W>(server: OpenMemoryMcpServer, read: R, mut write: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(read);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => server.handle(req),
            Err(e) => Some(JsonRpcResponse::error(
                None,
                JsonRpcError::parse_error(format!("parse error: {e}")),
            )),
        };

        if let Some(response) = response {
            let mut s = serde_json::to_string(&response)?;
            s.push('\n');
            write.write_all(s.as_bytes()).await?;
            write.flush().await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use open_memory_core::config::Config;
    use open_memory_graph::MemoryStore;
    use tokio::io::AsyncReadExt;

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    /// Pump a single request through the stdio loop and return the response
    /// line (no trailing newline). Builds two duplex streams so we can
    /// drive the server end-to-end.
    async fn one_request(request: &str) -> String {
        let server = server();
        let (client_to_server, server_in) = tokio::io::duplex(8192);
        let (server_out, mut client_from_server) = tokio::io::duplex(8192);

        let join = tokio::spawn(async move {
            let _ = run(server, server_in, server_out).await;
        });

        // Write request and close the input side so the server loop exits.
        let mut client_to_server = client_to_server;
        let mut buf = request.to_string();
        if !buf.ends_with('\n') {
            buf.push('\n');
        }
        client_to_server.write_all(buf.as_bytes()).await.unwrap();
        drop(client_to_server);

        let mut s = String::new();
        client_from_server.read_to_string(&mut s).await.unwrap();
        join.await.unwrap();
        s.trim().to_string()
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_tools_capability() {
        let resp =
            one_request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).await;
        assert!(
            resp.contains("\"protocolVersion\":\"2024-11-05\"")
                || resp.contains("\"protocol_version\":\"2024-11-05\"")
        );
        assert!(resp.contains("\"tools\""));
        assert!(resp.contains("\"name\":\"open-memory\""));
    }

    #[tokio::test]
    async fn tools_list_advertises_registered_tools() {
        let resp =
            one_request(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#).await;
        // Sanity-check that `tools/list` returns the registered set rather
        // than an empty advert; specific tool coverage lives in
        // `tools::tests::registry_lists_all_eleven_tools`.
        assert!(resp.contains("\"tools\""));
        assert!(resp.contains("open_memory_status"));
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let resp = one_request(r#"{"jsonrpc":"2.0","id":3,"method":"foo/bar","params":{}}"#).await;
        assert!(resp.contains("-32601"));
    }

    #[tokio::test]
    async fn parse_error_returns_minus_32700_with_null_id() {
        let resp = one_request("{invalid json").await;
        assert!(resp.contains("-32700"));
        assert!(resp.contains("\"id\":null"));
    }

    #[tokio::test]
    async fn notification_produces_no_response() {
        let resp =
            one_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#)
                .await;
        assert!(
            resp.is_empty(),
            "notification should produce no response: {resp}"
        );
    }
}
