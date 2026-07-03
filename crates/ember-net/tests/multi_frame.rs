//! Regression tests: several S101 frames arriving in a single TCP read must all
//! be delivered, not just the first (the read loop used to drop the surplus).

use ember_proto::glow::{self, Root};
use ember_proto::s101;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use ember_net::Connection;

/// Two distinct Glow payloads, framed, concatenated into one buffer so a single
/// socket write (and near-certainly a single peer read) carries both frames.
fn two_frames() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let p1 = glow::encode_root(&Root::get_directory_at(&[1])).unwrap();
    let p2 = glow::encode_root(&Root::get_directory_at(&[2])).unwrap();
    let mut wire = s101::encode_ember(&p1);
    wire.extend_from_slice(&s101::encode_ember(&p2));
    (wire, p1, p2)
}

#[tokio::test]
async fn next_root_delivers_all_frames_from_one_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (wire, _, _) = two_frames();

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(&wire).await.unwrap();
        sock.flush().await.unwrap();
        // Keep the socket open long enough for both reads, then close (EOF).
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    let mut conn = Connection::connect(addr).await.unwrap();
    let mut docs = 0;
    while let Ok(Ok(Some(_root))) =
        tokio::time::timeout(std::time::Duration::from_secs(2), conn.next_root()).await
    {
        docs += 1;
        if docs == 2 {
            break;
        }
    }
    assert_eq!(docs, 2, "second frame from the same read was lost");
    server.await.unwrap();
}

#[tokio::test]
async fn recv_delivers_all_frames_from_one_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (wire, p1, p2) = two_frames();

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(&wire).await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    let conn = Connection::connect(addr).await.unwrap();
    let (mut reader, _writer) = conn.into_split();
    let mut raws = Vec::new();
    for _ in 0..2 {
        let inbound = tokio::time::timeout(std::time::Duration::from_secs(2), reader.recv())
            .await
            .expect("timed out waiting for a frame that should already be buffered")
            .unwrap()
            .expect("connection closed before both frames arrived");
        if let ember_net::Inbound::Documents { raw, .. } = inbound {
            raws.push(raw);
        }
    }
    assert_eq!(raws, vec![p1, p2], "payloads lost or reordered");
    server.await.unwrap();
}

/// A keep-alive request packed in the same read as documents must still be
/// answered/delivered without losing the documents around it.
#[tokio::test]
async fn keepalive_between_frames_does_not_lose_documents() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let p1 = glow::encode_root(&Root::get_directory_at(&[1])).unwrap();
    let p2 = glow::encode_root(&Root::get_directory_at(&[2])).unwrap();
    let mut wire = s101::encode_ember(&p1);
    wire.extend_from_slice(&s101::encode_keepalive_request());
    wire.extend_from_slice(&s101::encode_ember(&p2));

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(&wire).await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    let conn = Connection::connect(addr).await.unwrap();
    let (mut reader, _writer) = conn.into_split();
    let mut docs = 0;
    let mut keepalives = 0;
    for _ in 0..3 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), reader.recv())
            .await
            .expect("timed out")
            .unwrap()
            .expect("closed early")
        {
            ember_net::Inbound::Documents { .. } => docs += 1,
            ember_net::Inbound::KeepAliveRequest => keepalives += 1,
        }
    }
    assert_eq!((docs, keepalives), (2, 1));
    server.await.unwrap();
}
