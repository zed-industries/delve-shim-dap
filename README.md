# delve-shim-dap

A Debug Adapter Protocol (DAP) shim specifically designed for Delve integration in Zed.

## Overview

This DAP shim acts as a middleman between a DAP client and Delve's DAP server. It handles the initialization handshake and spawns Delve via `runInTerminal`, then proxies all subsequent communication between the client and Delve.

## Features

- Responds to DAP `initialize` requests with appropriate capabilities
- Spawns Delve debugger via `runInTerminal` request
- Buffers requests before/during Delve startup
- Proxies bidirectional communication between client and Delve
- Communicates with client via stdin/stdout
- Connects to Delve via TCP

## Compatibility

**This shim is specifically designed for Delve and Zed integration.** While it attempts to adhere to the Debug Adapter Protocol specification, we make no guarantees about its compatibility with other debuggers or editors.

## Building

```bash
cargo build --release
```

## Usage

The shim communicates via stdin/stdout and spawns Delve via arguments passed to it via command line. argv[1] is path to the Delve and arg[2..] are passed as arguments.
