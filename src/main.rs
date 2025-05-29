use debugserver_types::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug)]
struct DapServer {
    seq: i64,
}

impl DapServer {
    fn new() -> Self {
        Self { seq: 0 }
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    async fn handle_request(&mut self, req: Request) -> Response {
        match req.command.as_str() {
            "initialize" => {
                let capabilities = Capabilities {
                    supports_configuration_done_request: Some(true),
                    supports_function_breakpoints: Some(false),
                    supports_conditional_breakpoints: Some(false),
                    supports_hit_conditional_breakpoints: Some(false),
                    supports_evaluate_for_hovers: Some(false),
                    exception_breakpoint_filters: Some(vec![]),
                    supports_step_back: Some(false),
                    supports_set_variable: Some(false),
                    supports_restart_frame: Some(false),
                    supports_goto_targets_request: Some(false),
                    supports_step_in_targets_request: Some(false),
                    supports_completions_request: Some(false),
                    supports_modules_request: Some(false),
                    additional_module_columns: Some(vec![]),
                    supported_checksum_algorithms: Some(vec![]),
                    supports_restart_request: Some(false),
                    supports_exception_options: Some(false),
                    supports_value_formatting_options: Some(false),
                    supports_exception_info_request: Some(false),
                    support_terminate_debuggee: Some(false),
                    supports_delayed_stack_trace_loading: Some(false),
                    supports_loaded_sources_request: Some(false),
                    supports_log_points: Some(false),
                    supports_terminate_threads_request: Some(false),
                    supports_set_expression: Some(false),
                    supports_terminate_request: Some(true),
                    supports_data_breakpoints: Some(false),
                };
                
                Response {
                    seq: self.next_seq(),
                    type_: "response".to_string(),
                    request_seq: req.seq,
                    success: true,
                    command: req.command.clone(),
                    message: None,
                    body: Some(serde_json::to_value(capabilities).unwrap()),
                }
            }
            _ => {
                Response {
                    seq: self.next_seq(),
                    type_: "response".to_string(),
                    request_seq: req.seq,
                    success: false,
                    command: req.command.clone(),
                    message: Some(format!("Unimplemented request: {}", req.command)),
                    body: None,
                }
            }
        }
    }
}

async fn handle_connection(stream: TcpStream, server: Arc<Mutex<DapServer>>) -> Result<(), Box<dyn std::error::Error>> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut headers = String::new();
    
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
        let mut server = server.lock().await;
        let response = server.handle_request(req).await;
        
        // Serialize response
        let response_json = serde_json::to_vec(&response)?;
        
        // Send response
        writer.write_all(format!("Content-Length: {}\r\n\r\n", response_json.len()).as_bytes()).await?;
        writer.write_all(&response_json).await?;
        writer.flush().await?;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:4711").await?;
    println!("DAP server listening on 127.0.0.1:4711");
    
    let server = Arc::new(Mutex::new(DapServer::new()));
    
    loop {
        let (stream, _) = listener.accept().await?;
        let server_clone = server.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, server_clone).await {
                eprintln!("Connection error: {}", e);
            }
        });
    }
}