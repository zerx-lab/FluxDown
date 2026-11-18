use std::{net::Ipv4Addr, path::Path};

use fluxdown_engine::bt_task_files::BtTaskOpError;
use fluxdown_engine::download_manager::NewTaskSpec;
use librqbit::{CreateTorrentOptions, create_torrent, spawn_utils::BlockingSpawner};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::swarm::{Downloader, PIECE_LEN, Tracker};

type MirrorFiles = &'static [(&'static str, &'static [u8])];

async fn serve_ranges(files: MirrorFiles) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            let path = line.split_ascii_whitespace().nth(1).unwrap();
            let data = files
                .iter()
                .find_map(|(name, data)| (*name == path).then_some(*data))
                .expect("request path matches the published mirror layout");
            let mut range = None;
            loop {
                line.clear();
                if socket.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("range")
                {
                    let (first, last) = value
                        .trim()
                        .strip_prefix("bytes=")
                        .unwrap()
                        .split_once('-')
                        .unwrap();
                    range = Some((
                        first.parse::<usize>().unwrap(),
                        last.parse::<usize>().unwrap(),
                    ));
                }
            }
            let (first, last) = range.expect("BEP 19 range request");
            let body = data.get(first..=last).expect("range within mirror file");
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {first}-{last}/{}\r\nConnection: close\r\n\r\n",
                body.len(),
                data.len(),
            );
            socket.get_mut().write_all(head.as_bytes()).await.unwrap();
            socket.get_mut().write_all(body).await.unwrap();
        }
    });
    (base, server)
}

async fn add_torrent(leech: &mut Downloader, path: &Path, start_paused: bool) -> String {
    let torrent = create_torrent(
        path,
        CreateTorrentOptions {
            piece_length: Some(PIECE_LEN),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    leech
        .engine
        .manager
        .create_task(NewTaskSpec {
            save_dir: leech.work.to_string_lossy().into_owned(),
            torrent_file_bytes: torrent.as_bytes().unwrap().to_vec(),
            unattended_selection: true,
            start_paused,
            ..Default::default()
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_a_direct_link_supplies_a_running_torrent_without_peers() {
    const BODY: &[u8] = b"content from a differently named HTTP file";
    let tracker = Tracker::start().await;
    let mut leech = Downloader::start(&tracker).await;
    let input = leech.work.join("fixture");
    tokio::fs::create_dir(&input).await.unwrap();
    let file = input.join("payload.bin");
    tokio::fs::write(&file, BODY).await.unwrap();
    let id = add_torrent(&mut leech, &file, false).await;
    leech.wait_for_status(&id, 1).await;

    assert!(matches!(
        leech
            .engine
            .manager
            .add_bt_web_seed(&id, "ftp://example.com/payload")
            .await,
        Err(BtTaskOpError::InvalidArgument(_))
    ));
    let (base, server) = serve_ranges(&[("/download?id=7", BODY)]).await;
    let url = format!("{base}/download?id=7");
    leech
        .engine
        .manager
        .add_bt_web_seed(&id, &url)
        .await
        .unwrap();
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "{:?}", task.error_message);
    assert_eq!(
        leech.landed(&task, "payload.bin").await.as_deref(),
        Some(BODY)
    );
    server.abort();
    leech.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_torrents_mirror_survives_restart_and_uses_the_published_paths() {
    const FIRST: &[u8] = b"first file";
    const SECOND: &[u8] = b"second file in a subdirectory";
    let tracker = Tracker::start().await;
    let mut leech = Downloader::start(&tracker).await;
    let input = leech.work.join("fixture/Pack");
    tokio::fs::create_dir_all(input.join("sub")).await.unwrap();
    tokio::fs::write(input.join("a.bin"), FIRST).await.unwrap();
    tokio::fs::write(input.join("sub/b.bin"), SECOND)
        .await
        .unwrap();
    let id = add_torrent(&mut leech, &input, true).await;
    let (base, server) =
        serve_ranges(&[("/pub/Pack/a.bin", FIRST), ("/pub/Pack/sub/b.bin", SECOND)]).await;
    let url = format!("{base}/pub/");
    for value in [format!(" {url} "), url.clone()] {
        leech
            .engine
            .manager
            .add_bt_web_seed(&id, &value)
            .await
            .unwrap();
    }

    let mut leech = leech.restart(&tracker).await;
    assert_eq!(leech.task(&id).await.status, 2);
    let status = leech.engine.manager.bt_status(&id).await.unwrap();
    assert!(matches!(status.web_seeds.as_slice(), [seed] if seed.url == url));
    leech.engine.manager.resume_task(&id).await;
    let task = leech.wait_done(&id).await;
    assert_eq!(task.status, 3, "{:?}", task.error_message);
    assert_eq!(leech.landed(&task, "a.bin").await.as_deref(), Some(FIRST));
    assert_eq!(
        leech.landed(&task, "sub/b.bin").await.as_deref(),
        Some(SECOND)
    );
    server.abort();
    leech.shutdown().await;
}
