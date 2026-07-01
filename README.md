# mcp-proxy

License: MIT

---

## IMPORTANT WARNING

> [!WARNING]
> ### AI-only migration
> This project was fully migrated from Python to Rust by AI only.
> It is in an experimental stage and is strongly not recommended for production use.

---

- [mcp-proxy](#mcp-proxy)
  - [About](#about)
  - [1. stdio to Streamable HTTP](#1-stdio-to-streamable-http)
    - [1.1 Configuration](#11-configuration)
    - [1.2 Example usage](#12-example-usage)
  - [2. Command to Streamable HTTP server](#2-command-to-streamable-http-server)
    - [2.1 Configuration](#21-configuration)
    - [2.2 Example usage](#22-example-usage)
  - [Named Servers](#named-servers)
  - [Installation](#installation)
    - [Installing via Cargo](#installing-via-cargo)
    - [Running in a container (local build)](#running-in-a-container-local-build)
  - [Docker Compose Setup](#docker-compose-setup)
  - [Command line arguments](#command-line-arguments)
    - [Example config file](#example-config-file)
  - [Testing](#testing)

## About

The `mcp-proxy` is a tool that lets you switch between MCP server transports. There are two supported modes:

1. **stdio to Streamable HTTP** — client mode: connect to a remote Streamable HTTP MCP server and expose it locally over stdio
2. **Command to Streamable HTTP server** — server mode: spawn a local stdio MCP server and expose it over Streamable HTTP

> **Note:** This is a Rust implementation and only Streamable HTTP (`/mcp`) is implemented.

## 1. stdio to Streamable HTTP

Run a proxy that connects to a remote Streamable HTTP MCP server and exposes it locally over stdio.

This mode allows clients like Claude Desktop to communicate with a remote MCP server even though they only support stdio.

```mermaid
graph LR
    A["Claude Desktop"] <--> |stdio| B["mcp-proxy"]
    B <--> |Streamable HTTP| C["External MCP Server"]

    style A fill:#ffe6f9,stroke:#333,color:black,stroke-width:2px
    style B fill:#e6e6ff,stroke:#333,color:black,stroke-width:2px
    style C fill:#e6ffe6,stroke:#333,color:black,stroke-width:2px
```

### 1.1 Configuration

Provide the URL of the remote MCP server's Streamable HTTP endpoint as the first argument.

Arguments

| Name                     | Required | Description                                                               | Example                                       |
| ------------------------ | -------- | ------------------------------------------------------------------------- | --------------------------------------------- |
| `command_or_url`         | Yes      | The remote MCP server endpoint to connect to                              | http://example.io/mcp                         |
| `--headers`              | No       | Headers to send to the remote server. Can be used multiple times.         | Authorization 'Bearer my-secret-access-token' |
| `--transport`            | No       | The transport to use for the client. Default is `streamablehttp`.         | streamablehttp                                |
| `--verify-ssl`           | No       | Control SSL verification. Omit for default, `false` to disable, or a PEM bundle path. | /etc/ssl/ca-bundle.pem     |
| `--no-verify-ssl`        | No       | Disable SSL verification (alias for `--verify-ssl false`)                 |                                               |
| `--client-id`            | No       | OAuth2 client ID for authentication                                       | your_client_id                                |
| `--client-secret`        | No       | OAuth2 client secret for authentication                                   | your_client_secret                            |
| `--token-url`            | No       | OAuth2 token endpoint URL for authentication                              | https://auth.example.com/oauth/token          |
| `--upstream-pool-size`   | No       | Number of independent upstream HTTP connections for round-robin distribution. Increases request concurrency. Default is `4`. | 8 |
| `--upstream-timeout-secs`| No       | Timeout in seconds for a single forwarded upstream request. Default is `30`. | 60                                         |
| `--log-level`            | No       | Set the log level. Default is `INFO`.                                     | DEBUG                                         |
| `--debug`                | No       | Enable debug mode (equivalent to `--log-level DEBUG`).                    |                                               |

Environment Variables

| Name               | Required | Description                                                                  | Example    |
| ------------------ | -------- | ---------------------------------------------------------------------------- | ---------- |
| `MCP_URL`          | No       | Alternative to passing the URL as a positional argument                      | http://example.io/mcp |
| `API_ACCESS_TOKEN` | No       | Can be used instead of `--headers Authorization 'Bearer <API_ACCESS_TOKEN>'` | YOUR_TOKEN |

### 1.2 Example usage

`mcp-proxy` is meant to be started by the MCP client, so the configuration must be done accordingly.

For Claude Desktop, the configuration entry can look like this:

```json
{
  "mcpServers": {
    "mcp-proxy": {
      "command": "mcp-proxy",
      "args": [
        "http://example.io/mcp"
      ],
      "env": {
        "API_ACCESS_TOKEN": "access-token"
      }
    }
  }
}
```

## 2. Command to Streamable HTTP server

Run a proxy that spawns a local stdio MCP server and exposes it over Streamable HTTP.

This allows remote clients to reach a local stdio server. `mcp-proxy` listens on a port, accepts Streamable HTTP connections, and forwards them to the spawned process over stdio.

```mermaid
graph LR
    A["LLM Client"] <-->|Streamable HTTP| B["mcp-proxy"]
    B <-->|stdio| C["Local MCP Server"]

    style A fill:#ffe6f9,stroke:#333,color:black,stroke-width:2px
    style B fill:#e6e6ff,stroke:#333,color:black,stroke-width:2px
    style C fill:#e6ffe6,stroke:#333,color:black,stroke-width:2px
```

### 2.1 Configuration

Arguments

| Name                                 | Required                   | Description                                                                                   | Example                                     |
| ------------------------------------ | -------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------- |
| `command_or_url`                     | Yes                        | The command to spawn the MCP stdio server                                                     | uvx mcp-server-fetch                        |
| `--port`                             | No, random available       | The port to listen on                                                                         | 8080                                        |
| `--host`                             | No, `127.0.0.1` by default | The host IP address to listen on                                                              | 0.0.0.0                                     |
| `--env`                              | No                         | Additional environment variables to pass to the stdio server. Can be used multiple times.     | FOO BAR                                     |
| `--cwd`                              | No                         | The working directory for the spawned stdio server process.                                   | /tmp                                        |
| `--pass-environment`                 | No                         | Pass through all environment variables when spawning the server                               |                                             |
| `--allow-origin`                     | No                         | Allowed origins for CORS. Can be used multiple times. Default is no CORS allowed.            | --allow-origin "\*"                         |
| `--expose-header`                    | No                         | Headers added to `Access-Control-Expose-Headers`. Can be used multiple times. Defaults to `mcp-session-id`. | --expose-header Custom-Header |
| `--stateless`                        | No                         | Enable stateless mode for the Streamable HTTP transport. Default is False.                    |                                             |
| `--named-server NAME COMMAND_STRING` | No                         | Defines a named stdio server.                                                                 | --named-server fetch 'uvx mcp-server-fetch' |
| `--named-server-config FILE_PATH`    | No                         | Path to a JSON file defining named stdio servers.                                             | --named-server-config /path/to/servers.json |
| `--log-level`                        | No                         | Set the log level. Default is `INFO`. Possible values: `DEBUG`, `INFO`, `WARNING`, `ERROR`, `CRITICAL`. | DEBUG            |
| `--debug`                            | No                         | Enable debug mode with detailed logging output. Equivalent to `--log-level DEBUG`.            |                                             |

### 2.2 Example usage

```bash
# Start the MCP server behind the proxy (random port)
mcp-proxy uvx mcp-server-fetch

# Start the MCP server behind the proxy with a custom port
mcp-proxy --port=8080 uvx mcp-server-fetch

# Start the MCP server behind the proxy with a custom host and port
mcp-proxy --host=0.0.0.0 --port=8080 uvx mcp-server-fetch

# Pass extra arguments to the spawned command using -- separator
mcp-proxy --port=8080 -- uvx mcp-server-fetch --user-agent=YourUserAgent

# Start multiple named MCP servers behind the proxy
mcp-proxy --port=8080 --named-server fetch 'uvx mcp-server-fetch' --named-server fetch2 'uvx mcp-server-fetch'

# Start multiple named MCP servers using a configuration file
mcp-proxy --port=8080 --named-server-config ./servers.json

# Start the MCP server with CORS enabled and custom exposed headers
mcp-proxy --port=8080 --allow-origin='*' --expose-header Custom-Header uvx mcp-server-fetch
```

## Named Servers

- `NAME` is used in the URL path `/servers/NAME/mcp`.
- `COMMAND_STRING` is the command to start the server (e.g., `'uvx mcp-server-fetch'`).
  - Can be used multiple times.
  - Ignored if `--named-server-config` is used.
- `FILE_PATH` — if provided, this is the exclusive source for named servers; `--named-server` CLI arguments are ignored.

If a default server is specified (the `command_or_url` argument without `--named-server` or `--named-server-config`), it will be accessible at:

```
http://127.0.0.1:<port>/mcp
```

Named servers will be accessible at:

```
http://127.0.0.1:<port>/servers/<server-name>/mcp
```

**JSON Configuration File Format for `--named-server-config`:**

```json
{
  "mcpServers": {
    "fetch": {
      "enabled": true,
      "command": "uvx",
      "args": [
        "mcp-server-fetch"
      ],
      "transportType": "stdio"
    },
    "github": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-github"
      ],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_TOKEN>"
      },
      "cwd": "/opt/my-project",
      "transportType": "stdio"
    }
  }
}
```

- `mcpServers`: dictionary where each key is the server name (used in the URL path, e.g., `/servers/fetch/mcp`)
- `command`: (Required) The command to execute for the stdio server
- `args`: (Optional) A list of arguments for the command. Defaults to an empty list
- `env`: (Optional) Additional environment variables to pass to the server process
- `cwd`: (Optional) Working directory for the spawned server process
- `enabled`: (Optional) If `false`, this server definition will be skipped. Defaults to `true`
- `timeout` and `transportType`: Present in standard MCP client configs but **ignored** by `mcp-proxy`. The transport type is always stdio.

## Installation

### Installing via Cargo

```bash
cargo install --path .
```

Or build from source:

```bash
git clone <your-fork-url>
cd mcp-proxy
cargo build --release
# Binary is at ./target/release/mcp-proxy
```

### Running in a container (local build)

```bash
docker build -t mcp-proxy:local -f Dockerfile .
docker run --rm -t mcp-proxy:local --help
```

## Docker Compose Setup

```yaml
services:
  filesystem-proxy:
    build:
      context: .
      dockerfile: Dockerfile.demo
    ports:
      - "9090:8080"
    volumes:
      - ./src:/workspace/src:ro
    command:
      - "--host=0.0.0.0"
      - "--port=8080"
      - "--"
      - "mcp-server-filesystem"
      - "/workspace/src"

  filesystem-stdio-proxy:
    build:
      context: .
      dockerfile: Dockerfile
    depends_on:
      - filesystem-proxy
    stdin_open: true
    tty: true
    command:
      - "http://filesystem-proxy:8080/mcp"
```

Run it with:

```bash
docker-compose up --build
```

## Command line arguments

```
Start the MCP proxy in one of two possible modes: as a client or a server.

Usage: mcp-proxy [OPTIONS] [COMMAND_OR_URL] [ARGS]...

Arguments:
  [COMMAND_OR_URL]  Command or URL to connect to. When a URL, will run a StreamableHTTP
                    client. Otherwise, if --named-server is not used, this will be the
                    command for the default stdio client [env: MCP_URL=]
  [ARGS]...         Any extra arguments to the command to spawn the default server

Options:
  -H, --headers <KEY> <VALUE>
          Headers to pass to the remote server. Can be used multiple times
      --transport <TRANSPORT>
          The transport to use for the client. Default is streamablehttp
          [default: streamablehttp] [possible values: streamablehttp]
      --client-id <CLIENT_ID>
          OAuth2 client ID for authentication
      --client-secret <CLIENT_SECRET>
          OAuth2 client secret for authentication
      --token-url <TOKEN_URL>
          OAuth2 token URL for authentication
      --verify-ssl [<VALUE>]
          Control SSL verification when acting as a client. Use without a value to force
          verification, pass 'false' to disable, or provide a path to a PEM bundle
      --no-verify-ssl
          Disable SSL verification (alias for --verify-ssl false)
  -e, --env <KEY> <VALUE>
          Environment variables used when spawning the default server. Can be used
          multiple times
      --cwd <CWD>
          The working directory to use when spawning the default server process
      --pass-environment
          Pass through all environment variables when spawning all server processes
      --log-level <LOG_LEVEL>
          Set the log level. Default is INFO
          [default: INFO] [possible values: DEBUG, INFO, WARNING, ERROR, CRITICAL]
      --debug
          Enable debug mode with detailed logging output. Equivalent to --log-level DEBUG
      --named-server <NAME> <COMMAND_STRING>
          Define a named stdio server. Can be used multiple times
      --named-server-config <NAMED_SERVER_CONFIG>
          Path to a JSON configuration file for named stdio servers
      --port <PORT>
          Port to expose the server on. Default is a random port [default: 0]
      --host <HOST>
          Host to expose the server on. Default is 127.0.0.1 [default: 127.0.0.1]
      --stateless
          Enable stateless mode for the Streamable HTTP transport. Default is False
      --allow-origin <ALLOW_ORIGIN>...
          Allowed origins for the server. Can be used multiple times. Default is no CORS
      --expose-header <EXPOSE_HEADERS>
          Headers to expose via Access-Control-Expose-Headers. Defaults to 'mcp-session-id'
      --upstream-pool-size <UPSTREAM_POOL_SIZE>
          Number of independent upstream HTTP connections to open in client mode. Requests
          are round-robin distributed across the pool. [default: 4]
      --upstream-timeout-secs <UPSTREAM_TIMEOUT_SECS>
          Timeout in seconds for a single forwarded upstream request in client mode.
          [default: 30]
  -h, --help
          Print help

Examples:
  mcp-proxy http://localhost:8080/mcp
  mcp-proxy --no-verify-ssl https://server.local/mcp
  mcp-proxy --headers Authorization 'Bearer YOUR_TOKEN' http://localhost:8080/mcp
  mcp-proxy --port 8080 -- your-command --arg1 value1 --arg2 value2
  mcp-proxy --named-server fetch 'uvx mcp-server-fetch' --port 8080
  mcp-proxy your-command --port 8080 -e KEY VALUE -e ANOTHER_KEY ANOTHER_VALUE
  mcp-proxy your-command --port 8080 --allow-origin='*'
```

### Example config file

```json
{
  "mcpServers": {
    "fetch": {
      "enabled": true,
      "command": "uvx",
      "args": [
        "mcp-server-fetch"
      ],
      "transportType": "stdio"
    },
    "github": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-github"
      ],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_TOKEN>"
      },
      "transportType": "stdio"
    }
  }
}
```

## Testing

Start a local server behind the proxy and connect to it with another `mcp-proxy` instance:

```bash
# Start the stdio server (mcp-server-fetch) exposed over Streamable HTTP on port 8080
mcp-proxy --port=8080 uvx mcp-server-fetch &

# Connect to the Streamable HTTP proxy from a second mcp-proxy over stdio
mcp-proxy http://127.0.0.1:8080/mcp

# Send CTRL+C to stop the second proxy, then bring the first to foreground and stop it
fg
```

You can also use the [MCP Inspector](https://modelcontextprotocol.io/docs/tools/inspector) to test the server directly at `http://127.0.0.1:8080/mcp`.
