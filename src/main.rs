use std::{collections::HashSet, env, error::Error, fs, io, path::PathBuf, process};

use xtorrent::{
    torrent::{self, metainfo::TorrentMeta},
    tracker::http::{self, AnnounceEvent, AnnounceOptions, AnnounceResponse, Peer},
};

fn main() {
    let file_arg = match env::args().nth(1) {
        Some(value) => value,
        None => {
            eprintln!("Error: Expected path to torrent file");
            process::exit(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("Failed to create Tokio runtime: {e}");
            process::exit(1);
        }
    };

    if let Err(e) = runtime.block_on(run(file_arg)) {
        eprintln!("Application error: {e}");
        process::exit(1);
    }
}

async fn run(file_arg: String) -> Result<(), Box<dyn Error>> {
    let file_path = PathBuf::from(file_arg);
    let bytes = fs::read(file_path)?;

    let mut torrent = torrent::metainfo::parse(&bytes)?;
    print_summary(&torrent);

    const PEER_PORT: u16 = 6881;
    let announce_urls = collect_announce_urls(&torrent);
    if announce_urls.is_empty() {
        return Err(io::Error::other("Torrent has no announce URL").into());
    }

    let client = reqwest::Client::new();
    let peer_id = rand::random();
    let original_announce = torrent.announce.clone();
    let mut announce_result = None;
    let mut announce_errors = Vec::new();

    for announce_url in &announce_urls {
        torrent.announce = Some(announce_url.clone());

        match http::announce(
            &torrent,
            AnnounceOptions {
                client: Some(client.clone()),
                peer_id: Some(peer_id),
                port: PEER_PORT,
                uploaded: 0,
                downloaded: 0,
                left: torrent.total_size,
                event: Some(AnnounceEvent::Started),
            },
        )
        .await
        {
            Ok((response, _client, peer_id, port)) => {
                println!("\nTracker accepted announce: {announce_url}");
                announce_result = Some((response, peer_id, port));
                break;
            }
            Err(error) => {
                eprintln!("Tracker announce failed ({announce_url}): {error}");
                announce_errors.push(format!("{announce_url}: {error}"));
            }
        }
    }

    torrent.announce = original_announce;

    let Some((response, peer_id, port)) = announce_result else {
        return Err(io::Error::other(format!(
            "All tracker announces failed: {}",
            announce_errors.join("; ")
        ))
        .into());
    };

    print_announce_response(&response, &peer_id, port);

    Ok(())
}

fn collect_announce_urls(torrent: &TorrentMeta) -> Vec<String> {
    let mut announce_urls = Vec::new();
    let mut seen = HashSet::new();

    if let Some(announce) = &torrent.announce
        && seen.insert(announce.clone())
    {
        announce_urls.push(announce.clone());
    }

    for tier in &torrent.announce_list {
        for announce in tier {
            if seen.insert(announce.clone()) {
                announce_urls.push(announce.clone());
            }
        }
    }

    announce_urls
}

fn print_announce_response(response: &AnnounceResponse, peer_id: &[u8; 20], port: u16) {
    println!("\nTracker announce:");
    println!("  Peer ID: {}", format_bytes_hex(peer_id));
    println!("  Port: {port}");
    println!("  Announce interval: {} seconds", response.interval);
    println!(
        "  Seeders (complete): {}",
        format_optional_count(response.complete)
    );
    println!(
        "  Leechers (incomplete): {}",
        format_optional_count(response.incomplete)
    );
    println!("  Peers: {}", response.peers.len());

    for peer in &response.peers {
        print_peer(peer);
    }
}

fn format_optional_count(count: Option<u64>) -> String {
    count.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn format_bytes_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(*byte >> 4) as usize] as char);
        result.push(HEX[(*byte & 0x0f) as usize] as char);
    }
    result
}

fn print_peer(peer: &Peer) {
    print!("    {}:{}", peer.host, peer.port);
    if let Some(peer_id) = &peer.peer_id {
        print!(" (peer ID: {})", format_bytes_hex(peer_id));
    }
    println!();
}

fn print_summary(torrent: &TorrentMeta) {
    println!("Name: {}", torrent.info.name);

    print!("Info hash: ");
    for byte in &torrent.info_hash {
        print!("{byte:02x}");
    }
    println!();

    println!(
        "Total size: {} bytes ({:.2} MiB)",
        torrent.total_size,
        torrent.total_size as f64 / (1024.0 * 1024.0)
    );
    println!("Piece size: {} bytes", torrent.info.piece_length);
    println!("Pieces: {}", torrent.info.piece_hashes.len());
    println!("Private: {}", torrent.info.private);

    if let Some(created_by) = &torrent.created_by {
        println!("Created by: {created_by}");
    }
    if let Some(creation_date) = torrent.creation_date {
        println!("Creation date (Unix timestamp): {creation_date}");
    }
    if let Some(comment) = &torrent.comment {
        println!("Comment: {comment}");
    }
    if let Some(encoding) = &torrent.encoding {
        println!("Encoding: {encoding}");
    }

    if let Some(announce) = &torrent.announce {
        println!("Announce: {announce}");
    }
    for (index, tier) in torrent.announce_list.iter().enumerate() {
        println!("Tracker tier {}:", index + 1);
        for tracker in tier {
            println!("  {tracker}");
        }
    }

    if !torrent.nodes.is_empty() {
        println!("DHT nodes: {}", torrent.nodes.len());
        for (host, port) in &torrent.nodes {
            println!("  {host} (port {port})");
        }
    }
    if !torrent.web_seeds.is_empty() {
        println!("Web seeds:");
        for seed in &torrent.web_seeds {
            println!("  {seed}");
        }
    }
    if !torrent.http_seeds.is_empty() {
        println!("HTTP seeds:");
        for seed in &torrent.http_seeds {
            println!("  {seed}");
        }
    }

    const FILE_PREVIEW_LIMIT: usize = 10;

    println!("Files: {}", torrent.info.files.len());
    for file in torrent.info.files.iter().take(FILE_PREVIEW_LIMIT) {
        println!(
            "  {} ({} bytes, offset {})",
            file.path.join("/"),
            file.length,
            file.offset
        );
    }
    if torrent.info.files.len() > FILE_PREVIEW_LIMIT {
        println!(
            "  ... and {} more files",
            torrent.info.files.len() - FILE_PREVIEW_LIMIT
        );
    }
}
