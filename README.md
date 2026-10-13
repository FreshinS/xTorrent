# XTorrent

XTorrent (temporary name) is an open-source BitTorrent client built in Rust without using BitTorrent libs.

***This is a pet project not intended for widespread use, created for the purpose of learning the Rust language. Use at your own risk.***

# Usage

At current state of project you can only parse meta-data from .torrent file.

```bash
cargo run -- ./path-to-torrent-file
```

# Goals

- [X] Bencode Decoder (Done 10.10.2026)
- [X] Parse Torrent File (Done 13.10.2026)
- [ ] Tracker Client
- [ ] Peer Wire Protocol
- [ ] Piece Downloader
- [ ] File & Downloads Manager
- [ ] GUI Built in Tauri using React
- [ ] Optimiaztions, New Features and etc.
