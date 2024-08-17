# Source Cooperative Nexus
This project implements an S3-compatible API that acts as a proxy for various object store backends, including Amazon S3 and Azure Blob Storage. It dynamically routes requests to the appropriate backend based on the Source Cooperative repository being accessed. This allows clients to interact with different storage systems using a consistent S3-like interface, simplifying access to diverse data sources within the Source Cooperative ecosystem.

## Features

- Get object content from repositories
- Head object to retrieve metadata
- List objects in repositories
- Support for S3 and Azure Blob Storage backends
- CORS support
- Streaming responses for large objects

## Prerequisites

- Rust (latest stable version)
- Cargo (comes with Rust)

## Installation

1. Clone the repository:
   ```
   git clone https://github.com/yourusername/source-data-api.git
   cd source-data-api
   ```

2. Build the project:
   ```
   cargo build --release
   ```

## Configuration

- TODO: Add configuration instructions

## Running the Server

To start the server, run:

```
cargo run --release
```

The server will start on `0.0.0.0:8080` by default.

## API Endpoints

- `GET /{account_id}/{repository_id}/{key}`: Retrieve an object
- `HEAD /{account_id}/{repository_id}/{key}`: Get object metadata
- `GET /{account_id}?prefix={repository_id}/{prefix}&list-type=2`: List objects in a repository

## Project Structure

- `src/main.rs`: Entry point and API route definitions
- `src/clients/`: Backend-specific implementations (S3, Azure)
- `src/utils/`: Utility functions and shared code

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the [MIT License](LICENSE).
