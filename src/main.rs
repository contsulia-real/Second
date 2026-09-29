use std::env;
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use second::{NodeId, client_ping, serve_ping_session};

const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();

    match args.as_slice() {
        [command, address, node_id] if command == "serve-once" => {
            serve_once(address, parse_u64("node id", node_id)?)
        }
        [command, address, node_id, nonce] if command == "ping" => ping(
            address,
            parse_u64("node id", node_id)?,
            parse_u64("nonce", nonce)?,
        ),
        _ => Err(
            "usage: second serve-once <listen-address> <node-id-u64> | second ping <address> <node-id-u64> <nonce>"
                .to_owned(),
        ),
    }
}

fn serve_once(address: &str, node_id: u64) -> Result<(), String> {
    let listener =
        TcpListener::bind(address).map_err(|error| format!("failed to bind {address}: {error}"))?;
    let local_address = listener
        .local_addr()
        .map_err(|error| format!("failed to read listening address: {error}"))?;

    println!("LISTENING {local_address}");
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush listening address: {error}"))?;

    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("failed to accept peer: {error}"))?;
    configure_stream(&stream)?;

    let peer = serve_ping_session(&mut stream, NodeId::from_u64(node_id))
        .map_err(|error| format!("network session failed: {error:?}"))?;

    println!("PEER {peer}");
    Ok(())
}

fn ping(address: &str, node_id: u64, nonce: u64) -> Result<(), String> {
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect {address}: {error}"))?;
    configure_stream(&stream)?;

    let peer = client_ping(&mut stream, NodeId::from_u64(node_id), nonce)
        .map_err(|error| format!("network ping failed: {error:?}"))?;

    println!("PONG peer={peer} nonce={nonce}");
    Ok(())
}

fn configure_stream(stream: &TcpStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("failed to set write timeout: {error}"))?;
    Ok(())
}

fn parse_u64(label: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|error| format!("invalid {label} {value:?}: {error}"))
}
