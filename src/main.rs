use debugserver_types::*;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Duration};

#[derive(Debug)]
struct DapShim {
    seq: i64,
    client_writer: Option<tokio::net::tcp::OwnedWriteHalf>,
    subdap_stream: Option<TcpStream>,
    buffered_requests: VecDeque<Request>,
    initialized: bool,
}

impl DapShim {
    fn new() -> Self {
        Self {
            seq: 0,
            client_writer: None,
            subdap_stream: None,
            buffered_requests: VecDeque::new(),
            initialized: false,
        }
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    async fn send_to_client(&mut self, msg: &Value) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(writer) = &mut self.client_writer {
            let msg_bytes = serde_json::to_vec(msg)?;
            writer
                .write_all(format!("Content-Length: {}\r\n\r\n", msg_bytes.len()).as_bytes())
                .await?;
            writer.write_all(&msg_bytes).await?;
            writer.flush().await?;
        }
        Ok(())
    }

    async fn handle_initialize(&mut self, req: Request) -> Result<(), Box<dyn std::error::Error>> {
        // Send initialize response
        let capabilities = Capabilities {
            supports_configuration_done_request: Some(true),
            supports_conditional_breakpoints: Some(true),
            supports_delayed_stack_trace_loading: Some(true),
            supports_function_breakpoints: Some(true),
            supports_instruction_breakpoints: Some(true),
            supports_exception_info_request: Some(true),
            supports_set_variable: Some(true),
            supports_evaluate_for_hovers: Some(true),
            supports_clipboard_context: Some(true),
            supports_stepping_granularity: Some(true),
            supports_log_points: Some(true),
            supports_disassemble_request: Some(true),
            supports_step_back: Some(false),
            support_terminate_debuggee: Some(false),
            supports_terminate_request: Some(false),
            supports_restart_request: Some(false),
            supports_set_expression: Some(false),
            supports_loaded_sources_request: Some(false),
            supports_read_memory_request: Some(false),
            supports_cancel_request: Some(false),
            supports_hit_conditional_breakpoints: Some(false),
            exception_breakpoint_filters: Some(vec![]),
            supports_restart_frame: Some(false),
            supports_goto_targets_request: Some(false),
            supports_step_in_targets_request: Some(false),
            supports_completions_request: Some(false),
            supports_modules_request: Some(false),
            additional_module_columns: Some(vec![]),
            supported_checksum_algorithms: Some(vec![]),
            supports_exception_options: Some(false),
            supports_value_formatting_options: Some(false),
            supports_terminate_threads_request: Some(false),
            supports_data_breakpoints: Some(false),
        };

        let response = Response {
            seq: self.next_seq(),
            type_: "response".to_string(),
            request_seq: req.seq,
            success: true,
            command: req.command.clone(),
            message: None,
            body: Some(serde_json::to_value(capabilities).unwrap()),
        };

        self.send_to_client(&serde_json::to_value(response)?).await?;

        // Send initialized event
        let initialized_event = serde_json::json!({
            "seq": self.next_seq(),
            "type": "event",
            "event": "initialized"
        });
        self.send_to_client(&initialized_event).await?;

        // Send runInTerminal request
        let run_in_terminal = serde_json::json!({
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
        self.send_to_client(&run_in_terminal).await?;

        self.initialized = true;

        // Start connecting to subdap
        tokio::spawn(async move {
            // Give the subprocess time to start
            sleep(Duration::from_secs(2)).await;
            // Connection will be established in connect_to_subdap
        });

        Ok(())
    }

    async fn connect_to_subdap(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Try to connect to the subdap server
        // You may need to adjust the port based on your subdap configuration
        for _ in 0..10 {
            match TcpStream::connect("127.0.0.1:4712").await {
                Ok(stream) => {
                    self.subdap_stream = Some(stream);
                    println!("Connected to subdap");
                    
                    // Forward buffered requests
                    while let Some(req) = self.buffered_requests.pop_front() {
                        self.forward_to_subdap(&req).await?;
                    }
                    
                    return Ok(());
                }
                Err(_) => {
                    sleep(Duration::from_millis(500)).await;
                }
            }
        }
        Err("Failed to connect to subdap".into())
    }

    async fn forward_to_subdap(&mut self, req: &Request) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(stream) = &mut self.subdap_stream {
            let req_bytes = serde_json::to_vec(req)?;
            stream
                .write_all(format!("Content-Length: {}\r\n\r\n", req_bytes.len()).as_bytes())
                .await?;
            stream.write_all(&req_bytes).await?;
            stream.flush().await?;
        }
        Ok(())
    }

    async fn handle_request(&mut self, req: Request) -> Result<(), Box<dyn std::error::Error>> {
        match req.command.as_str() {
            "initialize" => {
                self.handle_initialize(req).await?;
            }
            _ => {
                // Buffer or forward the request
                if self.subdap_stream.is_none() {
                    self.buffered_requests.push_back(req);
                    // Try to connect if we haven't already
                    if self.initialized {
                        let _ = self.connect_to_subdap().await;
                    }
                } else {
                    self.forward_to_subdap(&req).await?;
                }
            }
        }
        Ok(())
    }
}

async fn proxy_subdap_to_client(
    shim: Arc<Mutex<DapShim>>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        sleep(Duration::from_millis(100)).await;
        
        let mut shim = shim.lock().await;
        if let Some(stream) = &mut shim.subdap_stream {
            let mut buf = vec![0u8; 1024];
            match stream.try_read(&mut buf) {
                Ok(0) => continue,
                Ok(n) => {
                    // Parse and forward the response
                    // This is simplified - you'd need proper DAP message parsing
                    if let Ok(msg) = serde_json::from_slice::<Value>(&buf[..n]) {
                        shim.send_to_client(&msg).await?;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    shim: Arc<Mutex<DapShim>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut headers = String::new();

    // Store the writer in the shim
    {
        let mut shim_lock = shim.lock().await;
        shim_lock.client_writer = Some(writer);
    }

    // Start subdap proxy task
    let shim_clone = shim.clone();
    tokio::spawn(async move {
        if let Err(e) = proxy_subdap_to_client(shim_clone).await {
            eprintln!("Subdap proxy error: {}", e);
        }
    });

    loop {
        headers.clear();

        // Read headers
        loop {
            let bytes_read = reader.read_line(&mut headers).await?;
            if bytes_read == 0 {
                return Ok(()); // Connection closed
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
        let req: Request = serde_json::from_slice(&body)?;

        // Handle request
        let mut shim_lock = shim.lock().await;
        shim_lock.handle_request(req).await?;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:4711").await?;
    println!("DAP shim listening on 127.0.0.1:4711");

    loop {
        let (stream, _) = listener.accept().await?;
        let shim = Arc::new(Mutex::new(DapShim::new()));

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, shim).await {
                eprintln!("Connection error: {}", e);
            }
        });
    }
}