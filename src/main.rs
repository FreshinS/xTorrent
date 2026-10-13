use std::{env, error::Error, fs, path::PathBuf, process};

use xtorrent::metainfo::{self, TorrentMeta};

fn main() {
    let file_arg = match env::args().nth(1) {
        Some(value) => value,
        None => {
            eprintln!("Error: Expected path to torrent file");
            process::exit(1);
        }
    };

    if let Err(e) = run(file_arg) {
        eprintln!("Application error: {e}");
        process::exit(1);
    }
}

fn run(file_arg: String) -> Result<(), Box<dyn Error>> {
    let file_path = PathBuf::from(file_arg);
    let bytes = fs::read(file_path)?;

    let torrent = metainfo::parse(&bytes)?;
    print_summary(&torrent);

    Ok(())
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
