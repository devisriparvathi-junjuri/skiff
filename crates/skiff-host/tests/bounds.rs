//! Review M4 P2: a client that stops reading once made the service compute
//! every request it sent and hold each reply in a blocked sender (200 sent,
//! 154 replies held, against an advertised 16). Now a request stays admitted
//! until its reply is written, so such a client stalls at the bound, and its
//! work ends with its connection. Its own file: `admitted()` counts the
//! whole process.
#![cfg(unix)]

use std::{os::fd::AsRawFd, sync::Arc, time::Duration};

use skiff_host_api::{Capability, Envelope, Hello, Host, HostId, InstanceId, Limits, Message, Request, API_MAJOR};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

fn hello() -> Hello {
    Hello {
        host: Host { host_id: HostId::try_from("bounds".to_string()).unwrap(), os: "test".into(), account: "uid:0".into() },
        instance_id: InstanceId::try_from("run".to_string()).unwrap(),
        service_versions: Default::default(),
        api_major: API_MAJOR,
        capabilities: vec![Capability::projects()],
        limits: Limits { max_frame_bytes: skiff_host::MAX_FRAME, max_in_flight: skiff_host::MAX_IN_FLIGHT },
    }
}

/// Small kernel buffers, so the replies cannot hide in them.
fn small_buffers(s: &impl AsRawFd) {
    let size: libc::c_int = 2048;
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: a valid socket fd and an int option value.
        unsafe {
            libc::setsockopt(s.as_raw_fd(), libc::SOL_SOCKET, opt, (&size as *const libc::c_int).cast(), std::mem::size_of_val(&size) as libc::socklen_t);
        }
    }
}

fn frames(n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for id in 1..=n {
        out.extend(serde_json::to_vec(&Envelope { id, request: Request::Hello }).unwrap());
        out.push(b'\n');
    }
    out
}

async fn settle(want: impl Fn(usize) -> bool) -> usize {
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let n = skiff_host::admitted();
        if want(n) || tokio::time::Instant::now() > until {
            return n;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_stalls_at_the_bound_and_its_work_ends_with_it() {
    let (service, client) = UnixStream::pair().unwrap();
    small_buffers(&service);
    small_buffers(&client);
    let served = tokio::spawn(skiff_host::handle(service, Arc::new(hello())));
    let (rd, mut wr) = client.into_split();
    // 200 requests, and no reply is ever read.
    let sending = tokio::spawn(async move {
        let _ = wr.write_all(&frames(200)).await;
        wr
    });

    // It fills up to the bound and stays there: reading more stops.
    let full = settle(|n| n == skiff_host::MAX_IN_FLIGHT).await;
    assert_eq!(full, skiff_host::MAX_IN_FLIGHT, "the stall reaches the bound");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(skiff_host::admitted(), skiff_host::MAX_IN_FLIGHT, "and never passes it");
    assert!(!served.is_finished());

    // The client goes away: its connection and every admitted request end.
    sending.abort();
    let _ = sending.await;
    drop(rd);
    tokio::time::timeout(Duration::from_secs(10), served).await.expect("the connection ends").unwrap();
    assert_eq!(settle(|n| n == 0).await, 0, "nothing is left behind");

    // A client that reads gets all 200 replies, each once, through the same bound.
    let (service, client) = UnixStream::pair().unwrap();
    small_buffers(&service);
    small_buffers(&client);
    let served = tokio::spawn(skiff_host::handle(service, Arc::new(hello())));
    let (rd, mut wr) = client.into_split();
    let sending = tokio::spawn(async move { wr.write_all(&frames(200)).await.unwrap() });
    let mut ids = Vec::new();
    let mut lines = BufReader::new(rd).lines();
    while ids.len() < 200 {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await.unwrap().unwrap().unwrap();
        let Message::Reply { id, result } = serde_json::from_str(&line).unwrap() else { panic!("{line}") };
        assert!(result.is_ok());
        assert!(skiff_host::admitted() <= skiff_host::MAX_IN_FLIGHT);
        ids.push(id);
    }
    ids.sort();
    assert_eq!(ids, (1..=200).collect::<Vec<_>>());
    sending.await.unwrap();
    drop(lines);
    tokio::time::timeout(Duration::from_secs(10), served).await.unwrap().unwrap();
    assert_eq!(settle(|n| n == 0).await, 0);
}
