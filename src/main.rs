//! DAP Shim Server
//!
//! This is a Debug Adapter Protocol (DAP) shim that acts as a middleman between a DAP client
//! and a subprocess DAP server. It handles the initialize request, spawns a subprocess via
//! runInTerminal, and then proxies all subsequent DAP messages between the client and subprocess.
//!
//! Key features:
//! - Communicates with client via stdin/stdout
//! - Responds to initialize request with DAP capabilities
//! - Sends initialized event and runInTerminal request to spawn subprocess
//! - Buffers requests that arrive before subprocess is ready
//! - Forwards all subsequent requests to subprocess DAP via TCP on port 4712
//! - Proxies responses back from subprocess to client

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{stdin, stdout, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

#[derive(Debug)]
struct DapShim {
    seq: i64,
    subdap_writer: Option<tokio::net::tcp::OwnedWriteHalf>,
    buffered_requests: VecDeque<Value>,
    initialized: bool,
}

impl DapShim {
    fn new() -> Self {
        Self {
            seq: 0,
            subdap_writer: None,
            buffered_requests: VecDeque::new(),
            initialized: false,
        }
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    async fn send_to_subdap(
        &mut self,
        msg: &Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(writer) = &mut self.subdap_writer {
            let msg_bytes = serde_json::to_vec(msg)?;
            writer
                .write_all(format!("Content-Length: {}\r\n\r\n", msg_bytes.len()).as_bytes())
                .await?;
            writer.write_all(&msg_bytes).await?;
            writer.flush().await?;
        }
        Ok(())
    }

    async fn handle_initialize(
        &mut self,
        request: Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let seq = request["seq"].as_i64().unwrap_or(0);

        // Send initialize response
        let response = json!({
            "seq": self.next_seq(),
            "type": "response",
            "request_seq": seq,
            "success": true,
            "command": "initialize",
            "body": {
                "supportsConfigurationDoneRequest": true,
                "supportsConditionalBreakpoints": true,
                "supportsDelayedStackTraceLoading": true,
                "supportsFunctionBreakpoints": true,
                "supportsInstructionBreakpoints": true,
                "supportsExceptionInfoRequest": true,
                "supportsSetVariable": true,
                "supportsEvaluateForHovers": true,
                "supportsClipboardContext": true,
                "supportsSteppingGranularity": true,
                "supportsLogPoints": true,
                "supportsDisassembleRequest": true,
                "supportsStepBack": false,
                "supportTerminateDebuggee": false,
                "supportsTerminateRequest": false,
                "supportsRestartRequest": false,
                "supportsSetExpression": false,
                "supportsLoadedSourcesRequest": false,
                "supportsReadMemoryRequest": false,
                "supportsCancelRequest": false
            }
        });

        send_dap_message_stdout(&response).await?;

        // Send initialized event
        let initialized_event = json!({
            "seq": self.next_seq(),
            "type": "event",
            "event": "initialized"
        });
        send_dap_message_stdout(&initialized_event).await?;

        // Send runInTerminal request
        let run_in_terminal = json!({
            "seq": self.next_seq(),
            "type": "request",
            "command": "runInTerminal",
            "arguments": {
                "kind": "integrated",
                "title": "Debug Process",
                "cwd": ".",
                "args": ["TODO: Fill in subprocess args here"],
                "env": {}
            }
        });
        send_dap_message_stdout(&run_in_terminal).await?;

        // Buffer the initialize request to forward to subdap
        self.buffered_requests.push_back(request);
        
        self.initialized = true;
        Ok(())
    }

    async fn handle_request(
        &mut self,
        request: Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let command = request["command"].as_str().unwrap_or("");

        match command {
            "initialize" => {
                self.handle_initialize(request).await?;
            }
            _ => {
                // Buffer or forward the request
                if self.subdap_writer.is_none() {
                    self.buffered_requests.push_back(request);
                } else {
                    self.send_to_subdap(&request).await?;
                }
            }
        }
        Ok(())
    }

    async fn flush_buffered_requests(
        &mut self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        while let Some(req) = self.buffered_requests.pop_front() {
            self.send_to_subdap(&req).await?;
        }
        Ok(())
    }
}

async fn send_dap_message_stdout(
    msg: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg_bytes = serde_json::to_vec(msg)?;
    let mut stdout = stdout();
    stdout
        .write_all(format!("Content-Length: {}\r\n\r\n", msg_bytes.len()).as_bytes())
        .await?;
    stdout.write_all(&msg_bytes).await?;
    stdout.flush().await?;
    Ok(())
}

async fn read_dap_message(
    reader: &mut BufReader<impl AsyncReadExt + Unpin>,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let mut headers = String::new();

    // Read headers
    loop {
        headers.clear();
        loop {
            let bytes_read = reader.read_line(&mut headers).await?;
            if bytes_read == 0 {
                return Err("Connection closed".into());
            }
            if headers.ends_with("\r\n\r\n") {
                break;
            }
        }

        // Parse Content-Length
        let content_length = headers
            .lines()
            .find(|line| line.starts_with("Content-Length:"))
            .and_then(|line| line.split(':').nth(1))
            .and_then(|len| len.trim().parse::<usize>().ok())
            .ok_or("Missing or invalid Content-Length")?;

        // Read body
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;

        // Parse request
        let msg: Value = serde_json::from_slice(&body)?;
        return Ok(msg);
    }
}

async fn subdap_connect_and_proxy(shim: Arc<Mutex<DapShim>>) {
    // Try to connect to subdap
    let mut attempts = 0;
    let stream = loop {
        attempts += 1;
        match TcpStream::connect("127.0.0.1:4712").await {
            Ok(s) => break s,
            Err(_) if attempts < 20 => {
                sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => {
                eprintln!(
                    "Failed to connect to subdap after {} attempts: {}",
                    attempts, e
                );
                return;
            }
        }
    };

    eprintln!("Connected to subdap on port 4712");
    let (reader, writer) = stream.into_split();

    // Store writer and flush buffered requests
    {
        let mut shim_lock = shim.lock().await;
        shim_lock.subdap_writer = Some(writer);
        if let Err(e) = shim_lock.flush_buffered_requests().await {
            eprintln!("Error flushing buffered requests: {}", e);
        }
    }

    // Proxy responses from subdap to stdout
    let mut reader = BufReader::new(reader);
    loop {
        match read_dap_message(&mut reader).await {
            Ok(msg) => {
                // Skip initialize response from subdap since we already responded
                if msg.get("type") == Some(&json!("response")) 
                    && msg.get("command") == Some(&json!("initialize")) {
                    continue;
                }
                
                if let Err(e) = send_dap_message_stdout(&msg).await {
                    eprintln!("Error forwarding to stdout: {}", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Error reading from subdap: {}", e);
                break;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let shim = Arc::new(Mutex::new(DapShim::new()));

    // Spawn task to connect to subdap once initialized
    let shim_clone = shim.clone();
    tokio::spawn(async move {
        // Wait for initialization
        loop {
            {
                let shim_lock = shim_clone.lock().await;
                if shim_lock.initialized {
                    break;
                }
            }
            sleep(Duration::from_millis(100)).await;
        }

        subdap_connect_and_proxy(shim_clone).await;
    });

    // Read requests from stdin
    let stdin = stdin();
    let mut reader = BufReader::new(stdin);

    loop {
        match read_dap_message(&mut reader).await {
            Ok(msg) => {
                let mut shim_lock = shim.lock().await;
                if let Err(e) = shim_lock.handle_request(msg).await {
                    eprintln!("Error handling request: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Error reading from stdin: {}", e);
                break;
            }
        }
    }

    Ok(())
}
